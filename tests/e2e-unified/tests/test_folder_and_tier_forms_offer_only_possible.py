"""The folder wizard, the follow-a-public-folder form and the tier and redeem
forms offer only a submit that can succeed: each journey empties a draft
through the page itself and reads that the submit is disabled with its reason
beside it, then completes the draft and reads it live again.

Goal: ``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what
the person can unblock is disabled with its reason within eyeshot; rule 4's
form case: a form's submit is painted from its draft-validity predicate). The
per-page instances: ``docs/goal/ui/folders.md`` § Errors & edge cases (*The
follow and share forms are painted from a draft-validity predicate*; *Own-tier
and mail gates* — the wizard's Next) and ``docs/goal/ui/profile.md`` § Errors &
edge cases (*The tier form and the claim redeem are painted from a
draft-validity predicate*).

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet
or when its reason is not within eyeshot. The shared predicates' machine tests
(``fauna_client_folders::forms``, ``fauna_folders_machine::possibility``,
``fauna_client_subscriptions::forms``) and tui's in-process walk are not that
witness.

**The reason is read as painted** — ``PlatformDriver.blocked_reason``, compared
with the ``S.`` string of the predicate's reason key, never a literal; an app
whose driver cannot read it yet takes the ``blocked-reason-read`` surface gate.

**Pressing a blocked control.** The disabled control IS the refusal on an app
that paints it from the predicate, and the test agent refuses a press on a
disabled control (convention 11), so a journey never presses one; the press
half (the submit refusing with the same reason on ``error-message``) is the
shared predicate's backstop, pinned by each app's unit tests.

**Not witnessed here, handed on.** The wizard Create's in-flight arm (disabled
"being created" while the create is on its way) needs the create held at the
nest while the page is read, which tui's inline op cannot answer from its loop.

Every act goes through the page (convention 8); each wait is a deadline over a
local repaint, never a settle sleep (convention 14).
"""

from __future__ import annotations

import pytest

from helpers import budgets
from helpers.app_surface import require_surface
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]


def _diag(driver, element_id: str, scope: str | None = None) -> str:
    return (
        f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'), scope=scope)}, "
        f"{driver.diagnose('error-message')}"
    )


def _blocked_with(driver, element_id: str, reason: str, scope: str | None) -> bool:
    if driver.is_absent(element_id, scope=scope):
        return False
    if driver.is_enabled(element_id, scope=scope):
        return False
    return driver.blocked_reason(element_id, scope=scope) == reason


def _assert_blocked_with(driver, element_id: str, reason: str, *, what: str,
                         scope: str | None = None) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it. The poll is a deadline over the repaint that follows the last
    keystroke; the asserts after it name the part that failed (convention 6)."""
    try:
        wait_until(lambda: _blocked_with(driver, element_id, reason, scope),
                   budgets.UI_SETTLE_S)
    except AssertionError:
        pass  # the asserts below say which part failed
    diag = _diag(driver, element_id, scope)
    assert not driver.is_absent(element_id, scope=scope), (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id, scope=scope), (
        f"{what}: {element_id} is live while its draft cannot succeed: {diag}"
    )
    painted = driver.blocked_reason(element_id, scope=scope)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _assert_offered(driver, element_id: str, *, what: str, scope: str | None = None) -> None:
    """The draft is complete: the submit is live again, so the gate follows the
    draft rather than a control that stays disabled."""
    wait_until(
        lambda: driver.is_enabled(element_id, scope=scope),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"{what}: {element_id} stayed disabled on a complete draft: "
                         f"{_diag(driver, element_id, scope)}",
    )


# ── follow-a-public-folder 12 ────────────────────────────────────────────────


@pytest.mark.feature("follow-a-public-folder")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_follow_confirm_says_which_box_is_missing_until_both_are_typed(logged_in_app):
    """With the follow form open and nothing typed, the confirm is disabled and
    asks for the owner's handle; with the handle typed and no folder name it
    asks for the name; with both typed it is live."""
    app = logged_in_app
    d = app.driver
    require_surface(d, "blocked-reason-read")

    app.backups.navigate_folders()
    d.wait_for("folder-follow-button", timeout=budgets.UI_SETTLE_S)
    d.click("folder-follow-button")
    d.wait_for("folder-follow-name-input", timeout=budgets.UI_SETTLE_S)

    d.clear_and_type("recipient-picker-input", "")
    d.clear_and_type("folder-follow-name-input", "")
    _assert_blocked_with(d, "folder-follow-confirm", S.folders.follow_handle_required,
                         what="a follow with neither box typed")

    d.clear_and_type("recipient-picker-input", "row-6221-owner")
    _assert_blocked_with(d, "folder-follow-confirm", S.folders.follow_name_required,
                         what="a follow with the handle typed and no folder name")

    d.clear_and_type("folder-follow-name-input", "row-6221-pictures")
    _assert_offered(d, "folder-follow-confirm", what="a follow with both boxes typed")

    # Blanking the handle again blocks it again: the gate follows the draft.
    d.clear_and_type("recipient-picker-input", "   ")
    _assert_blocked_with(d, "folder-follow-confirm", S.folders.follow_handle_required,
                         what="a follow whose handle was cleared to blanks")


# ── folders 21 (Next arm) ────────────────────────────────────────────────────


@pytest.mark.feature("folders")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_new_folder_wizard_next_says_a_name_is_needed_while_the_name_is_blank(
    logged_in_app,
):
    """On the wizard's name step with no name typed, Next is disabled and says a
    name is needed; a typed name makes it live, and a name cleared to blanks
    blocks it again."""
    app = logged_in_app
    d = app.driver
    require_surface(d, "blocked-reason-read")
    b = app.backups

    b.navigate_folders()
    b.add_folder()
    d.wait_for("wizard-name-input", timeout=budgets.UI_SETTLE_S)

    b.wizard_set_name("")
    _assert_blocked_with(d, "wizard-next-button", S.devices.wizard.name_required,
                         what="the wizard's name step with no name")

    b.wizard_set_name("row-6221-folder")
    _assert_offered(d, "wizard-next-button", what="the wizard's name step with a name")

    b.wizard_set_name("   ")
    _assert_blocked_with(d, "wizard-next-button", S.devices.wizard.name_required,
                         what="the wizard's name step with a name of blanks")


# ── subscriptions-and-tiers 37 ───────────────────────────────────────────────


@pytest.mark.feature("subscriptions-and-tiers")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_tier_save_and_redeem_say_what_is_missing_while_the_name_or_code_is_blank(
    logged_in_app,
):
    """The tier form's Save is disabled and says a name is needed while the name
    is blank; the claim redeem is disabled and says a code is needed while the
    code is blank; typing either makes its submit live. Nothing is sent: the
    form is cancelled and no code is redeemed."""
    app = logged_in_app
    d = app.driver
    require_surface(d, "blocked-reason-read")
    subs = app.subscriptions

    # The tier form, on the SELF Tiers tab.
    subs.navigate()
    subs.open_tiers_tab()
    d.click("subscription-tier-create-button")
    d.wait_for("subscription-tier-form", timeout=budgets.UI_SETTLE_S)
    d.clear_and_type("subscription-tier-form-name", "")
    _assert_blocked_with(d, "subscription-tier-form-save", S.subscriptions.tier_name_required,
                         what="a tier form with no name")
    d.clear_and_type("subscription-tier-form-name", "row-6221-tier")
    _assert_offered(d, "subscription-tier-form-save", what="a tier form with a name")
    d.clear_and_type("subscription-tier-form-name", "  ")
    _assert_blocked_with(d, "subscription-tier-form-save", S.subscriptions.tier_name_required,
                         what="a tier form whose name was cleared to blanks")
    subs.cancel_tier_form()

    # The claim redeem, on Settings ▸ Subscriptions.
    subs.navigate_settings()
    d.wait_for("subscription-claim-redeem-input", timeout=budgets.UI_SETTLE_S)
    d.clear_and_type("subscription-claim-redeem-input", "")
    _assert_blocked_with(d, "subscription-claim-redeem-button",
                         S.subscriptions.redeem_code_required,
                         what="a redeem with no code")
    d.clear_and_type("subscription-claim-redeem-input", "ROW-6221-CODE")
    _assert_offered(d, "subscription-claim-redeem-button", what="a redeem with a code")
    d.clear_and_type("subscription-claim-redeem-input", "")
