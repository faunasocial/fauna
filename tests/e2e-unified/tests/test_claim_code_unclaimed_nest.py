"""E2E coverage for the claim_code page (unclaimed-nest branch).

When handle-check returns UnregisteredUnclaimedNest, the wizard routes
to claim_code instead of invite_request — there is no admin to issue
invites yet, so the only path forward is to enter the one-time claim
code printed by the nest server's bootstrap process and atomically
become the admin via POST /api/v1/claim-admin.

These tests use the test-helpers bridge (set_handle_check_snapshot,
set_claim_code_snapshot) — no real nest required, no real /claim-admin
POST. Rust unit tests in libs/fauna-onboarding-machine/tests/ cover the
HTTP-level claim flow.

Per docs/goal/behavior/onboarding.md §3a (Claim code).
"""

import json
import time

import pytest

from drivers.machine_test_setter import (
    set_claim_code_snapshot,
    set_handle_check_snapshot,
)

pytestmark = pytest.mark.tier_2


@pytest.mark.feature("claim-a-fresh-nest")
def test_unclaimed_nest_routes_to_claim_code(app):
    """Handle-check UnregisteredUnclaimedNest → click Continue → claim_code page."""
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": "UnregisteredUnclaimedNest",
        "message": {
            "key": "onboarding.handle_check.outcome.unregistered_unclaimed_nest",
            "args": {"domain": "example.com"},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    })
    assert app.is_enabled("handle-entry-continue-button"), (
        "UnregisteredUnclaimedNest should enable Continue: "
        f"{app.driver.diagnose('handle-entry-continue-button')} "
        f"error={app.error_text()!r}"
    )
    app.click("handle-entry-continue-button")
    assert app.is_visible("claim-code-input"), (
        "UnregisteredUnclaimedNest Continue should route to the claim_code page: "
        f"{app.driver.diagnose('claim-code-input')} error={app.error_text()!r}"
    )
    assert app.is_visible("claim-code-submit-button"), (
        "claim_code page should show the submit button: "
        f"{app.driver.diagnose('claim-code-submit-button')} error={app.error_text()!r}"
    )
    assert app.is_visible("claim-code-back-button"), (
        "claim_code page should show the back button: "
        f"{app.driver.diagnose('claim-code-back-button')} error={app.error_text()!r}"
    )


def test_claim_code_page_idle_state(app):
    """Idle state: input visible, submit disabled (input empty)."""
    set_claim_code_snapshot(app, {
        "state": "Idle",
        "message": {"key": "onboarding.claim_code.idle", "args": {}},
        "submit_enabled": False,
    })
    assert app.is_visible("claim-code-input"), (
        "claim_code idle state should render the code input: "
        f"{app.driver.diagnose('claim-code-input')} error={app.error_text()!r}"
    )
    assert app.is_visible("claim-code-status"), (
        "claim_code idle state should render the status line: "
        f"{app.driver.diagnose('claim-code-status')} error={app.error_text()!r}"
    )
    assert not app.is_enabled("claim-code-submit-button"), (
        "claim_code idle (empty input) should keep submit disabled: "
        f"{app.driver.diagnose('claim-code-submit-button')} "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_claim_code_invalid_renders_status_not_error_message(app):
    """Invalid code: claim-code-status carries the per-field error;
    the page-level error-message is NOT used (claim-code-status owns the
    code-specific feedback per ui.yaml)."""
    set_claim_code_snapshot(app, {
        "state": {"Invalid": {"reason": "code does not match any pending claim"}},
        "message": {
            "key": "onboarding.claim_code.invalid",
            "args": {"reason": "code does not match any pending claim"},
        },
        "submit_enabled": True,  # user can retry with a corrected code
    })
    status_text = app.get_text("claim-code-status")
    assert "code does not match" in status_text or "not match" in status_text.lower()
    # Submit re-enabled so user can retry.
    assert app.is_enabled("claim-code-submit-button"), (
        "Invalid code should re-enable submit so the user can retry: "
        f"{app.driver.diagnose('claim-code-submit-button')} "
        f"error={app.error_text()!r}"
    )


def test_claim_code_back_returns_to_handle_entry(app):
    """Back button on claim_code returns to handle_entry."""
    set_claim_code_snapshot(app, {
        "state": "Idle",
        "message": {"key": "onboarding.claim_code.idle", "args": {}},
        "submit_enabled": False,
    })
    assert app.is_visible("claim-code-back-button"), (
        "claim_code page should render the back button before clicking it: "
        f"{app.driver.diagnose('claim-code-back-button')} error={app.error_text()!r}"
    )
    app.click("claim-code-back-button")
    assert app.is_visible("handle-input")  # back on handle_entry


@pytest.mark.feature("claim-a-fresh-nest")
def test_claim_code_uri_paste_reaches_the_machine_intact(app):
    """A pasted ~90+ char `fauna://claim` URI must reach
    `wizard_submit_claim_code` intact — `claim-code-input` must apply no
    `maxLength`, format mask, or character filter that would truncate or
    garble it first. The other 6 apps were verified clean by direct source
    read; this is the one app's real widget the harness can drive.

    Uses the REAL `parse_claim_input` (via `navigate_to_claim_code_for_known_nest`
    + a genuine submit), not the injected-snapshot setters the rest of this
    module uses — the point is to prove the UI door, not just the paint.
    A malformed-but-64-char-shaped `nest=` tail is the proof: it only
    surfaces THIS specific "unrecognized claim input" refusal
    (`libs/fauna-core/src/claim_code.rs::parse_claim_input`) when the whole
    ~90+ char string, tail included, arrives at the parser — a truncated or
    filtered paste would instead fall back to a bare-code parse (no `nest=`
    ever seen) or garble the leading `fauna://claim` scheme, producing a
    different outcome (or none at all).
    """
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest",
        json.dumps(["https://nest.example.test", "alice@example.test"]),
    )
    app.driver.wait_for("claim-code-input", timeout=15)

    malformed_uri = "fauna://claim?code=k7q2-m9xj-aaaa-bbbb&nest=" + "g" * 64
    assert len(malformed_uri) > 90, "exercise a realistic full-length claim URI"
    app.driver.clear_and_type("claim-code-input", malformed_uri)
    app.driver.click("claim-code-submit-button")

    # Deadline poll (testing.md convention 14) — submit briefly flips to
    # Submitting before the parse-fail Invalid transition lands.
    status_deadline = time.monotonic() + 15.0
    status_text = ""
    while time.monotonic() < status_deadline:
        status_text = app.get_text("claim-code-status")
        if "unrecognized claim input" in status_text:
            break
        time.sleep(0.25)
    assert "unrecognized claim input" in status_text, (
        "an intact paste must reach the URI parse branch and refuse on the "
        f"malformed nest= tail, got claim-code-status={status_text!r} "
        f"error={app.error_text()!r}"
    )
    assert app.is_enabled("claim-code-submit-button"), (
        "the refusal must re-enable submit so the user can retry with a "
        "corrected paste — never a dead end"
    )


def test_claim_code_submitting_disables_submit(app):
    """Submitting state: button disabled while POST in flight."""
    set_claim_code_snapshot(app, {
        "state": "Submitting",
        "message": {"key": "onboarding.claim_code.submitting", "args": {}},
        "submit_enabled": False,
    })
    # Precondition: page must actually be rendered, not just missing.
    assert app.is_visible("claim-code-submit-button"), (
        "claim_code Submitting state should still render the submit button: "
        f"{app.driver.diagnose('claim-code-submit-button')} error={app.error_text()!r}"
    )
    assert not app.is_enabled("claim-code-submit-button"), (
        "claim_code Submitting state should keep submit disabled while in flight: "
        f"{app.driver.diagnose('claim-code-submit-button')} "
        f"error={app.error_text()!r}"
    )
