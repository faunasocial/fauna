"""The private audience an archive import seals only-you posts under is never
offered to anyone, subscribing to it is refused as if it did not exist, and
not even a subscriber to the author's highest tier is ever granted it.

Owner docs: ``docs/goal/behavior/monetization.md`` § The unifying model — *A
tier may be hidden* (hidden means not offered AND not subscribable, the refusal
the same a nonexistent tier gets; the reserved tier alone holds the top rank);
``docs/goal/behavior/archive-import.md`` § What each category becomes →
*Audience mapping* (the import mints the reserved owner-only tier ``only-me``,
hidden, for only-you, custom and unknown originals). Catalog outcomes
``import-your-social-archive`` 47 and 48.

The tier is created the way the import machine creates it
(``fauna_client_subscriptions::orchestration``, the owner-only arm): the
reserved name at the reserved rank, hidden, never auto-approving, with its
birth KeyBlob. Everything after is what a stranger or a subscriber sees over
the wire: the author's offers, the answer to a subscribe — compared, byte for
byte in its error code, with the answer for a tier name the author never
created — and, for a granted subscriber, which KeyBlobs the nest serves them.
"""

import time

import pytest

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register

pytestmark = pytest.mark.tier_3

#: `fauna_core::subscription::OWNER_ONLY_TIER` / `OWNER_ONLY_TIER_RANK`.
ONLY_ME = "only-me"
ONLY_ME_RANK = 2**32 - 1


def _user(nest) -> dict:
    return create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
    )


def _ws(nest, user) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=user["actor_id_bytes"], signing_key=bytes(user["signing_key"])
    )


def _create_tier(ws, author, name: str, rank: int, *, hidden: bool) -> None:
    ws.call(
        "fauna.subscriptions.tiers.create",
        {
            "name": name,
            "rank": rank,
            "description": None,
            "price_hint": None,
            "payment_url": None,
            "auto_approve": False,
            "hidden": hidden,
            "encrypted_upload": fauna_ffi.build_tier_birth_upload(
                bytes(author["signing_key"]), name, bytes(range(32))
            ),
        },
    )


def _refusal(ws, author_id: bytes, tier: str) -> str:
    with pytest.raises(RpcCallError) as refused:
        ws.call("fauna.subscriptions.subscribe", {"author_id": author_id, "tier": tier})
    return refused.value.code


@pytest.mark.feature("import-your-social-archive")
def test_the_only_you_audience_is_never_offered_and_cannot_be_subscribed(nest_instance):
    nest = nest_instance
    author = _user(nest)
    stranger = _user(nest)
    author_id = author["actor_id_bytes"]

    with _ws(nest, author) as ws:
        _create_tier(ws, author, ONLY_ME, ONLY_ME_RANK, hidden=True)
        # The control: an ordinary tier the same author offers.
        _create_tier(ws, author, "gold", 2, hidden=False)
        own = {t["name"]: t for t in ws.call("fauna.subscriptions.tiers.list", {})["tiers"]}
        assert own.get(ONLY_ME, {}).get("hidden") is True, (
            f"the author's own list carries the reserved tier, hidden: {own!r}"
        )

    with _ws(nest, stranger) as ws:
        offered = {
            t["name"]
            for t in ws.call("fauna.subscriptions.offers.list", {"author_id": author_id})["tiers"]
        }
        assert "gold" in offered, f"the author's ordinary tier is offered: {offered!r}"
        assert ONLY_ME not in offered, "the only-you audience is never offered"

        hidden_code = _refusal(ws, author_id, ONLY_ME)
        missing_code = _refusal(ws, author_id, "a-tier-that-never-existed")
        assert hidden_code == "fauna.subscriptions.tier_not_found", hidden_code
        assert hidden_code == missing_code, (
            "subscribing to the only-you audience must answer exactly as a tier "
            f"that does not exist: {hidden_code!r} vs {missing_code!r}"
        )
        status = ws.call("fauna.subscriptions.status.get", {"author_id": author_id})
        assert status.get("tier") != ONLY_ME, f"no subscription to it was recorded: {status!r}"

    with _ws(nest, author) as ws:
        pending = ws.call("fauna.subscriptions.requests.list", {}).get("requests", [])
        assert not [r for r in pending if r.get("tier_name") == ONLY_ME], (
            f"no request the author could approve by accident: {pending!r}"
        )


@pytest.mark.feature("import-your-social-archive")
def test_a_subscriber_to_the_highest_tier_never_opens_an_only_you_import(nest_instance):
    """Outcome 48: a subscriber to the author's highest tier is granted that
    tier and never the only-you audience. The reserved tier sits alone at the
    top of the rank range — creating any other tier there is refused — so the
    highest tier anyone can hold is the one just below it; the grant through
    the approve door lands on that tier, and the subscriber is still refused
    the only-you KeyBlob, is on no only-you roster, and no request was queued
    that could ever put them there."""
    nest = nest_instance
    author = _user(nest)
    subscriber = _user(nest)
    author_id = author["actor_id_bytes"]
    top_key = bytes(range(1, 33))

    with _ws(nest, author) as ws:
        _create_tier(ws, author, ONLY_ME, ONLY_ME_RANK, hidden=True)
        with pytest.raises(RpcCallError):
            _create_tier(ws, author, "patron-at-the-top", ONLY_ME_RANK, hidden=False)
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": "patron",
                "rank": ONLY_ME_RANK - 1,
                "description": None,
                "price_hint": None,
                "payment_url": None,
                "auto_approve": False,
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(
                    bytes(author["signing_key"]), "patron", top_key
                ),
            },
        )

    with _ws(nest, subscriber) as ws:
        ws.call("fauna.subscriptions.subscribe", {"author_id": author_id, "tier": "patron"})

    with _ws(nest, author) as ws:
        pending = [
            r
            for r in ws.call("fauna.subscriptions.requests.list", {})["requests"]
            if r.get("tier_name") == "patron" and r.get("kind") == "subscribe"
        ]
        assert len(pending) == 1, pending
        upload = fauna_ffi.build_tier_roster_upload(
            bytes(author["signing_key"]),
            "patron",
            top_key,
            time.time_ns() // 1000,
            [subscriber["actor_id_bytes"]],
        )
        ws.call(
            "fauna.subscriptions.requests.approve",
            {"request_id": pending[0]["request_id"], "encrypted_upload": upload},
        )
        roster = ws.call("fauna.subscriptions.subscribers.list", {"tier_name": ONLY_ME})
        assert roster.get("subscribers", []) == [], f"nobody is on the only-you roster: {roster!r}"
        queued = [
            r
            for r in ws.call("fauna.subscriptions.requests.list", {})["requests"]
            if r.get("tier_name") == ONLY_ME
        ]
        assert queued == [], f"no request could ever put a subscriber there: {queued!r}"

    with _ws(nest, subscriber) as ws:
        held = ws.call(
            "fauna.subscriptions.key_blob.get", {"author_id": author_id, "tier_name": "patron"}
        )
        assert held.get("blob_data"), "the subscriber holds the highest tier's KeyBlob"
        with pytest.raises(RpcCallError) as refused:
            ws.call(
                "fauna.subscriptions.key_blob.get", {"author_id": author_id, "tier_name": ONLY_ME}
            )
        assert refused.value.code == "fauna.subscriptions.not_subscribed", refused.value.code
