"""family-safety outcome 70 — the guardian's add-contact and hand-over forms,
and the policy Save, offer only a submit that can succeed: each is disabled
with its own reason while its draft cannot be sent, comes alive once it can,
and a policy save the nest turns away leaves the editor holding what was typed.

Goal: ``docs/goal/behavior/family-safety.md`` § Errors & edge cases (*The
guardian's forms are painted from a draft-validity predicate*):
``family-contact-add-button`` and ``family-transfer-button`` answer
``fauna_client_family::forms::contact_add_possibility`` /
``transfer_possibility`` — blocked while the box is blank or is not a hex actor
id, each with its OWN reason (the transfer no longer reuses the contact-add
words); ``family-policy-save-button`` answers ``policy_save_possibility``,
blocked while the screen-time cells do not parse or validate through the shared
rules the nest refuses by (``fauna_core::screen_time``), the rule's own words in
the reason. The page rule is ``docs/goal/ui/README.md`` § Offer only what is
possible (rule 2: what the person can unblock is disabled with its reason within
eyeshot; rule 4's form case: the submit is painted from the draft-validity
predicate and asks the same one).

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet,
when its reason is not within eyeshot, or when a control this person can never
use is shown. The predicates' machine tests (``libs/fauna-client-family/src/forms.rs``,
``libs/fauna-ffi/src/forms.rs``) and tui's in-process walk
(``apps/fauna-tui/src/family.rs``) are not that witness.

**The reason is read as painted.** ``PlatformDriver.blocked_reason`` answers
what the app shows beside the control, and the journey compares it with the
``S.`` string of the predicate's reason key, never a literal. The screen-time
reason is ``S.family.policy_screen_time_invalid`` with ``{reason}`` filled by
the shared rule's own words — static strings of ``fauna_core::screen_time``
(``parse_time_of_day`` / ``parse_daily_minutes`` / ``ScreenTimePolicy::validate``),
which no ``i18n`` key carries; they are quoted below as that rule's words, the
same text every app is handed by the one shared function. An app whose driver
cannot read a painted reason yet takes the ``blocked-reason-read`` surface gate.

**Pressing a blocked control.** On an app that paints the submit from the
predicate the disabled control IS the refusal: no person can press it, and the
test agent refuses a press on a disabled control (convention 11), so a journey
never presses one. The press half of the ruling (each submit asking the same
predicate, refusing with the same reason on ``error-message`` and sending
nothing) is the shared predicate's backstop, pinned by tui's unit tests
(``apps/fauna-tui/src/family.rs``, the blocked-form walk). The two actor-id
forms are never pressed live either: a valid id is typed only to read the
control come alive, then the box is emptied again, so no contact or proposal is
left on the ward.

**A save that fails.** ``helpers/rpc_hold.refuse_rpc`` turns the policy save's
own request (``fauna.family.policy.update``) away at the nest without running its
handler; the journey reads the editor still open with the typed cells, releases
the refusal, presses the same Save again and reads the cells back from the nest,
then clears them and saves once more — so the ward's policy is left as found
(``family_pair`` also resets it before the test).

Every act the outcome names goes through the guardian's page (convention 8);
the waits are deadlines over a local repaint or a named request, never settle
sleeps (convention 14). Shares ``family_pair``: nothing this journey stores
survives it.
"""

from __future__ import annotations

import pytest

from helpers import budgets
from helpers.app_surface import require_surface
from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_family import (  # noqa: F401 - fixtures are registered by import
    _guardian_opens_editor,
    _reload_editor,
    _shared_family,
    family_pair,
)

pytestmark = [pytest.mark.tier_3]

POLICY_UPDATE = "fauna.family.policy.update"

CONTACT_INPUT = "family-contact-add-input"
CONTACT_BUTTON = "family-contact-add-button"
TRANSFER_INPUT = "family-transfer-input"
TRANSFER_BUTTON = "family-transfer-button"
POLICY_SAVE = "family-policy-save-button"

#: A well-formed 64-hex account id — typed only to read the control come alive,
#: never submitted.
VALID_ACTOR_ID = "ab" * 32

# The shared screen-time rule's own words (`libs/fauna-core/src/screen_time.rs`),
# the `{reason}` of `family.policy_screen_time_invalid` — no i18n key carries them.
RULE_HHMM = "a time of day looks like HH:MM (for example 21:00)"
RULE_RANGE = "a time of day runs from 00:00 to 23:59"
RULE_PAIRS = "window bounds come in pairs (set both or neither)"
RULE_BUDGET = "a daily budget is a whole number of minutes"


def _diag(driver, element_id: str) -> str:
    return (
        f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'))}, "
        f"{driver.diagnose('error-message')}"
    )


def _blocked_with(driver, element_id: str, reason: str) -> bool:
    if driver.is_absent(element_id):
        return False
    if driver.is_enabled(element_id):
        return False
    return driver.blocked_reason(element_id) == reason


def _assert_blocked_with(driver, element_id: str, reason: str, *, what: str) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it. The poll is a deadline over the repaint that follows the last
    keystroke; the asserts after it name every part that failed (convention 6)."""
    try:
        wait_until(lambda: _blocked_with(driver, element_id, reason), budgets.UI_SETTLE_S)
    except AssertionError:
        pass  # the asserts below say which half failed
    diag = _diag(driver, element_id)
    assert not driver.is_absent(element_id), (
        f"{what}: {element_id} must stay visible while the guardian can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id), (
        f"{what}: {element_id} is live while its draft cannot be sent "
        f"(family-safety.md § Errors & edge cases): {diag}"
    )
    painted = driver.blocked_reason(element_id)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _assert_offered(driver, element_id: str, *, what: str) -> None:
    """The draft is complete: the submit is live again (so the gate follows the
    draft, never a control that stays disabled)."""
    wait_until(
        lambda: driver.is_enabled(element_id),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"{what}: {element_id} stayed disabled on a complete draft: "
                         f"{_diag(driver, element_id)}",
    )


def _page_error(app) -> str:
    try:
        return app.error_text() or ""
    except Exception:  # noqa: BLE001 - an absent error element reads as none
        return ""


def _cells(app) -> tuple[str, str, str]:
    return (
        app.family.screen_window_start(),
        app.family.screen_window_end(),
        app.family.screen_daily_minutes(),
    )


def _screen_time_blocked(rule: str) -> str:
    return S.family.policy_screen_time_invalid(reason=rule)


@pytest.mark.feature("family-safety")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_guardian_contact_transfer_and_policy_save_are_blocked_with_their_reasons(
    admin_app, request, nest_instance, family_pair,
):
    """On the guardian's view of a ward: pre-approve-contact and propose-new-
    guardian are each disabled with their own reason while the box is blank and
    again while it holds something that is not an account id, and come alive on
    a 64-hex id; the policy Save is disabled with the screen-time rule's words
    while a cell does not read as a valid time (or budget), comes alive on valid
    cells, and a save the nest turns away keeps the editor with what was typed."""
    app = admin_app
    d = app.driver
    require_surface(d, "blocked-reason-read")
    port = nest_instance["port"]
    guardian_identity, _ward_identity, _guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    assert not d.is_absent(TRANSFER_INPUT), (
        f"precondition: the shared ward carries no pending transfer, so the propose "
        f"row renders: {d.diagnose(TRANSFER_INPUT)}, {d.diagnose('family-transfer-pending')}"
    )

    # ── 1. Pre-approve contact: blank, then not an account id, then a valid id. ──
    d.clear_and_type(CONTACT_INPUT, "")
    _assert_blocked_with(d, CONTACT_BUTTON, S.family.contact_add_required,
                         what="a blank contact box")
    d.clear_and_type(CONTACT_INPUT, "   ")
    _assert_blocked_with(d, CONTACT_BUTTON, S.family.contact_add_required,
                         what="a contact box of spaces only")
    d.clear_and_type(CONTACT_INPUT, "not-an-account-id")
    _assert_blocked_with(d, CONTACT_BUTTON, S.family.contact_add_invalid_actor_id,
                         what="a contact box that is not hex")
    d.clear_and_type(CONTACT_INPUT, "abc123")
    _assert_blocked_with(d, CONTACT_BUTTON, S.family.contact_add_invalid_actor_id,
                         what="a contact box of hex too short to be an account id")
    d.clear_and_type(CONTACT_INPUT, VALID_ACTOR_ID)
    _assert_offered(d, CONTACT_BUTTON, what="a 64-hex contact id")
    d.clear_and_type(CONTACT_INPUT, "")
    _assert_blocked_with(d, CONTACT_BUTTON, S.family.contact_add_required,
                         what="the contact box emptied again")

    # ── 2. Propose a new guardian: the same states, with the transfer's OWN words. ──
    assert S.family.transfer_required != S.family.contact_add_required
    assert S.family.transfer_invalid_actor_id != S.family.contact_add_invalid_actor_id
    d.clear_and_type(TRANSFER_INPUT, "")
    _assert_blocked_with(d, TRANSFER_BUTTON, S.family.transfer_required,
                         what="a blank new-guardian box")
    d.clear_and_type(TRANSFER_INPUT, "not-an-account-id")
    _assert_blocked_with(d, TRANSFER_BUTTON, S.family.transfer_invalid_actor_id,
                         what="a new-guardian box that is not an account id")
    d.clear_and_type(TRANSFER_INPUT, VALID_ACTOR_ID)
    _assert_offered(d, TRANSFER_BUTTON, what="a 64-hex new-guardian id")
    d.clear_and_type(TRANSFER_INPUT, "")
    _assert_blocked_with(d, TRANSFER_BUTTON, S.family.transfer_required,
                         what="the new-guardian box emptied again")
    assert not app.family.transfer_pending_visible(), (
        f"no proposal was sent, so none may be pending: {d.diagnose('family-transfer-pending')}"
    )

    # ── 3. The policy Save: each malformed cell blocks it with the rule's own words. ──
    assert _cells(app) == ("", "", ""), (
        f"precondition: the reset pair carries no screen time, read {_cells(app)!r}"
    )
    _assert_offered(d, POLICY_SAVE, what="an editor with no screen time set")
    for start, end, daily, rule, what in (
        ("9pm", "07:00", "", RULE_HHMM, "a window start that is not HH:MM"),
        ("25:00", "07:00", "", RULE_RANGE, "a window start past 23:59"),
        ("21:00", "07:60", "", RULE_RANGE, "a window end past 23:59"),
        ("21:00", "", "", RULE_PAIRS, "a window with only its start"),
        ("", "", "lots", RULE_BUDGET, "a daily budget that is not a number"),
    ):
        app.family.set_screen_window(start, end)
        app.family.set_screen_daily_minutes(daily)
        _assert_blocked_with(d, POLICY_SAVE, _screen_time_blocked(rule),
                             what=f"{what} ({start!r}..{end!r}, budget {daily!r})")

    typed = ("21:00", "07:00", "90")
    app.family.set_screen_window(typed[0], typed[1])
    app.family.set_screen_daily_minutes(typed[2])
    _assert_offered(d, POLICY_SAVE, what="a valid window and budget")

    # ── 4. A save the nest turns away keeps the editor and what was typed. ──
    refuse_rpc(port, POLICY_UPDATE)
    try:
        d.click(POLICY_SAVE)
        wait_for_refused_rpc(port, POLICY_UPDATE)
        error = wait_until(
            lambda: _page_error(app),
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"a refused policy save said nothing on error-message: "
                             f"{_diag(d, POLICY_SAVE)}",
        )
        assert app.family.policy_editor_visible(), (
            f"a refused policy save closed the editor; error={error!r}"
        )
        assert _cells(app) == typed, (
            f"a refused policy save must keep the typed cells {typed!r}; "
            f"read {_cells(app)!r}, error={error!r}"
        )
    finally:
        release_rpc_hold(port, POLICY_UPDATE)

    # ── 5. The same press once the nest takes it: the cells read back from the nest. ──
    try:
        d.click(POLICY_SAVE)

        def _stored() -> bool:
            _reload_editor(app, ward_handle)
            return _cells(app) == typed

        wait_until(
            _stored,
            budgets.RPC_ROUNDTRIP_S,
            interval=0.5,
            diagnose=lambda: f"the retried save never stored {typed!r}; the editor refilled "
                             f"{_cells(app)!r}, error={_page_error(app)!r}",
        )
    finally:
        # Leave the ward's screen time as found: empty cells are valid ("unset").
        app.family.set_screen_window("", "")
        app.family.set_screen_daily_minutes("")
        _assert_offered(d, POLICY_SAVE, what="the cells emptied again")
        d.click(POLICY_SAVE)

        def _cleared() -> bool:
            _reload_editor(app, ward_handle)
            return _cells(app) == ("", "", "")

        wait_until(
            _cleared,
            budgets.RPC_ROUNDTRIP_S,
            interval=0.5,
            diagnose=lambda: f"clearing the screen time did not store; the editor refilled "
                             f"{_cells(app)!r}, error={_page_error(app)!r}",
        )
