"""tier_3 E2E for the feed interaction-bar counters (ratified 2026-06-27).

Proves the increment + projection end-to-end against a real nest binary
(`feed.md` § Interaction bar; `api-layers.md` § Labels & Engagement):

* a ``like`` on a post bumps its ``like_count`` and the next feed snapshot
  carries it; a repeat like by the same actor does NOT double-count
  (toggle dedup via the stable ``compute_toggle_event_id`` key); ``unlike``
  decrements it back.
* a reply / repost / quote *post* referencing a target bumps that target's
  matching counter when the referencing post lands (the ``Reference::{Reply,
  Repost,Quote}`` write site); a byte-identical re-create does NOT double-count
  (idempotent, keyed by the referencing post id via ``compute_reference_event_id``).

The counts ride the wire ``FeedPostItem.{like,reply,repost,quote}_count``
(`libs/fauna-protocol/src/feed.rs`), projected from ``content_meta`` by
``query_feed`` and read here off ``fauna.feed.local.posts``.
"""

import time

import pytest
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import post_reference, sign_and_encode_post

pytestmark = pytest.mark.tier_3


def _post_in_feed(port: int, actor: dict, post_id: str) -> dict:
    """The single ``FeedPostItem`` dict for ``post_id`` from the local feed."""
    posts = ws_api.local_feed_posts(port, actor)
    for p in posts:
        if p["post_id"] == post_id:
            return p
    raise AssertionError(f"post {post_id[:16]} not in local feed ({len(posts)} posts)")


@pytest.mark.feature("feed-interactions")
def test_like_increments_like_count_idempotently(two_nodes):
    """like → like_count 1; repeat like → still 1; unlike → 0."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    liker = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    post_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "Like me")
    )

    # Freshly created — no activity yet.
    assert _post_in_feed(port, author, post_id)["like_count"] == 0

    # First like bumps the counter.
    ws_api.interact(port, liker, post_id, action="like")
    assert _post_in_feed(port, author, post_id)["like_count"] == 1

    # A repeat like by the SAME actor is a counter no-op (toggle dedup).
    ws_api.interact(port, liker, post_id, action="like")
    assert _post_in_feed(port, author, post_id)["like_count"] == 1

    # unlike reverses it (clamped ≥ 0).
    ws_api.interact(port, liker, post_id, action="unlike")
    assert _post_in_feed(port, author, post_id)["like_count"] == 0

    # A second unlike is a no-op (already at 0, the actor no longer holds a like).
    ws_api.interact(port, liker, post_id, action="unlike")
    assert _post_in_feed(port, author, post_id)["like_count"] == 0


@pytest.mark.feature("feed-interactions")
def test_two_actors_each_count_once(two_nodes):
    """Distinct actors each contribute one like; one unlike removes only theirs."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    post_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "Two likers")
    )

    ws_api.interact(port, alice, post_id, action="like")
    ws_api.interact(port, bob, post_id, action="like")
    assert _post_in_feed(port, author, post_id)["like_count"] == 2

    # Bob un-likes; Alice's like remains.
    ws_api.interact(port, bob, post_id, action="unlike")
    assert _post_in_feed(port, author, post_id)["like_count"] == 1


@pytest.mark.parametrize(
    "kind,count_field",
    [
        ("Reply", "reply_count"),
        ("Repost", "repost_count"),
        ("Quote", "quote_count"),
    ],
)
@pytest.mark.feature("feed-interactions")
def test_reference_post_increments_target_count_idempotently(two_nodes, kind, count_field):
    """A reply/repost/quote post bumps its target's counter; re-create doesn't double."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    actor = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    target_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "Quote/reply me")
    )
    assert _post_in_feed(port, author, target_id)[count_field] == 0

    # A post referencing the target bumps the target's matching counter at ingest.
    referencing_bytes = sign_and_encode_post(
        actor["signing_key"],
        now_us + 1_000_000,
        f"a {kind} of the target",
        references=[post_reference(kind, target_id)],
    )
    ws_api.create_post(port, actor, referencing_bytes)
    assert _post_in_feed(port, author, target_id)[count_field] == 1

    # Re-creating the byte-identical referencing post is idempotent: content-
    # addressed store_post dedups it, and the engagement event (keyed by the
    # referencing post id) is already recorded, so the counter does not move.
    ws_api.create_post(port, actor, referencing_bytes)
    assert _post_in_feed(port, author, target_id)[count_field] == 1
