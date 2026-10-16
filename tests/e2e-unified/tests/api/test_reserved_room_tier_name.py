"""The tier name "room" is kept for posts addressed to a room: a creator cannot
name a tier of their own that.

Owner doc: ``docs/goal/behavior/restricted-posts.md`` § Encryption at rest
(*Room-restricted — the ruling*, ruling 3): every room-restricted post carries
the reserved tier ``room``; a subscribable tier of that name would put a tier an
app could offer on a post whose readers are a room's floor, never a
subscription. Catalog outcome ``subscriptions-and-tiers`` 17.

Observed from outside the nest, over the wire a creator's app rides
(``fauna.subscriptions.tiers.create``): the refusal, the nest still holding no
such tier afterwards, and — as the control that makes the refusal a statement
about the NAME and nothing else — the same request under another name landing.
"""

import pytest

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register

pytestmark = pytest.mark.tier_3


def _create_tier(ws: WsRpcAdminClient, author: dict, name: str) -> dict:
    return ws.call(
        "fauna.subscriptions.tiers.create",
        {
            "name": name,
            "rank": 2,
            "description": None,
            "price_hint": "5 EUR / month",
            "payment_url": "https://pay.example/tier",
            "auto_approve": False,
            # The required birth KeyBlob (empty roster).
            "encrypted_upload": fauna_ffi.build_tier_birth_upload(
                bytes(author["signing_key"]), name, bytes(range(32))
            ),
        },
    )


def _tier_names(ws: WsRpcAdminClient) -> set[str]:
    reply = ws.call("fauna.subscriptions.tiers.list", {})
    return {t["name"] for t in reply["tiers"]}


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_creator_cannot_name_a_tier_room(nest_instance):
    nest = nest_instance
    author = create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )
    with WsRpcAdminClient(
        nest["url"],
        actor_id=author["actor_id_bytes"],
        signing_key=bytes(author["signing_key"]),
    ) as ws:
        with pytest.raises(RpcCallError) as refused:
            _create_tier(ws, author, "room")
        assert "room" in str(refused.value).lower() or "reserved" in str(refused.value).lower(), (
            f"the refusal does not say why: {refused.value}"
        )
        assert "room" not in _tier_names(ws), "the nest kept a tier named room"

        # The control: the same request under an ordinary name is accepted, so
        # the refusal above is about the name and nothing else.
        _create_tier(ws, author, "gold")
        assert _tier_names(ws) >= {"gold"}
        assert "room" not in _tier_names(ws)
