"""Feed search and a custom feed's text rule over BRIDGED posts — tier_3, api.

``docs/features/feed-search.md`` outcome 4 (``feed-read-model.md`` § Implementation
status today, *The feed's text filters over bridged posts*): a post that came in
over Bluesky, the fediverse or Nostr is matched by its words — by the feed read's
``search`` re-query (the mandatory ``BodyContains`` the apps' search box sends)
and by a feed of the user's own with a ``BodyContains`` rule — and stops matching
once that bridge is turned off for search.

Each leg seeds the post through the transit point that RESTS it as a post, the
one that writes ``bridge_index_map.post_id`` (the link the text filters follow):
the ActivityPub inbox ``Create``, the Bluesky feed ingest's poll, the Nostr
inbound sweep. A relay-store-only Nostr write rests no post and would prove
nothing here.

"Turned off" is the bridge card's ``show_in_search`` setting
(``content-index.md`` § Bridge content in the Search corpus). The corpus is
nest-wide and its policy is the union of every human actor's choice, so a leg
turns it off for every human actor its nest holds — the admin and the one user —
and only then is the corpus purged.

Convention 14: the absence after the switch is asserted only once the same
caller's reads have found the post, and the post itself is still served
unfiltered — what stops is the match, not the post.
"""

from __future__ import annotations

import os
import secrets
import time

import pytest

import fauna_ffi
from drivers.port_util import find_free_port
from helpers.ap_nest import deliver_note, enable_ap, start_ap_nest
from helpers.fake_follower import FakeFollower
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    FOLLOWED_DID,
    FOLLOWED_HANDLE,
    _link_through_callback,
    _poll_now,
    atproto_far_end,
    bluesky_feed_nest,
)
from tests.test_nostr_inbound_feed import (  # noqa: F401 — fixtures
    INBOUND_POKED_S,
    _poke,
    far_relay,
    nostr_inbound_nest,
)

pytestmark = pytest.mark.tier_3


def _admin(nest) -> dict:
    sk = nest["admin"]["signing_key"]
    return {"actor_id_bytes": bytes(sk.verify_key), "signing_key": sk}


def _ids(posts: list[dict]) -> list[str]:
    return [p["post_id"] for p in posts]


def _matches_then_stops(nest, user: dict, bridge_id: str, post_id: str, word: str) -> None:
    """The outcome, for one bridge: matched by search and by a text-rule feed,
    then — the bridge off for search for every human actor — matched by
    neither, while the post itself is still served."""
    port = nest["port"]
    feed_id = ws_api.create_feed(
        port, user, f"words-{secrets.token_hex(3)}",
        rules=[{"BodyContains": {"terms": [word]}}],
    )

    searched = ws_api.local_feed_posts(port, user, limit=50, search=word)
    assert _ids(searched) == [post_id], (
        f"the feed search for {word!r} must find exactly the {bridge_id} post "
        f"{post_id}; got {[(p['post_id'], p.get('body')) for p in searched]!r}"
    )
    ruled = ws_api.feed_posts(port, user, feed_id, limit=50)
    assert _ids(ruled) == [post_id], (
        f"a feed whose rule is the word {word!r} must list exactly the "
        f"{bridge_id} post {post_id}; got {[(p['post_id'], p.get('body')) for p in ruled]!r}"
    )

    for actor in (user, _admin(nest)):
        ws_api.bridge_set_settings(port, actor, bridge_id, {"show_in_search": False})

    served = _ids(ws_api.local_feed_posts(port, user, limit=50))
    assert post_id in served, (
        f"turning the bridge off for search must leave the {bridge_id} post in "
        f"the feed; the feed serves {served!r}"
    )
    assert ws_api.local_feed_posts(port, user, limit=50, search=word) == [], (
        f"with {bridge_id} off for search, the feed search for {word!r} must "
        f"find nothing"
    )
    assert ws_api.feed_posts(port, user, feed_id, limit=50) == [], (
        f"with {bridge_id} off for search, the text-rule feed must list nothing"
    )


# ── The fediverse ───────────────────────────────────────────────────────────


@pytest.fixture()
def ap_search_nest(ap_binary, tmp_path_factory):
    """One ActivityPub-federating nest: its admin and the one user below are
    every human actor on it."""
    port = find_free_port()
    nest = start_ap_nest(
        ap_binary, tmp_path_factory.mktemp("feed_search_bridged"), port, f"127.0.0.1:{port}",
    )
    try:
        yield nest
    finally:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.mark.feature("feed-search")
def test_a_fediverse_post_is_found_by_its_words_until_the_bridge_is_off_for_search(
    ap_search_nest,
):
    from common import create_actor_and_register

    nest = ap_search_nest
    user = create_actor_and_register(nest["port"], admin_signing_key=nest["admin"]["signing_key"])
    username, actor_url = enable_ap(nest, user)
    word = f"fediword{secrets.token_hex(5)}"
    with FakeFollower(find_free_port()) as peer:
        deliver_note(nest, peer, username, actor_url, f"a fediverse note that says {word} once")
        found = wait_until(
            lambda: ws_api.local_feed_posts(nest["port"], user, limit=50, search=word),
            10.0,
            diagnose=lambda: (
                f"the ingested note never matched its word; the feed serves "
                f"{[(p['post_id'], p.get('source'), p.get('body')) for p in ws_api.local_feed_posts(nest['port'], user, limit=50)]!r}"
            ),
        )
    _matches_then_stops(nest, user, "activitypub", found[0]["post_id"], word)


# ── Bluesky ─────────────────────────────────────────────────────────────────


@pytest.mark.feature("feed-search")
def test_a_bluesky_post_is_found_by_its_words_until_the_bridge_is_off_for_search(
    bluesky_feed_nest, atproto_far_end,
):
    nest, far = bluesky_feed_nest, atproto_far_end
    user = nest["user"]
    _link_through_callback(nest, far)
    word = f"skyword{secrets.token_hex(5)}"
    far.publish_post(
        author_did=FOLLOWED_DID, author_handle=FOLLOWED_HANDLE,
        rkey=f"3ksearch{secrets.token_hex(3)}", text=f"a bluesky post that says {word} once",
    )
    report = _poll_now(nest)
    assert report["errors"] == [] and report["stored"] >= 1, (
        f"the poll should store the followed post: {report!r}; far end saw {far.requests!r}"
    )
    found = wait_until(
        lambda: ws_api.local_feed_posts(nest["port"], user, limit=50, search=word),
        10.0,
        diagnose=lambda: (
            f"the ingested Bluesky post never matched its word; the feed serves "
            f"{[(p['post_id'], p.get('source'), p.get('body')) for p in ws_api.local_feed_posts(nest['port'], user, limit=50)]!r}"
        ),
    )
    _matches_then_stops(nest, user, "bluesky", found[0]["post_id"], word)


# ── Nostr ───────────────────────────────────────────────────────────────────


@pytest.mark.feature("feed-search")
def test_a_nostr_post_is_found_by_its_words_until_the_bridge_is_off_for_search(
    nostr_inbound_nest, far_relay,
):
    nest, relay = nostr_inbound_nest, far_relay
    user, port = nest["user"], nest["port"]
    author_secret = os.urandom(32)
    author = fauna_ffi.nostr_keys(author_secret)
    word = f"nostrword{secrets.token_hex(5)}"
    relay.seed(fauna_ffi.nostr_sign_event(author_secret, {
        "kind": 1, "created_at": int(time.time()) - 60, "tags": [],
        "content": f"a nostr note that says {word} once",
    }))

    linked = ws_api.bridge_link(port, user, "nostr", "generate")
    assert linked["linked"], linked
    ws_api.bridge_set_settings(port, user, "nostr", {"inbound_to_feed": True})
    ws_api.bridge_add_follow(port, user, "nostr", author["npub"])

    def swept():
        _poke(nest)
        return ws_api.local_feed_posts(port, user, limit=50, search=word)

    found = wait_until(
        swept, INBOUND_POKED_S, interval=1.0,
        diagnose=lambda: (
            f"the followed author's note never matched its word; the fake relay "
            f"saw REQs {relay.requests!r}"
        ),
    )
    _matches_then_stops(nest, user, "nostr", found[0]["post_id"], word)
