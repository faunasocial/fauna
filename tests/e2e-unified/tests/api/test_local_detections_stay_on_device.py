"""tier_3 nest-tier witness: what a device flags in its user's encrypted messages
never reaches the nest (`moderation.md` § State & data shape — local detections
are held client-side, and the coarse `scan_report` beacon left the wire
2026-09-24 with its kind, its wire types and its handler).

Real ``fauna-nest`` binary + real WS-RPC; no app. The device half — an app
keeping its detections in its own `LocalDetectionStore` — is the app's to show;
this is the nest's half, observed from outside: the nest has no way to be told.
The one kind any client ever used to report detections is answered as a kind
the nest does not have, to a signed-in member, and nothing about it is kept.
"""

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("moderation-queue")
def test_the_nest_has_no_way_to_learn_what_your_device_flags(nest_instance):
    """`moderation-queue` outcome 10: what your device flags in your encrypted
    messages stays on your device; your nest never learns of it.

    A signed-in member's connection (an anonymous one is refused before the
    router, which would prove nothing about the router) sends the retired
    detection report, naming a category the way the last client that produced
    it did. The nest answers that it has no such kind, and the connection
    carries on. (No confidence value: a float is not canonical dag-cbor, and the
    nest drops the frame before it ever reads the kind.)
    """
    member = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    ws = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=member["actor_id_bytes"],
        signing_key=bytes(member["signing_key"]),
    )
    with ws:
        with pytest.raises(RpcCallError) as excinfo:
            ws.call(
                "fauna.moderation.scan_report",
                {"labels": [{"category": "spam"}]},
            )
        assert excinfo.value.code == "fauna.protocol.unknown_kind", (
            f"the nest must have no kind a device could report its local detections "
            f"through; got {excinfo.value.code!r} (details={excinfo.value.details!r})"
        )
        # The refusal is the router's, not a dropped connection: the member's
        # own moderation stats still answer on it.
        assert isinstance(ws.call("fauna.moderation.stats", {}), dict)
