"""The nest refuses a NEW device past the tier's device cap, enrolls nothing,
and lets a device the account already holds re-register freely — on a real
``fauna-nest`` binary, over the WS-RPC door a client's sync daemon uses.

Owner doc: ``docs/goal/behavior/devices.md`` § Step 4 — Register for sync —
*"On a multi-tenant nest a **new** ``device_id`` past the caller's
``max_devices`` is refused with ``fauna.sync.device_limit_exceeded`` … nothing
is written. … a **re-register of a ``device_id`` the actor already holds** —
the row is an upsert, so re-labelling and every re-provision keep working at
the cap."*

What this adds over the two existing witnesses: the cargo
``bins/fauna-nest/tests/conformance_device_tier_cap.rs`` drives the handler
in-process, and the app journey ``tests/test_device_cap_refusal.py`` proves the
Devices page renders the refusal. Neither pins the NEST's verdict on the wire of
a real binary — so a green journey could sit over a nest that enrolled the
device and merely reported an error. Here the roster is read back after the
refusal.

⚠ A dedicated actor on a dedicated tier, never the shared ``free`` tier: the
session fixture lifts ``free``'s device cap (``test_device_cap_refusal.py``'s
module docstring), and moving a shared actor would leak into other tests.
"""

import os

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register, ensure_tier, set_user_tier

pytestmark = pytest.mark.tier_3

#: One slot, so a single register fills it and the second new id is the one
#: past the cap.
CAPPED_TIER = "e2e-api-one-device"


def _register(ws, device_id: str, label: str) -> dict:
    return ws.call(
        "fauna.sync.register",
        {"device_id": device_id, "label": label, "capabilities": "read,write"},
    )


def _roster(ws) -> set[str]:
    return {d["device_id"] for d in ws.call("fauna.sync.devices.list", {}).get("devices", [])}


@pytest.mark.feature("devices")
def test_a_new_device_past_the_tier_cap_is_refused_and_a_held_one_re_registers(nest_instance):
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    ensure_tier(port, admin_signing_key=admin_sk, name=CAPPED_TIER, base_url=url, max_devices=1)
    set_user_tier(port, actor["actor_id_hex"], CAPPED_TIER, admin_signing_key=admin_sk, base_url=url)

    held = os.urandom(32).hex()
    newcomer = os.urandom(32).hex()
    with WsRpcAdminClient(
        url, actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    ) as ws:
        # The tier's whole allowance registers.
        _register(ws, held, "Laptop")
        assert _roster(ws) == {held}

        # ── A NEW id past the cap: refused with the typed code… ──
        with pytest.raises(RpcCallError) as refused:
            _register(ws, newcomer, "Phone")
        assert refused.value.code == "fauna.sync.device_limit_exceeded", refused.value.code

        # …and nothing was written: a nest that enrolled the device and only
        # *reported* an error would pass the line above and fail here.
        assert _roster(ws) == {held}, "a refused register must enroll nothing"

        # ── The device already held re-registers at the cap (an upsert —
        # re-labelling and every re-provision must keep working). ──
        _register(ws, held, "Laptop, renamed")
        assert _roster(ws) == {held}, "a re-register is not a new slot"
