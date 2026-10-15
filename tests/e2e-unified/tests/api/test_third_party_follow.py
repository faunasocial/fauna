"""tier_3 API e2e: the **follow arm**, ``fauna:follow:write`` — a third-party
principal follows and unfollows as the account, the free ``followers`` tier
only, and reads only the followers bit of its status
(``docs/goal/behavior/authorization-server.md`` § Scope grammar → *The sixth
arm*; follow = the free tier, ``docs/goal/behavior/monetization.md`` § The
unifying model).

One journey against a real nest and the real PDS bridge (the consent's
precondition, as in ``test_third_party_session.py``):

- the principal's follow is refused while the account has published no
  subscriber encapsulation key — never a classical degrade — and lands once the
  account's first-party session publishes its registry entry
  (``post-quantum.md`` § surface B; what every app sends at session start, here
  through the same shared builder over the C-ABI);
- a paid tier name is refused; the follow reads back pending;
- the author's client approves it (the headless-author witness rides the real
  approve door, wrapping exactly as an app's drain pump does), and the
  subscription shows in the owner's own ``mine.list`` with a **hybrid (X-Wing)**
  ``KeyBlob`` entry the owner opens to the author's period key;
- the principal's ``status.get`` says following; after the owner subscribes to
  a paid tier and then unsubscribes from it on its own first-party connection,
  the principal's ``status.get`` names ``followers`` at every point and never
  the paid tier, and its unfollow is refused while the paid tier is held.

No wall-clock assertion anywhere (convention 14).
"""

import os

import cbor2
import pytest

from actions.api_actor import ApiActor
from fauna_ffi import open_key_blob
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from helpers.principal_session import PrincipalSession, ceremony, refusal_code, upgrade

pytestmark = pytest.mark.tier_3

FOLLOW = "fauna:follow:write"
FOLLOWERS = "followers"


def _actor(nest, who) -> ApiActor:
    return ApiActor(nest["url"], who["token"], who["actor_id_hex"], bytes(who["signing_key"]))


def _entry_for(blob_data: bytes, subscriber: bytes) -> dict:
    """The stored ``KeyBlob``'s entry for ``subscriber`` — the blob is an
    ``EmbedAsBytes`` wire (``bytes`` = the signed blob's canonical CBOR)."""
    blob = cbor2.loads(cbor2.loads(bytes(blob_data))["bytes"])
    entries = [e for e in blob["entries"] if bytes(e["subscriber"]) == subscriber]
    assert len(entries) == 1, blob["entries"]
    return entries[0]


@pytest.mark.feature("connected-apps")
def test_a_principal_follows_and_reads_only_the_followers_bit(consent_nest, consent_bridge):
    from common.auth import register_handled_actor

    nest = consent_nest
    owner = _actor(nest, nest["user"])
    owner_id = bytes.fromhex(owner.actor_id)
    bob = register_handled_actor(
        nest["port"], handle="bob", domain=HANDLE_DOMAIN, base_url=nest["url"]
    )
    author = _actor(nest, bob)
    author_id = bytes.fromhex(author.actor_id)
    _, gold_key = author.subscription_create_tier_with_key("gold", rank=1)

    client, tokens = ceremony(nest, FOLLOW, declared=f"{FOLLOW} openid")
    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))

    def status() -> dict:
        ok, reply = session.call("fauna.subscriptions.status.get", {"author_id": author_id})
        assert ok, f"the principal reads the followers bit: {reply!r}"
        assert reply.get("tier") in (None, FOLLOWERS), f"never a paid tier: {reply!r}"
        assert reply.get("expires_at") is None, reply
        return reply

    def follow(tier=FOLLOWERS):
        return session.call(
            "fauna.subscriptions.subscribe", {"author_id": author_id, "tier": tier}
        )

    # ── No registry entry yet: refused, never degraded to classical. ──
    ok, reply = follow()
    assert not ok and refusal_code(reply) == "fauna.subscriptions.pq_key_unpublished", reply
    assert status().get("tier") is None

    # ── The owner's first-party session publishes the entry; the follow lands. ──
    owner.publish_encaps_key()
    ok, reply = follow("gold")
    assert not ok and refusal_code(reply) == "fauna.subscriptions.permission_denied", (
        f"a paid tier is the user's own purchase: {reply!r}"
    )
    ok, reply = follow()
    assert ok and reply.get("outcome") == "queued", reply
    pending = status()
    assert pending.get("tier") is None and pending.get("follow_pending") is True, pending

    # ── The author's client approves, wrapping hybrid to the published key. ──
    followers_key = os.urandom(32)
    author.subscription_grant_pending(FOLLOWERS, followers_key, owner_id)
    following = status()
    assert following.get("tier") == FOLLOWERS and not following.get("follow_pending"), following

    mine = [
        s for s in owner.subscription_mine_list()
        if bytes(s["author_id"]) == author_id and s["tier"] == FOLLOWERS
    ]
    assert len(mine) == 1, owner.subscription_mine_list()
    blob = owner.subscription_key_blob(author_id, FOLLOWERS)["blob_data"]
    entry = _entry_for(blob, owner_id)
    assert str(entry.get("suite", "")).lower() == "xwing", f"a hybrid entry: {entry!r}"
    opened, blob_author = open_key_blob(bytes(nest["user"]["signing_key"]), blob)
    assert (opened, blob_author) == (followers_key, author_id)

    # ── A paid tier the owner holds is never named to the principal. ──
    assert owner.subscribe(author_id, "gold").get("outcome") == "queued"
    assert status().get("tier") == FOLLOWERS
    author.subscription_grant_pending("gold", gold_key, owner_id)
    assert owner.subscription_status(author_id).get("tier") == "gold", "the owner sees its tier"
    assert status().get("tier") == FOLLOWERS
    ok, reply = session.call("fauna.subscriptions.unsubscribe", {"author_id": author_id})
    assert not ok and refusal_code(reply) == "fauna.subscriptions.permission_denied", (
        f"leaving a paid tier is the user's own act: {reply!r}"
    )
    assert owner.unsubscribe(author_id).get("outcome") == "queued"
    assert status().get("tier") == FOLLOWERS
