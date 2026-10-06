"""Tier-3 API isolation: a registered sync device surfaces in the WS-RPC device
list for the same actor.

This is the API-side half of the cross-app `tests/test_device_cards.py`,
which registers a device then reads the list back through each app's
`DevicesMachine` renderer. By driving the nest directly (no client driver), it
pins *which layer* a "No devices registered persists after a real register"
failure lives in:

* this test GREEN → the nest register→list path is sound; a client still showing
  the empty state is a client-side (renderer / connection / refresh) bug.
* this test RED   → the nest / shared path is broken for **every** client.

Authored 2026-06-14 while disambiguating a macOS `test_device_cards` red. It
turned out the cross-app test's `_register_test_device` was POSTing to the
**deleted** HTTP twin `POST /api/v1/sync/register` (the sync control plane —
register / changes / status / files / devices / backup-status — moved to
`fauna.sync.*` WS-RPC in the WS-RPC-everywhere rip-out; only the byte-download
route survives, see `bins/fauna-nest/src/sync_routes.rs`). The dead POST lands on
the SPA `.fallback` → 200, so registration silently no-ops and the list is empty
on every app. Registration must ride the WS-RPC kind, exactly as
`test_devices_conflicts.py` already reports conflicts over `fauna.sync.*`.
`docs/goal/ui/devices.md` § Where logic lives (page-level device list →
`fauna.sync.devices.list`; registration → `fauna.sync.register`).
"""
import os

import pytest

from common import create_actor_and_register, port_base_url
from common.auth import PLACE_ORIGINATES_ONLY, add_folder_member, user_create_folder
from clients.ws_rpc_admin_client import WsRpcAdminClient
from fauna_ffi import open_device_label, seal_device_label

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("devices")
def test_registered_device_appears_in_ws_rpc_list(two_nodes):
    """Register a device over `fauna.sync.register`, then list it back over
    `fauna.sync.devices.list` as the same actor (the production-faithful path —
    a client's sync daemon registers over WS-RPC, not the retired HTTP twin)."""
    port = two_nodes["port_a"]
    nest_url = port_base_url(port)
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    secret = bytes(actor["signing_key"])
    device_id = os.urandom(32).hex()
    label_sealed = seal_device_label(secret, bytes.fromhex(device_id), "my-linux-box")
    assert label_sealed, "a user-chosen label must seal — only the three synthetic ones return None"
    with WsRpcAdminClient(
        nest_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=secret,
    ) as client:
        client.call(
            "fauna.sync.register",
            {
                "device_id": device_id,
                "label": "my-linux-box",
                "capabilities": "read,write",
                "label_sealed": label_sealed,
            },
        )
        reply = client.call("fauna.sync.devices.list", {})

    devices = reply.get("devices", [])
    assert any(d.get("device_id") == device_id for d in devices), (
        f"registered device {device_id} (my-linux-box) not in WS-RPC "
        f"devices.list reply: {devices!r}"
    )
    registered = next(d for d in devices if d.get("device_id") == device_id)
    # Post-S9-flip the wire row's plaintext `label` is `''` for a user-chosen
    # label — the nest scrubs it (`register_sync_device`) and the name lives only
    # in `label_sealed`. So the assertion is the RENDER, through the same
    # `render_device_label` seam `DevicesMachine` uses under the same owner-only
    # custody: what this opens is by construction what the owner's app shows.
    # Asserting `registered["label"]` directly would now be asserting the scrub.
    assert registered.get("label_sealed"), (
        f"the register's seal did not ride the wire back: {registered!r}"
    )
    rendered = open_device_label(
        secret,
        bytes.fromhex(device_id),
        registered.get("label_sealed"),
        registered.get("label", "") or "",
    )
    assert rendered == "my-linux-box", f"label drift: rendered {rendered!r} from {registered!r}"


@pytest.mark.feature("devices")
def test_device_folder_role_appears_in_ws_rpc_list(two_nodes):
    """DEBUG isolation for row 231 (`device-folder-role-badge`): register a
    device, create a folder the same actor owns, seat the device as an
    originates-only place, then confirm `fauna.sync.devices.list` reports it in
    `folders` — the server-side half of `test_device_cards.py`'s
    `test_device_fileset_role_badge_shows_role_chip`, isolating whether a
    render failure is server-side or client-side."""
    port = two_nodes["port_a"]
    nest_url = port_base_url(port)
    admin_sk = two_nodes["admin_sk_a"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk)

    secret = bytes(actor["signing_key"])
    device_id = os.urandom(32).hex()
    with WsRpcAdminClient(
        nest_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=secret,
    ) as client:
        client.call(
            "fauna.sync.register",
            {"device_id": device_id, "label": "role-chip-device", "capabilities": "read,write"},
        )

    set_name = "role-chip-api-set"
    secret_key = secret.hex()
    user_create_folder(port, set_name, secret_key=secret_key)
    add_folder_member(
        port, set_name, device_id, PLACE_ORIGINATES_ONLY,
        admin_signing_key=admin_sk, actor_id=actor["actor_id_hex"],
    )

    with WsRpcAdminClient(
        nest_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=secret,
    ) as client:
        reply = client.call("fauna.sync.devices.list", {})

    devices = reply.get("devices", [])
    registered = next((d for d in devices if d.get("device_id") == device_id), None)
    assert registered is not None, f"registered device {device_id} not in list: {devices!r}"
    folders = registered.get("folders", [])
    assert folders, (
        f"device {device_id} has no folders after add_member: {registered!r}"
    )
    assert folders[0].get("flags") == {
        "originates": True, "accepts": False, "applies_deletes": False,
    }, f"unexpected place flags: {folders!r}"
    assert "role" not in folders[0], f"the role contraction retired `role`: {folders!r}"
    assert folders[0].get("name") == set_name, f"unexpected folder name: {folders!r}"
