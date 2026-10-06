"""E2E coverage for the factory-reset re-onboard claim-code prefill.

The admin "Factory reset this nest" affordance (`admin-factory-reset-button` on
the `admin-nest` Danger zone — moved off admin-settings in the per-page-services
redesign, admin.md § N Nest) calls the Admin-gated `fauna.admin.factory_reset`.
The reply carries the post-reset claim code, which the **client** holds — the
human never sees it. So the re-onboard path must pre-fill that code into the
wizard's `claim-code-input`, or the admin lands on the claim-code page with
nothing to type. The shared `OnboardingMachine` carries the code via
`navigate_to_claim_code_for_known_nest_with_code`; the claim-code page pre-fills
its (empty) input from `claim_code_prefill()`.

Scope of this file (tier_2): the shared-machine → client-view prefill plumbing,
invoked directly via the test-agent bridge (no nest restart needed). The full
factory-reset → restart-wipe → re-claim round-trip through the real UI button is
covered by the believable live test `test_mail_enable_live_nest.py` (live nest,
env-gated). The Rust unit test
`fauna-onboarding-machine::machine::tests::navigate_to_claim_code_with_code_pre_loads_prefill`
pins the machine-side carry/clear semantics.

Per `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory reset and
`docs/goal/behavior/onboarding.md` §3a (Claim code).
"""

import json

import pytest

pytestmark = pytest.mark.tier_2


def _seed_and_navigate_with_code(app, nest_url: str, handle: str, code: str):
    """Seed identity, then invoke the factory-reset re-onboard navigate helper
    that carries the returned claim code. Mirrors what the linux factory-reset
    handler does on `FactoryResetComplete`."""
    secret_hex = "00" * 32
    app.driver.call_machine_method("seed_identity", json.dumps(secret_hex))
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest_with_code",
        json.dumps([nest_url, handle, code]),
    )


@pytest.mark.feature("factory-reset")
def test_factory_reset_navigate_prefills_claim_code(app):
    """`navigate_to_claim_code_for_known_nest_with_code` lands the wizard on the
    claim_code page with the returned code pre-filled — the leg the factory-reset
    affordance takes after `fauna.admin.factory_reset` returns the code to the
    client (the human never sees it)."""
    _seed_and_navigate_with_code(
        app,
        nest_url="https://nest.example.test",
        handle="alice@nest.example.test",
        code="A1B2C3",
    )
    assert app.is_visible("claim-code-input"), (
        "factory-reset navigate_with_code should land on the claim_code page: "
        f"{app.driver.diagnose('claim-code-input')} error={app.error_text()!r}"
    )
    assert app.is_visible("claim-code-submit-button"), (
        "claim_code page should show the submit button: "
        f"{app.driver.diagnose('claim-code-submit-button')} error={app.error_text()!r}"
    )
    # The input is pre-filled with the code the client holds — otherwise the
    # admin would be stranded with nothing to type.
    assert app.driver.get_text("claim-code-input") == "A1B2C3"
