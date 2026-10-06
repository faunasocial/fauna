"""E2E coverage for handle_entry outcomes.

Each test uses set_handle_check_snapshot to put the wizard in a
specific outcome state, then asserts the page renders the expected
elements + message. The smoke E2E test_flow_a in a sibling file
exercises the network-real path; this file is pure UI rendering.

`message.key` values are the i18n key paths the wizard's machine emits
(see libs/fauna-onboarding-machine/src/machine.rs's
LocalizedText constructions); the client looks them up against its
generated i18n table at render time.

These tests are RED at the end of Plan 1; per-app wiring (Plans 2-6)
turns them green one app at a time.
"""

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot

pytestmark = pytest.mark.tier_2


@pytest.mark.parametrize("driver_kind", ["web", "linux", "windows", "ios", "macos", "android"])
def test_format_invalid_disables_continue(app, driver_kind):
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": "FormatInvalid",
        "message": {"key": "onboarding.handle_check.outcome.format_invalid", "args": {}},
        "continue_enabled": False,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    assert app.is_visible("handle-message-area"), (
        "FormatInvalid Complete state should render the message area: "
        f"{app.driver.diagnose('handle-message-area')} error={app.error_text()!r}"
    )
    assert app.get_text("handle-message-area"), (
        "FormatInvalid Complete state should render non-empty message text: "
        f"{app.driver.diagnose('handle-message-area')} error={app.error_text()!r}"
    )
    assert not app.is_enabled("handle-entry-continue-button"), (
        "FormatInvalid should disable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )


def test_tld_invalid_disables_continue(app):
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": "TldInvalid",
        "message": {"key": "onboarding.handle_check.outcome.tld_invalid", "args": {"tld": "xyz"}},
        "continue_enabled": False,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    assert not app.is_enabled("handle-entry-continue-button"), (
        "TldInvalid should disable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )


def test_domain_available_unpriced_enables_continue(app):
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": {"DomainAvailable": {"buyable_via_provider": True, "price": None}},
        "message": {
            "key": "onboarding.handle_check.outcome.domain_available_unpriced",
            "args": {"domain": "example.com"},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    assert app.is_enabled("handle-entry-continue-button"), (
        "DomainAvailable (unpriced) should enable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )


def test_registered_no_nest_shows_checkbox(app):
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": "RegisteredNoNest",
        "message": {
            "key": "onboarding.handle_check.outcome.registered_no_nest",
            "args": {"domain": "example.com"},
        },
        "continue_enabled": False,
        "control_checkbox_visible": True,
        "control_checkbox_checked": False,
    })
    assert app.is_visible("handle-control-checkbox"), (
        "RegisteredNoNest should show the control checkbox: "
        f"{app.driver.diagnose('handle-control-checkbox')} error={app.error_text()!r}"
    )
    assert not app.is_enabled("handle-entry-continue-button"), (
        "RegisteredNoNest should disable Continue until the checkbox is checked: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )
    app.click("handle-control-checkbox")
    assert app.is_enabled("handle-entry-continue-button"), (
        "checking the control checkbox should enable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )


@pytest.mark.feature("join-a-nest")
def test_already_on_nest_handle_matches_enables_continue(app):
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": {"AlreadyOnNest": {
            "handle_matches": True,
            "current_handle": "alice@example.com",
        }},
        "message": {
            "key": "onboarding.handle_check.outcome.already_on_nest_handle_matches",
            "args": {"handle": "alice@example.com"},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    assert app.is_enabled("handle-entry-continue-button"), (
        "AlreadyOnNest (handle matches) should enable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )


def test_already_on_nest_handle_differs_shows_old_handle(app):
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": {"AlreadyOnNest": {
            "handle_matches": False,
            "current_handle": "bob@example.com",
        }},
        "message": {
            "key": "onboarding.handle_check.outcome.already_on_nest_handle_differs",
            "args": {"domain": "example.com", "old_handle": "bob@example.com"},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    text = app.get_text("handle-message-area")
    assert "bob@example.com" in text


@pytest.mark.feature("join-a-nest")
def test_nest_user_unregistered_routes_to_invite(app):
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": "NestRunningUserUnregistered",
        "message": {
            "key": "onboarding.handle_check.outcome.user_unregistered",
            "args": {"domain": "example.com"},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    assert app.is_enabled("handle-entry-continue-button"), (
        "NestRunningUserUnregistered should enable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )
    app.click("handle-entry-continue-button")
    assert app.is_visible("invite-request-submit-button"), (
        "Continue from user-unregistered should route to the invite_request page: "
        f"{app.driver.diagnose('invite-request-submit-button')} error={app.error_text()!r}"
    )


@pytest.mark.feature("join-a-nest")
def test_probe_error_transient_shows_retry(app):
    # Populate the input so the Check button (gated on input non-empty)
    # is reachable as Retry. Per target-state §"Handle entry":
    # "Check stays disabled while the input is empty" — a user reaching
    # the ProbeError state must have typed a handle first.
    import json as _json
    app.driver.call_machine_method(
        "set_current_handle", _json.dumps("alice@example.com")
    )
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": {"ProbeError": {
            "phase": "DnsLookup", "transient": True, "cause": "no network",
        }},
        "message": {"key": "onboarding.handle_check.error.no_network", "args": {}},
        "continue_enabled": False,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    assert not app.is_enabled("handle-entry-continue-button"), (
        "ProbeError (transient) should disable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} error={app.error_text()!r}"
    )
    # The Check button should remain enabled (it doubles as Retry).
    assert app.is_enabled("handle-check-button"), (
        "ProbeError should keep the Check button enabled (doubles as Retry): "
        f"{app.driver.diagnose('handle-check-button')} error={app.error_text()!r}"
    )
