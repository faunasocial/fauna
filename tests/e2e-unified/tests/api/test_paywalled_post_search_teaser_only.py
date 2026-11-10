"""Search on a nest finds a paywalled post by the words of its teaser and never
by the words behind the paywall. ``nest/network-exposure.md`` § Rulings
owns the ruling: the full-text search corpus is fed only ``Post::body_text()``,
which for a gated post is the public preview the paywall already renders to
anyone, and content derived from the sealed body never enters the index. The
teaser being searchable is intended. On a real ``fauna-nest`` binary.

The test makes the nest read the gated body before it searches: the creator
web-publishes the post and grants the web-serve holder the tier's period key,
so the nest renders the full page into its sealed class (shown by
``fauna.web.paywall.mint_token`` succeeding, which it refuses while no sealed
page exists). A body word that never reached the nest would make the negative
trivial. Three signed-in callers search: someone outside the tier, a member of
the tier (a claim code redeemed and the grant minted the way the creator's app
does it), and the creator. ``fauna.search.query`` is User-class only, so there
is no anonymous search to walk; the public corpus is the same for every
caller, which is what the three agreeing shows.

Convention 14: the negative is asserted only after the positive control, the
teaser word, has been found by the same caller on the same corpus.

Witness of ``paid-posts-and-tips`` outcome 21.
"""

import os
import secrets
import time

import pytest

import fauna_ffi
from actions.api_actor import ApiActor
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.blob_upload import SEALED_MIME, upload_blob
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from tests.api import ws_api

pytestmark = pytest.mark.tier_3


def _actor(nest: dict) -> tuple[dict, ApiActor]:
    user = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
    )
    return user, ApiActor(
        nest["url"], user["token"], user["actor_id_hex"], bytes(user["signing_key"])
    )


def _ws(nest: dict, user: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=user["actor_id_bytes"], signing_key=bytes(user["signing_key"])
    )


def _hits(nest: dict, user: dict, word: str) -> list[dict]:
    return ws_api.search(nest["port"], user, word)


def _naming(hits: list[dict], word: str) -> list[dict]:
    """The hits whose snippet quotes ``word``. A hit's ``content_id`` is the
    opaque corpus key, not the post id, so the snippet is what names the post."""
    return [h for h in hits if word in (h.get("snippet") or "")]


@pytest.mark.feature("paid-posts-and-tips")
def test_search_finds_a_paywalled_post_by_its_teaser_and_never_by_its_gated_body(
    nest_instance,
):
    nest = nest_instance
    port = nest["port"]
    teaser_word = f"teaserword{secrets.token_hex(6)}"
    body_word = f"gatedword{secrets.token_hex(6)}"
    tier = f"search-gold-{secrets.token_hex(3)}"
    slug = f"search-premium-{secrets.token_hex(3)}"

    # ── The creator's tier and gated post: the teaser carries one word, the
    # sealed body another. ──
    creator, creator_actor = _actor(nest)
    creator_id = bytes(creator["actor_id_bytes"])
    creator_secret = bytes(creator["signing_key"])
    reply, period_key = creator_actor.subscription_create_tier_with_key(
        tier, rank=1, price_hint="5 EUR / month"
    )
    assert reply.get("created") is True, f"tier create failed: {reply!r}"

    post_bytes, encrypted_blob = fauna_ffi.build_gated_post(
        creator_secret,
        f"Search Teaser\n\nThe public teaser says {teaser_word} once.",
        f"Search Teaser\n\nBehind the paywall it says {body_word} once.",
        tier,
        1,
        os.urandom(32),
        period_key,
    )
    upload_blob(
        port, creator["token"], encrypted_blob,
        audience_class="PeriodRestrictedPost", mime=SEALED_MIME,
    )
    post_id = ws_api.create_post(port, creator, post_bytes)

    # ── The nest reads the gated body: web-published, the web-serve holder
    # granted the period key, the sealed full page rendered. ──
    now = int(time.time())
    with _ws(nest, creator) as ws:
        ws.call("fauna.web.publish.set", {"post_id": bytes.fromhex(post_id), "slug": slug})
        holder = ws.call(
            "fauna.bridges.fetch_bridge_pubkey",
            {"bridge_role": "content-processor", "bridge_id": "web-serve"},
        )
        grant_blob = fauna_ffi.build_post_grant(
            creator_id, os.urandom(16), bytes(holder["x25519_pubkey"]),
            bytes(holder["mlkem_ek"]) if holder.get("mlkem_ek") else None,
            now, now + 3600, tier, period_key,
        )
        minted = ws.call("fauna.capabilities.mint", {"grant_blob": grant_blob})
        assert minted.get("ok") is True, minted
        token = ws.call("fauna.web.paywall.mint_token", {"slug": slug})
        assert token.get("token"), (
            f"the sealed full page must exist — the nest has read the gated body: {token!r}"
        )

    # ── A member of the tier: a claim code redeemed, the grant minted as the
    # creator's app mints it, the tier's key served to the member. ──
    member, member_actor = _actor(nest)
    code = creator_actor._ws_call("fauna.payments.claims.mint", {"tier": tier})["code"]
    redeemed = member_actor._ws_call("fauna.payments.claims.redeem", {"code": code})
    assert redeemed.get("tier") == tier, f"{redeemed!r}"
    creator_actor.subscription_grant_pending(tier, period_key, bytes(member["actor_id_bytes"]))
    blob = member_actor.subscription_key_blob(creator_id, tier)
    opened, _ = fauna_ffi.open_key_blob(bytes(member["signing_key"]), blob["blob_data"])
    assert opened == period_key, "the member must hold the tier's key"

    outsider, _ = _actor(nest)
    callers = {"outside the tier": outsider, "a member": member, "the creator": creator}

    # ── Positive control first, for every caller: the teaser word finds the
    # post. ──
    for who, user in callers.items():
        found = wait_until(
            lambda user=user: _naming(_hits(nest, user, teaser_word), teaser_word),
            RPC_ROUNDTRIP_S,
            diagnose=lambda user=user: (
                f"search by {who} for the teaser word {teaser_word!r} never found the "
                f"post; hits: {_hits(nest, user, teaser_word)!r}"
            ),
        )
        assert len(found) == 1 and str(found[0].get("content_type", "")).startswith("post"), (
            f"the teaser word finds exactly the one post for {who}: {found!r}"
        )

    # ── Then the negative: the gated body's word finds nothing, for anyone. ──
    leaked = {who: _hits(nest, user, body_word) for who, user in callers.items()}
    leaked = {who: hits for who, hits in leaked.items() if hits}
    assert not leaked, (
        f"a word behind the paywall ({body_word!r}) was found by search: {leaked!r}"
    )
