"""A supervised child's Contacts page offers only what can succeed: while the
guardian must approve new contacts, the knock toward a stranger is blocked with
the refusal's own words and the ask is offered in its place before anything
was refused, an incoming request's accept is blocked by the
waiting-for-the-guardian line, and until the child's app has read its policy
both say they are loading.

Goal: ``docs/goal/ui/contacts.md`` § Errors & edge cases (*A supervised ward's
knock and accept are gated at the entry*: the shared predicates
``fauna_client_family::ward_asks::knock_possibility`` / ``accept_possibility``
over ``OwnSupervision`` — unread until this run's first successful
``fauna.family.status`` read — answer ``contacts.guardian_approval_required``
for the knock, ``contacts.knock_guardian_pending`` for each accept, and
``common.loading`` for both while unread; ``contact-request-guardian-button``
is then the result's one step), and its § Implementation status today (shared
Rust, tui, linux, web, android, macos and ios built; windows still offers the
ask only after the typed refusal). The rule is
``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what the
person can unblock is disabled with its reason within eyeshot; rule 3: the one
unblocking step is offered; rule 5: nothing is enabled on a guess before its
read). Catalog: ``docs/features/family-safety.md`` outcome 69.

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet,
when its reason is not within eyeshot, or when a control this person can never
use is shown. ``ward_asks.rs``'s unit tests and tui's in-process
``contacts.rs`` tests are not that witness.

**The reason is read as painted.** ``PlatformDriver.blocked_reason`` answers
what the app shows beside the control, compared with the ``S.`` string of the
predicate's reason key, never a literal. An app whose driver cannot read it yet
takes the ``blocked-reason-read`` surface gate; an app that has not lifted the
entry gate takes the ``ward-contact-entry-gate`` one.

**Pressing a blocked control.** A journey never presses a disabled control
(the test agent refuses it, convention 11); the press half — the knock and the
accept asking the same predicate before anything is sent — is the shared
predicate's backstop, pinned by tui's unit tests.

**The loading window is stood in, not raced.** The nest holds the child's
``fauna.family.status`` read (``helpers/rpc_hold``) from before the child signs
in, and ``wait_for_held_rpc`` proves the app asked; the loading assertions run
while the reply is parked, so they read the genuine unread paint. Releasing the
hold lets the read answer, and the gated state follows from it.

What the journey arranges rather than drives: the shared guardian/ward pair
(``tests/test_family.py``'s ``family_pair``), the two strangers (direct-admitted
adults), and the incoming request (the nest's knock door, as the stranger is
not the user under test). The guardian's contact-approval switch is a real
gesture on the guardian's editor (convention 8); the module's policy reset
over the wire turns it back off at the end so the child, free to decide again,
dismisses the request through its own row.
"""

from __future__ import annotations

import pytest

from helpers.admin_wire import admit_adult
from helpers.app_surface import app_name, require_surface, surface_built
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.family_wire import reset_ward_policy
from helpers.rpc_hold import (
    arm_rpc_hold,
    release_rpc_hold,
    rpc_hold_status,
    wait_for_held_rpc,
)
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_family import (  # noqa: F401
    _guardian_opens_editor,
    _push_knock,
    _shared_family,
    _unique_suffix,
    family_pair,
)

pytestmark = [pytest.mark.tier_3]

#: The wire kind of the child's own supervision read (the nest's router
#: registration, as ``test_family.py`` holds it).
FAMILY_STATUS_KIND = "fauna.family.status"

KNOCK = "contacts-add-button"
ASK = "contact-request-guardian-button"
ACCEPT = "contacts-accept-button"
BLOCK = "contacts-block-button"


def _diag(driver, element_id: str) -> str:
    return (
        f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'))}, "
        f"{driver.diagnose('error-message')}"
    )


def _reasons(driver, element_id: str) -> list[str | None]:
    return [driver.blocked_reason(element_id, i) for i in range(driver.count(element_id))]


def _blocked_with(driver, element_id: str, reason: str) -> bool:
    if driver.count(element_id) == 0:
        return False
    if driver.is_enabled(element_id):
        return False
    return set(_reasons(driver, element_id)) == {reason}


def _assert_blocked_with(driver, element_id: str, reason: str, *, what: str) -> None:
    """Every ``element_id`` is shown, not live, and the words ``reason`` are
    painted beside each. The answer is the account's (one predicate over the
    child's own policy, never per row), so every occurrence carries the same
    reason; enabledness is read on the first, the only occurrence the driver's
    ``is_enabled`` addresses. The poll is a deadline over the repaint that
    follows the read it depends on; the asserts after it name every part that
    failed (convention 6)."""
    try:
        wait_until(lambda: _blocked_with(driver, element_id, reason),
                   RPC_ROUNDTRIP_S + UI_SETTLE_S)
    except AssertionError:
        pass  # the asserts below say which half failed
    diag = _diag(driver, element_id)
    assert driver.count(element_id) > 0, (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id), (
        f"{what}: {element_id} is live while it cannot succeed: {diag}"
    )
    painted = _reasons(driver, element_id)
    assert set(painted) == {reason}, (
        f"{what}: the reason beside every {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _page_error(app) -> str:
    try:
        return app.error_text() or ""
    except Exception:  # noqa: BLE001 - an absent error element reads as none
        return ""


@pytest.mark.feature("family-safety")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_ward_knock_and_accept_are_blocked_with_their_reasons_and_the_ask_is_offered_at_the_entry(
    admin_app, request, nest_instance, family_pair,
):
    """With the child's policy read held, the knock toward a stranger and the
    accept on an incoming request are both blocked as loading and no ask is
    offered on a guess; once the read answers with contact approval on, the
    knock is blocked with the refusal's words, the ask is offered live in its
    place with nothing refused yet, and the accept is blocked by the
    waiting-for-the-guardian line while the row's block stays the child's."""
    from conftest import _login_app_as

    app = admin_app
    d = app.driver
    require_surface(d, "blocked-reason-read")
    if not surface_built("ward-contact-entry-gate", app_name(d)):
        pytest.skip(f"{app_name(d)} has not lifted the ward's contact entry gate "
                    "(surfaces.toml [ward-contact-entry-gate])")
    url = nest_instance["url"]
    port = nest_instance["port"]
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair

    # ── The guardian turns contact approval on, through the editor (point 8). ──
    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_contact_approval(True)
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"

    suffix = _unique_suffix()
    stranger = admit_adult(nest_instance, f"ward-entry-stranger-{suffix}")
    knocker = admit_adult(nest_instance, f"ward-entry-knocker-{suffix}")
    stranger_hex = stranger["actor_id_hex"]
    knocker_hex = knocker["actor_id_hex"]
    _push_knock(url, ward_identity["actor_id_hex"], knocker_hex, f"offer-possible hello {suffix}")

    def _knock_row():
        row = app.contacts.knock_index_for(knocker_hex)
        if row is not None:
            return (row,)  # a tuple: row 0 is falsy to wait_until
        app.contacts.navigate()
        return None

    def _find_stranger() -> None:
        app.contacts.find_by_actor_id(stranger_hex)
        wait_until(
            lambda: app.contacts.actor_id_result_text() == stranger_hex,
            RPC_ROUNDTRIP_S + UI_SETTLE_S,
            diagnose=lambda: f"the Find User lookup did not resolve the stranger: "
                             f"{app.contacts.actor_id_result_text()!r}, "
                             f"error={_page_error(app)!r}",
        )

    try:
        # ── 1. The child signs in with its policy read held: both controls say
        #    they are loading, and no ask is offered on a guess (rule 5). ──
        d.reset()
        released_before = rpc_hold_status(port, FAMILY_STATUS_KIND).get("released", 0)
        arm_rpc_hold(port, FAMILY_STATUS_KIND)
        try:
            _login_app_as(app, request, nest_instance, ward_identity)
            wait_for_held_rpc(port, FAMILY_STATUS_KIND,
                              diagnose=lambda: f"error={_page_error(app)!r}")
            app.contacts.navigate()
            _find_stranger()
            _assert_blocked_with(d, KNOCK, S.common.loading,
                                 what="the knock before the child's policy is read")
            assert d.count(ASK) == 0, (
                "the ask is offered before the child's policy is read — an offer "
                f"made on a guess (rule 5): {_diag(d, ASK)}"
            )
            wait_until(
                _knock_row,
                RPC_ROUNDTRIP_S + UI_SETTLE_S,
                diagnose=lambda: f"the stranger's request is not listed; senders="
                                 f"{app.contacts.knock_senders()!r} error={_page_error(app)!r}",
            )
            _assert_blocked_with(d, ACCEPT, S.common.loading,
                                 what="an accept before the child's policy is read")
            assert rpc_hold_status(port, FAMILY_STATUS_KIND).get("holding", 0) >= 1, (
                "the policy read was answered before the loading assertions ran, "
                "so they proved nothing about the unread window"
            )
        finally:
            release_rpc_hold(port, FAMILY_STATUS_KIND)
        wait_until(
            lambda: rpc_hold_status(port, FAMILY_STATUS_KIND).get("released", 0)
            > released_before,
            RPC_ROUNDTRIP_S,
            diagnose=lambda: f"status={rpc_hold_status(port, FAMILY_STATUS_KIND)!r}",
        )
        d.barrier()

        # ── 2. The read answers with approval on: the knock is blocked with the
        #    refusal's own words, and the ask is offered live at the entry. ──
        def _gated() -> bool:
            if d.count(ASK) == 1 and _blocked_with(
                d, KNOCK, S.contacts.guardian_approval_required
            ):
                return True
            # The page has not re-read since the release: a fresh visit does.
            app.family.navigate()
            app.contacts.navigate()
            app.contacts.find_by_actor_id(stranger_hex)
            return False

        try:
            wait_until(_gated, RPC_ROUNDTRIP_S + UI_SETTLE_S)
        except AssertionError:
            pass  # the asserts below say which half failed
        _assert_blocked_with(d, KNOCK, S.contacts.guardian_approval_required,
                             what="the knock toward a stranger while the guardian decides")
        assert d.count(ASK) == 1, (
            "the ask is not offered at the entry while the guardian decides new "
            f"contacts (rule 3): {_diag(d, ASK)}"
        )
        assert d.is_enabled(ASK), f"the ask is offered but not live: {_diag(d, ASK)}"
        assert d.count("contact-request-pending") == 0, (
            "nothing was asked yet, so no pending ask may paint: "
            f"{d.diagnose('contact-request-pending')}"
        )
        assert S.contacts.guardian_approval_required not in _page_error(app), (
            "the ask must be offered before anything was refused, yet the page "
            f"carries the refusal: {_page_error(app)!r}"
        )

        # ── 3. The incoming request: accept blocked by the line the row already
        #    paints; block stays the child's (read beside it, so a bridge that
        #    cannot read enabledness fails here instead of passing vacuously). ──
        wait_until(
            _knock_row,
            RPC_ROUNDTRIP_S + UI_SETTLE_S,
            diagnose=lambda: f"the stranger's request left the list; senders="
                             f"{app.contacts.knock_senders()!r} error={_page_error(app)!r}",
        )
        _assert_blocked_with(d, ACCEPT, S.contacts.knock_guardian_pending,
                             what="an accept while the guardian decides")
        assert d.is_enabled(BLOCK), (
            f"block must stay the child's: {_diag(d, BLOCK)}"
        )
    finally:
        release_rpc_hold(port, FAMILY_STATUS_KIND)
        reset_ward_policy(nest_instance, guardian_identity, ward_identity)

    # ── Leave no request behind: with approval off the child decides again and
    #    dismisses it through its own row. ──
    def _free_row():
        app.family.navigate()
        app.family.reload()
        app.contacts.navigate()
        row = app.contacts.knock_index_for(knocker_hex)
        if row is not None and d.is_enabled(ACCEPT):
            return (row,)
        return None

    (row,) = wait_until(
        _free_row,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"with approval off the request's accept stayed blocked: "
                         f"{_diag(d, ACCEPT)}",
    )
    app.contacts.dismiss_knock(row)
    wait_until(
        lambda: app.contacts.knock_index_for(knocker_hex) is None or app.contacts.navigate(),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"the dismissed request must leave the list. error={_page_error(app)!r}",
    )
