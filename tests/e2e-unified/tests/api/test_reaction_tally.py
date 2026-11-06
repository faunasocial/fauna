"""tier_3 E2E for the reaction tally and the Upvote-as-like dedupe (feed-interactions outcome 12).

Proves `docs/goal/ui/feed.md` § Layout & flow → *Reaction posts count and
paint* (rulings 1, 2 and 5) end-to-end against a real nest binary:

* a ``React { emoji }`` post is an actor-keyed ``content_links
  link_type='react'`` row; ``FeedPostItem.reactions`` carries one group per
  emoji, ``count`` = distinct actors, groups ordered by the earliest row,
  ``viewer_react_id`` = the reading viewer's own live ``React`` post id; a
  deleted ``React`` post leaves the tally.
* an ``Upvote`` post and an interact-door ``like`` by one actor count once
  (the shared ``compute_toggle_event_id`` key); the door's ``unlike``
  withdraws a carrying ``Upvote`` post.
"""

import time

import pytest
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import post_reference, sign_and_encode_post, sign_and_encode_tombstone

pytestmark = pytest.mark.tier_3

HEART = "❤️"
FIRE = "\U0001f525"


def _item(port: int, viewer: dict, post_id: str) -> dict:
    """The ``FeedPostItem`` dict for ``post_id`` as read by ``viewer``."""
    for p in ws_api.local_feed_posts(port, viewer):
        if p["post_id"] == post_id:
            return p
    raise AssertionError(f"post {post_id[:16]} not in {viewer['id'][:8]}'s local feed")


def _in_feed(port: int, viewer: dict, post_id: str) -> bool:
    return any(p["post_id"] == post_id for p in ws_api.local_feed_posts(port, viewer))


def _groups(item: dict) -> list[tuple[str, int]]:
    return [(g["emoji"], g["count"]) for g in item.get("reactions", [])]


def _reference_post(port, actor, now_us, offset_s, text, kind, target, **kw) -> str:
    return ws_api.create_post(
        port,
        actor,
        sign_and_encode_post(
            actor["signing_key"],
            now_us + offset_s * 1_000_000,
            text,
            references=[post_reference(kind, target, **kw)],
        ),
    )


def _delete(port, actor, post_id, now_us, offset_s):
    ws_api.delete_post(
        port,
        actor,
        sign_and_encode_tombstone(actor["signing_key"], post_id, now_us + offset_s * 1_000_000),
    )


@pytest.mark.feature("feed-interactions")
def test_reaction_tally_counts_distinct_actors_and_clears_on_delete(two_nodes):
    """Two actors' hearts are one group of 2; a second device is a no-op; delete reverses."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    carol = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    target = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "React to me")
    )
    assert _groups(_item(port, author, target)) == []

    alice_react = _reference_post(port, alice, now_us, 1, "h", "React", target, emoji=HEART)
    bob_react = _reference_post(port, bob, now_us, 2, "h", "React", target, emoji=HEART)

    # One group, two distinct actors; each viewer sees their own React post id.
    as_alice = _item(port, alice, target)
    assert _groups(as_alice) == [(HEART, 2)]
    assert as_alice["reactions"][0].get("viewer_react_id") == alice_react
    as_bob = _item(port, bob, target)
    assert as_bob["reactions"][0].get("viewer_react_id") == bob_react
    # The author never reacted: the count shows, no viewer id.
    as_author = _item(port, author, target)
    assert _groups(as_author) == [(HEART, 2)]
    assert as_author["reactions"][0].get("viewer_react_id") is None

    # A second device's React by alice (a distinct post) leaves count at 2.
    _reference_post(port, alice, now_us, 3, "h again", "React", target, emoji=HEART)
    assert _groups(_item(port, bob, target)) == [(HEART, 2)]

    # A third actor's other emoji adds a second group, after the first.
    _reference_post(port, carol, now_us, 4, "f", "React", target, emoji=FIRE)
    assert _groups(_item(port, bob, target)) == [(HEART, 2), (FIRE, 1)]

    # Bob deletes his React post: the heart leaves bob's tally and his viewer id.
    _delete(port, bob, bob_react, now_us, 5)
    as_bob = _item(port, bob, target)
    assert _groups(as_bob) == [(HEART, 1), (FIRE, 1)]
    assert as_bob["reactions"][0].get("viewer_react_id") is None


@pytest.mark.feature("feed-interactions")
def test_upvote_post_and_door_like_count_once(two_nodes):
    """An Upvote post plus a door like is one like; unlike withdraws both."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    liker = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    target = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "Like me")
    )

    other = create_actor_and_register(port, admin_signing_key=admin_sk)

    # Door like first, Upvote post second (other): one key, one like. The row
    # names no carrier, so the later Upvote post is redundant.
    ws_api.interact(port, other, target, action="like")
    _reference_post(port, other, now_us, 1, "up", "Upvote", target)
    assert _item(port, author, target)["like_count"] == 1

    # Upvote post first (liker) — it is the key row's carrier — then a door
    # like by the same actor: still one like for that actor (2 in all).
    upvote = _reference_post(port, liker, now_us, 2, "up", "Upvote", target)
    assert _item(port, author, target)["like_count"] == 2
    ws_api.interact(port, liker, target, action="like")
    assert _item(port, author, target)["like_count"] == 2
    assert _in_feed(port, liker, upvote)

    # The door's unlike removes the like AND withdraws the carrying Upvote post.
    ws_api.interact(port, liker, target, action="unlike")
    assert _item(port, author, target)["like_count"] == 1
    assert not _in_feed(port, liker, upvote)


@pytest.mark.feature("feed-interactions")
def test_upvote_post_alone_counts_once_and_delete_reverses(two_nodes):
    """An Upvote post alone is one like; deleting it reverses the count."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    liker = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    target = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "Upvote me")
    )
    upvote = _reference_post(port, liker, now_us, 1, "up", "Upvote", target)
    assert _item(port, author, target)["like_count"] == 1

    _delete(port, liker, upvote, now_us, 2)
    assert _item(port, author, target)["like_count"] == 0
