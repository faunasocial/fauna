"""Nostr — a zap signer you named can always be removed, even when your plan
no longer lets you add one.

`docs/goal/ui/nostr.md` § Layout & flow item 7 (c): `add` is gated by the
`zaps.signer.designate` surface (`docs/goal/architecture/dynamic-features.md`
§ per-surface composition) and `remove` is not — removal is de-escalation, and
a tier that can only tighten must never trap a user in a roster they cannot
undo. The outcome is the catalog's `nostr` 23.

The journey: designate a signer while zaps are allowed, then the nest admin
turns `zaps` off. Back on the page, the add button reads the refusal (disabled
— the courtesy layer, so the user never meets a guaranteed failure) while the
row's remove button stays live, and pressing it empties the roster.

The admin's deny is written through `fauna.features.policy.update` directly:
it arranges the precondition (`testing.md` point 8's fixture-setup
carve-out); the admin authoring journey is `test_feature_limits.py`'s. The
policy is cleared in `finally`, so the session nest is left as found.

tier_3: a real `fauna-nest` binary. tui first, the lead app; the other apps
gain their markers in the cross-app lift.
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
]

SIGNER = "cd" * 32


def _set_zaps_policy(nest_instance, policy: dict | None) -> None:
    admin = nest_instance["admin"]
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        payload: dict = {"feature": "zaps"}
        if policy is not None:
            payload["policy"] = policy
        client.call("fauna.features.policy.update", payload)


@pytest.mark.feature("nostr")
def test_a_named_zap_signer_stays_removable_when_zaps_are_turned_off(
    logged_in_app, nest_instance,
):
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    assert app.nostr.wait_for_zap_signer_section(), (
        f"Zap signers section should render: "
        f"{app.driver.diagnose('nostr-zap-signer-add-btn')}"
    )
    app.nostr.clear_zap_signers()
    assert app.nostr.wait_for_zap_signer_count(0), "roster should start empty"
    app.nostr.add_zap_signer(SIGNER, label="wallet I used to trust")
    assert app.nostr.wait_for_zap_signer_count(1), (
        f"designating a signer should create one roster row. "
        f"error: {app.nostr.page_error_text()!r}"
    )

    _set_zaps_policy(nest_instance, {"availability": "deny"})
    try:
        # Re-enter the page so it re-reads the feature plane.
        app.driver.navigate_to("feed")
        app.nostr.navigate()
        assert app.nostr.wait_for_zap_signer_section(), (
            f"the section must still render under a deny: "
            f"{app.driver.diagnose('nostr-zap-signer-add-btn')}"
        )
        wait_until(
            lambda: not app.driver.is_enabled("nostr-zap-signer-add-btn"),
            20.0,
            diagnose=lambda: (
                "with zaps turned off, designating must read as refused "
                "(disabled), not as a live button bound to fail: "
                f"{app.driver.diagnose('nostr-zap-signer-add-btn')}"
            ),
        )
        assert app.nostr.zap_signer_count() == 1, (
            "the signer named before the deny must still be listed"
        )
        assert app.driver.is_enabled("nostr-zap-signer-remove"), (
            "removing a signer is de-escalation and must never be gated: "
            f"{app.driver.diagnose('nostr-zap-signer-remove')}"
        )
        app.nostr.remove_zap_signer(0)
        assert app.nostr.wait_for_zap_signer_count(0), (
            f"remove must work under the deny and empty the roster. "
            f"error: {app.nostr.page_error_text()!r}"
        )
        assert app.nostr.wait_for_zap_signer_empty_state(True), (
            "the emptied roster must state that nothing is believed"
        )
    finally:
        _set_zaps_policy(nest_instance, None)
        app.nostr.clear_zap_signers()
