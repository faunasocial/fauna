"""Real invite-request submit over WS-RPC — the "join an existing nest" flow.

`test_invite_request_states.py` only *injects* invite-request snapshots; it never
drives the real submit. This test drives the linux app's `invite_request` page
against a **claimed** nest, so `wizard_submit_invite_request()` makes a real
`fauna.account.invite_request.submit` call over the pre-identity (anonymous)
WS-RPC connection — the path that had no real e2e before
an internal follow-up track S3b flipped onboarding from HTTP onto WS-RPC.

Tier 3 (real `fauna-nest`, real wire). The complementary in-process proof —
the same submit/recheck through the production `Arc<dyn NestApi>` wrapper — is
`bins/fauna-nest/tests/onboarding_ws_nest_api_roundtrip.rs`.
"""

import json
import time

import pytest
from nacl.signing import SigningKey

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("join-a-nest")
def test_invite_request_submit_reaches_pending_review(app, nest_instance):
    # A would-be joiner's identity, distinct from the nest's admin. Imported
    # through the real paste-secret onboarding UI.
    sk = SigningKey.generate()
    secret_hex = bytes(sk).hex()

    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=15)

    # Jump the wizard to the invite_request page for this claimed nest (the nest
    # can't be discovered by DNS in a test). Everything downstream is the real
    # client flow: the submit below is a real `invite_request.submit` over the
    # anonymous WS to `nest_instance`.
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_instance["url"], "bobjoiner"]),
    )

    app.driver.wait_for("invite-request-submit-button", timeout=20)
    app.driver.click("invite-request-submit-button")

    # A successful submit transitions the snapshot to PendingReview, which is the
    # only state that reveals the recheck affordance (an optional element).
    deadline = time.monotonic() + 20.0
    while time.monotonic() < deadline and not app.driver.is_visible(
        "invite-request-recheck-button"
    ):
        time.sleep(0.5)
    assert app.driver.is_visible("invite-request-recheck-button"), (
        "invite-request submit did not reach PendingReview (the real "
        f"invite_request.submit over WS failed). error: {app.error_text()!r}"
    )
