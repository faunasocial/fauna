"""The by-continuing terms notice on the four account-making pages.

`docs/goal/behavior/terms-and-content-guidelines.md` § The decisions, item 3
(amended 2026-10-19) and § Element IDs: `onboarding-terms-notice` (the sentence)
with `onboarding-terms-link` (the button inside it) sits above the bottom row of
`invite_request`, `claim_code` and `nest_provisioning` always, and of
`handle_entry` only while Continue signs in — `handle_check_snapshot()`'s
`terms_notice_visible`, true iff the outcome is `AlreadyOnNest`. The other two
nest outcomes lead on to a page carrying its own notice, so `handle_entry` shows
none for them.

Pure UI rendering through the machine's test setters, as
`test_handle_entry_outcomes.py` does: no nest, no network. The link's target is
`fauna_core::TERMS_URL`; a terminal cannot open a browser, so tui's link shows
the URL and copies it — the walk reads the URL off the button.

tui leads (`onboarding.md` § 2, § 3, § 3a; `onboarding-provisioning.md` § 6);
the six other apps follow in the batched trickle-down and skip as unbuilt here.
"""

import json

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.app_surface import app_name, skip_unbuilt
from i18n.strings import S

pytestmark = pytest.mark.tier_2

#: `fauna_core::TERMS_URL` — the one constant every app's link opens.
TERMS_URL = "https://fauna.social/terms"

#: The apps that paint the notice today (tui first, then the trickle-down).
_PAINTED = ("tui", "web", "linux", "android", "macos", "ios")

#: The apps whose link text carries the URL: a terminal cannot open a browser,
#: so tui shows and copies it. The others open it in the browser and label the
#: link — their target is pinned by the shared `terms_url` faces' unit tests.
_SHOWS_URL = ("tui",)


def _require_the_notice(app):
    if app_name(app.driver) not in _PAINTED:
        skip_unbuilt(
            app.driver,
            surface="onboarding-terms-notice",
            detail="the by-continuing terms notice is painted on tui first",
            tracked="docs/goal/behavior/terms-and-content-guidelines.md § Implementation status today",
        )


def _land(app, step: str):
    app.driver.call_machine_method("set_step_for_test", json.dumps(step))


def _assert_the_notice(app, page: str):
    assert app.is_visible("onboarding-terms-notice"), (
        f"{page} should show the by-continuing terms notice: "
        f"{app.driver.diagnose('onboarding-terms-notice')} error={app.error_text()!r}"
    )
    text = app.get_text("onboarding-terms-notice").strip()
    assert text == S.onboarding.terms.notice, (
        f"{page}'s terms notice reads {text!r}, expected the shared sentence "
        f"{S.onboarding.terms.notice!r}"
    )
    assert app.is_enabled("onboarding-terms-link"), (
        f"{page} should offer the terms link inside the notice: "
        f"{app.driver.diagnose('onboarding-terms-link')} error={app.error_text()!r}"
    )
    link = app.get_text("onboarding-terms-link")
    if app_name(app.driver) in _SHOWS_URL:
        assert TERMS_URL in link, (
            f"{page}'s terms link {link!r} does not show {TERMS_URL} — a terminal "
            f"shows the address it copies"
        )
    else:
        assert S.settings.terms_link in link, (
            f"{page}'s terms link reads {link!r}, expected the label "
            f"{S.settings.terms_link!r}"
        )


def _assert_no_notice(app, why: str):
    assert app.driver.is_absent("onboarding-terms-notice"), (
        f"handle_entry should show no terms notice {why}: "
        f"{app.driver.diagnose('onboarding-terms-notice')}"
    )
    assert app.driver.is_absent("onboarding-terms-link"), (
        f"handle_entry should show no terms link {why}: "
        f"{app.driver.diagnose('onboarding-terms-link')}"
    )


def _handle_outcome(app, outcome, key: str, args: dict):
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": outcome,
        "message": {"key": key, "args": args},
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    # The page is painted from the snapshot; Continue enabled is the anchor
    # that the injected outcome has reached the render.
    assert app.is_enabled("handle-entry-continue-button"), (
        f"the injected {outcome!r} outcome should enable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )


def test_invite_request_shows_the_terms_notice(app):
    _require_the_notice(app)
    _land(app, "InviteRequest")
    assert app.is_visible("invite-request-submit-button"), app.driver.diagnose(
        "invite-request-submit-button"
    )
    _assert_the_notice(app, "invite_request")


def test_claim_code_shows_the_terms_notice(app):
    _require_the_notice(app)
    _land(app, "ClaimCode")
    assert app.is_visible("claim-code-input"), app.driver.diagnose("claim-code-input")
    _assert_the_notice(app, "claim_code")


def test_nest_provisioning_shows_the_terms_notice(app):
    _require_the_notice(app)
    _land(app, "NestProvisioning")
    # The page's own region, not the price recap: web paints the recap only once
    # the wizard holds a priced line, and this walk injects the step alone.
    assert app.is_visible("provisioning-progress"), app.driver.diagnose(
        "provisioning-progress"
    )
    _assert_the_notice(app, "nest_provisioning")


def test_handle_entry_shows_the_notice_only_when_continue_signs_in(app):
    _require_the_notice(app)
    # Before any check: nothing to continue to, no notice.
    set_handle_check_snapshot(app, {
        "phase": "Idle",
        "outcome": "None",
        "message": {"key": "onboarding.handle_check.idle", "args": {}},
        "continue_enabled": False,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    assert app.is_visible("handle-input"), app.driver.diagnose("handle-input")
    _assert_no_notice(app, "before a check")

    # An identity already registered on the nest: Continue signs in.
    _handle_outcome(
        app,
        {"AlreadyOnNest": {"handle_matches": True, "current_handle": "alice@example.com"}},
        "onboarding.handle_check.outcome.already_on_nest_handle_matches",
        {"handle": "alice@example.com"},
    )
    _assert_the_notice(app, "handle_entry (already on the nest)")

    # A running nest that does not know the user: Continue leads to
    # invite_request, which carries its own notice.
    _handle_outcome(
        app,
        "NestRunningUserUnregistered",
        "onboarding.handle_check.outcome.user_unregistered",
        {"domain": "example.com"},
    )
    _assert_no_notice(app, "when Continue leads to invite_request")

    # An unclaimed nest: Continue leads to claim_code, which carries its own.
    _handle_outcome(
        app,
        "UnregisteredUnclaimedNest",
        "onboarding.handle_check.outcome.unregistered_unclaimed_nest",
        {"domain": "example.com"},
    )
    _assert_no_notice(app, "when Continue leads to claim_code")
