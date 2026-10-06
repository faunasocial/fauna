"""E2E tests for the push-subscription WS-RPC kinds (`fauna.push.*`).

The legacy HTTP twins (`GET /api/v1/push/vapid-key`, `POST /api/v1/push/subscribe`,
`DELETE /api/v1/push/subscribe/{device_id}`) were ripped in the WS-RPC-everywhere
cut; the canonical surface is now `fauna.push.{vapid_key,subscribe,unsubscribe}`
(`bins/fauna-nest/src/push_handlers.rs`, gated `User | Admin`; web migrated). These tests drive a User-class actor over `WsRpcAdminClient` — the
generic WS-RPC client (the "Admin" name is historical; a User keypair makes it a
User-class caller).
"""

from common import create_actor_and_register, port_base_url
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

import pytest

pytestmark = pytest.mark.tier_3


def _user_client(port: int, actor: dict) -> WsRpcAdminClient:
    """A User-class WS-RPC client for ``actor`` against the loopback nest."""
    return WsRpcAdminClient(
        port_base_url(port),
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


@pytest.mark.feature("notifications")
def test_push_vapid_key_self_generated_by_default(two_nodes):
    """`fauna.push.vapid_key` succeeds with no `--vapid-pem` flag.

    Web Push is nest infrastructure, not an operator choice
    (`docs/goal/architecture/apps/common.md` § Push Notifications): the nest
    generates and persists its own VAPID keypair on first boot
    (`ensure_vapid_pem`, `bins/fauna-nest/src/push.rs`), so `push_service` is
    always configured. The test nest (started by `start_nest` with only
    `--bind`/`--config`/`--blob-dir`, no `--vapid-pem`) proves the out-of-the-box
    path, matching the "works out-of-the-box" product invariant.

    (Replaces the prior `test_push_vapid_key_unconfigured`, which pinned the
    pre-auto-generation "off unless `--vapid-pem` is passed" contract — a
    config-surface invariant repair: no user or admin ever chooses a VAPID
    key, so requiring one via CLI flag was configuration-file theatre.)
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        reply = c.call("fauna.push.vapid_key", {})
    public_key = reply["public_key"]
    # Uncompressed P-256 public key is 65 bytes; base64url without padding
    # encodes 65 bytes as ceil(65*4/3) = 87 characters (mirrors the Rust unit
    # test `push_service_parses_vapid_key`).
    assert len(public_key) == 87, (
        f"base64url-encoded uncompressed P-256 public key must be 87 chars, "
        f"got {len(public_key)}: {public_key!r}"
    )
    assert all(c.isalnum() or c in "-_" for c in public_key), (
        f"public key must be valid base64url: {public_key!r}"
    )


@pytest.mark.feature("notifications")
def test_push_subscribe_unsubscribe(two_nodes):
    """Subscribe (web-push), re-subscribe (upsert), then unsubscribe (idempotent)."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        # Subscribe
        reply = c.call("fauna.push.subscribe", {
            "device_id": "test-device-001",
            "transport": "web-push",
            "endpoint": "https://fcm.googleapis.com/fcm/send/test-endpoint",
            "key_p256dh": "BNxfi3mA1reEQOV_aJF8m0wOBNw5u-rY",  # gitleaks:allow
            "key_auth": "tBHI_test_auth_key",
        })
        assert reply["ok"] is True

        # Subscribe again with a new endpoint (upsert — should not error)
        reply = c.call("fauna.push.subscribe", {
            "device_id": "test-device-001",
            "transport": "web-push",
            "endpoint": "https://fcm.googleapis.com/fcm/send/updated-endpoint",
            "key_p256dh": "BNxfi3mA1reEQOV_aJF8m0wOBNw5u-rY",  # gitleaks:allow
            "key_auth": "tBHI_test_auth_key",
        })
        assert reply["ok"] is True

        # Unsubscribe
        reply = c.call("fauna.push.unsubscribe", {"device_id": "test-device-001"})
        assert reply["ok"] is True

        # Unsubscribe again — delete is idempotent, still ok
        reply = c.call("fauna.push.unsubscribe", {"device_id": "test-device-001"})
        assert reply["ok"] is True


@pytest.mark.feature("notifications")
def test_push_subscribe_apns(two_nodes):
    """Subscribe with transport=apns for an iOS device token (no web-push keys)."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        reply = c.call("fauna.push.subscribe", {
            "device_id": "ios-device-001",
            "transport": "apns",
            "endpoint": "a1f1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1",
        })
        assert reply["ok"] is True

        reply = c.call("fauna.push.unsubscribe", {"device_id": "ios-device-001"})
        assert reply["ok"] is True


def test_push_subscribe_default_transport(two_nodes):
    """Subscribe without a transport field defaults to web-push and succeeds."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        reply = c.call("fauna.push.subscribe", {
            "device_id": "default-transport-device",
            "endpoint": "https://fcm.googleapis.com/fcm/send/test-default",
            "key_p256dh": "BNxfi3mA1reEQOV_aJF8m0wOBNw5u-rY",  # gitleaks:allow
            "key_auth": "tBHI_test_auth_key",
        })
        assert reply["ok"] is True


def test_push_subscribe_rejects_unknown_transport(two_nodes):
    """Subscribe with an unknown transport is rejected (`fauna.push.invalid_request`)."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        with pytest.raises(RpcCallError) as excinfo:
            c.call("fauna.push.subscribe", {
                "device_id": "bad-transport-device",
                "transport": "carrier-pigeon",
                "endpoint": "https://example.com/push",
                "key_p256dh": "BNxfi3mA1reEQOV_aJF8m0wOBNw5u-rY",  # gitleaks:allow
                "key_auth": "tBHI_test_auth_key",
            })
    assert excinfo.value.code == "fauna.push.invalid_request"


@pytest.mark.feature("notifications")
@pytest.mark.parametrize(
    ("transport", "endpoint"),
    [
        ("web-push", "http://fcm.googleapis.com/fcm/send/plain-http"),
        ("web-push", "https://10.0.0.7/push/private-range"),
        ("web-push", "http://192.168.1.10:8080/push/private-range"),
        ("web-push", "https://169.254.169.254/latest/meta-data/"),
        ("web-push", "https://localhost/push/by-name"),
        ("web-push", "fcm.googleapis.com/fcm/send/not-a-url"),
        ("apns", "../../3/device/not-a-token"),
        ("apns", "a]f1b2c3"),
    ],
)
def test_push_subscribe_refuses_an_endpoint_the_nest_must_never_dial(two_nodes, transport, endpoint):
    """An endpoint is a destination the nest will dial on the subscriber's
    say-so, so a web-push endpoint must be https on a public address and an
    apns endpoint must be a hex device token (common.md § Registration).

    Every case here is refused by ANY build of the nest. The one shape a
    ``test-hooks`` build admits and a shipped build refuses — a loopback IP
    literal, which ``test_push_dispatch.py`` needs — is pinned in Rust instead
    (``push.rs`` ``subscribe_refuses_an_undialable_endpoint_in_a_shipped_build``),
    where the shipped posture is assertable from a test build.
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])
    with _user_client(port, actor) as c:
        with pytest.raises(RpcCallError) as excinfo:
            c.call("fauna.push.subscribe", {
                "device_id": "undialable-endpoint-device",
                "transport": transport,
                "endpoint": endpoint,
                "key_p256dh": "BNxfi3mA1reEQOV_aJF8m0wOBNw5u-rY",  # gitleaks:allow
                "key_auth": "tBHI_test_auth_key",
            })
    assert excinfo.value.code == "fauna.push.invalid_request", excinfo.value


# NOTE: `test_push_preferences` (GET|PUT /api/v1/push/settings) was deleted —
# it covered a dead feature. Per-channel push preferences were a half-built
# parallel-session artifact: the route mounting was added
# referencing `push_routes::get_settings`/`put_settings`, and removed in that
# same session's own dedup commit ("resolve duplicate code from
# parallel push notification sessions") which deferred to the other session's
# already-merged implementation — which never had settings. The
# only surviving artifact is a vestigial `push_preferences` DB table
# (`db/migrations.rs`) that no code reads or writes. There is no HTTP route, no
# `fauna.push.{settings,preferences}` WS-RPC kind, no client UI, and no spec:
# `docs/goal/behavior/notifications.md` describes the in-app notification feed,
# not per-channel push toggles. If push preferences are ever re-introduced they
# should land as a `fauna.push.*` WS-RPC kind with a goal-doc spec first.
