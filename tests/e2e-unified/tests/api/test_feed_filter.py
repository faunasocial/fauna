"""E2E test: Feed filtering by hashtag.

Tests that the feed query path correctly filters posts by hashtag tags,
using the ``fauna.posts.*`` / ``fauna.feed.*`` WS-RPC kinds (the HTTP twins
``POST /api/v1/posts`` and ``GET /api/v1/feeds*`` were deleted by the
WS-RPC-everywhere migration) with BARE-encoded post payloads.
"""

import time

import pytest
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("custom-feeds")
def test_feed_single_author_filter(two_nodes):
    """Alice creates a feed filtering for #cats; only Bob's cat post matches."""
    port_a = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    # --- Register users ---
    alice = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port_a, admin_signing_key=admin_sk)

    # --- Bob posts two messages ---
    now_us = int(time.time() * 1_000_000)

    # Post 1: Bob posts about cats
    cat_text = "Look at my cute #cats"
    cat_post = sign_and_encode_post(
        bob["signing_key"], now_us, cat_text,
        tags=[("cats", 17, 22)],
    )
    cat_post_id = ws_api.create_post(port_a, bob, cat_post)

    # Post 2: Bob posts about dogs
    dog_text = "I love #dogs too"
    dog_post = sign_and_encode_post(
        bob["signing_key"], now_us + 1_000_000, dog_text,
        tags=[("dogs", 7, 12)],
    )
    ws_api.create_post(port_a, bob, dog_post)

    # --- Alice creates a feed filtering for #cats ---
    feed_id = ws_api.create_feed(port_a, alice, "Cat Feed", rules=[
        {"HasHashtag": {"tags": ["cats"]}},
    ])

    # --- Query the feed ---
    posts = ws_api.feed_posts(port_a, alice, feed_id)

    assert len(posts) == 1, f"expected 1 post, got {len(posts)}: {posts}"
    assert posts[0]["post_id"] == cat_post_id
    assert "cats" in posts[0]["tags"]


@pytest.mark.feature("custom-feeds")
def test_feed_multi_author_filter(two_nodes):
    """Three users on the same node; feed filters #cats across multiple authors."""
    port_a = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    # --- Register Alice, Bob, Charlie ---
    alice = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port_a, admin_signing_key=admin_sk)
    charlie = create_actor_and_register(port_a, admin_signing_key=admin_sk)

    # --- Four posts with distinct timestamps ---
    now_us = int(time.time() * 1_000_000)

    # Bob's cat post
    bob_cat_text = "Bob loves #cats so much"
    bob_cat_post = sign_and_encode_post(
        bob["signing_key"], now_us, bob_cat_text,
        tags=[("cats", 10, 15)],
    )
    bob_cat_id = ws_api.create_post(port_a, bob, bob_cat_post)

    # Bob's dog post
    bob_dog_text = "Bob also likes #dogs"
    bob_dog_post = sign_and_encode_post(
        bob["signing_key"], now_us + 1_000_000, bob_dog_text,
        tags=[("dogs", 15, 20)],
    )
    ws_api.create_post(port_a, bob, bob_dog_post)

    # Charlie's cat post
    charlie_cat_text = "Charlie has #cats at home"
    charlie_cat_post = sign_and_encode_post(
        charlie["signing_key"], now_us + 2_000_000, charlie_cat_text,
        tags=[("cats", 12, 17)],
    )
    charlie_cat_id = ws_api.create_post(port_a, charlie, charlie_cat_post)

    # Charlie's dog post
    charlie_dog_text = "Charlie walks #dogs daily"
    charlie_dog_post = sign_and_encode_post(
        charlie["signing_key"], now_us + 3_000_000, charlie_dog_text,
        tags=[("dogs", 14, 19)],
    )
    ws_api.create_post(port_a, charlie, charlie_dog_post)

    # --- Alice creates a feed filtering for #cats ---
    feed_id = ws_api.create_feed(port_a, alice, "Cats Only", rules=[
        {"HasHashtag": {"tags": ["cats"]}},
    ])

    # --- Query the feed ---
    posts = ws_api.feed_posts(port_a, alice, feed_id)

    assert len(posts) == 2, f"expected 2 cat posts, got {len(posts)}: {posts}"

    # Collect post IDs and verify both cat posts are present
    returned_ids = {p["post_id"] for p in posts}
    assert bob_cat_id in returned_ids, f"Bob's cat post {bob_cat_id} not in results"
    assert charlie_cat_id in returned_ids, f"Charlie's cat post {charlie_cat_id} not in results"

    # Verify all returned posts have "cats" in their tags
    for p in posts:
        assert "cats" in p["tags"], f"post {p['post_id']} missing 'cats' tag: {p['tags']}"
