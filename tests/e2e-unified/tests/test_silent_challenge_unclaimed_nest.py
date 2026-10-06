"""E2E coverage for the silent-challenge unclaimed-nest fork.

When the launch flow's silent challenge against a saved nest_url returns
404 on /verify, it probes the `fauna.setup.status` WS-RPC kind and routes the wizard to:
  - claim_code  if `claimed: false` (unclaimed nest — user must claim)
  - invite_request  otherwise (claimed nest, or any reachability failure
    — safer default; user can ask the admin for an invite)

Per `docs/goal/behavior/onboarding.md` § App-launch routing — silent-challenge
fallback table.

Scope of this file: the per-app navigation glue. Each app's launch
flow lands on `LaunchPhase::WizardAt(ClaimCode)` or
`LaunchPhase::WizardAt(InviteRequest)` and dispatches to the respective
navigate helper on the OnboardingMachine. We invoke those helpers
directly via the test-agent bridge and assert the right page renders.

Out of scope (deferred to a separate follow-up TODO that needs
long-term-store seeding infrastructure):
  - End-to-end "saved identity + saved nest URL → real silent challenge
    → setup-status probe → routed page" — Linux needs a libsecret seed
    helper; web needs a parametrizable SPA fixture pointing at an
    unclaimed-nest. The Rust unit tests in
    `libs/fauna-launch-machine/tests/silent_challenge.rs` cover the
    routing decision exhaustively for all three setup-status outcomes.
"""

import json

import pytest

pytestmark = pytest.mark.tier_2


def _seed_and_navigate_to_claim_code(app, nest_url: str, handle: str):
    """Seed identity + step the wizard, then invoke the launch-flow
    navigate-to-claim-code helper. Mirrors what the per-app launch
    glue does on `LaunchPhase::WizardAt(ClaimCode)`."""
    secret_hex = "00" * 32
    app.driver.call_machine_method("seed_identity", json.dumps(secret_hex))
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest",
        json.dumps([nest_url, handle]),
    )


def _seed_and_navigate_to_invite_request(app, nest_url: str, handle: str):
    """Seed identity + invoke the launch-flow navigate-to-invite-request
    helper. Mirrors what the per-app launch glue does on
    `LaunchPhase::WizardAt(InviteRequest)`."""
    secret_hex = "00" * 32
    app.driver.call_machine_method("seed_identity", json.dumps(secret_hex))
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_url, handle]),
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_launch_flow_navigate_to_claim_code_renders_page(app):
    """`navigate_to_claim_code_for_known_nest` lands the wizard on the
    claim_code page — the launch-flow leg taken when /verify returns 404
    AND setup-status reports claimed=false."""
    _seed_and_navigate_to_claim_code(
        app,
        nest_url="https://nest.example.test",
        handle="alice@nest.example.test",
    )
    assert app.is_visible("claim-code-input"), (
        "launch-flow navigate_to_claim_code should land on the claim_code page: "
        f"{app.driver.diagnose('claim-code-input')} error={app.error_text()!r}"
    )
    assert app.is_visible("claim-code-submit-button"), (
        "claim_code page should show the submit button: "
        f"{app.driver.diagnose('claim-code-submit-button')} error={app.error_text()!r}"
    )


def test_launch_flow_navigate_to_invite_request_renders_page(app):
    """`navigate_to_invite_request_for_known_nest` lands the wizard on
    the invite_request page — the launch-flow leg taken when /verify
    returns 404 AND setup-status reports claimed=true (or any
    reachability failure → safer default)."""
    _seed_and_navigate_to_invite_request(
        app,
        nest_url="https://nest.example.test",
        handle="alice@nest.example.test",
    )
    assert app.is_visible("invite-request-submit-button"), (
        "launch-flow navigate_to_invite_request should land on the invite_request page: "
        f"{app.driver.diagnose('invite-request-submit-button')} error={app.error_text()!r}"
    )
