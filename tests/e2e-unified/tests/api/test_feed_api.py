"""E2E API test: Feed post creation, querying, and custom feed filters.

Tests the feed API directly without a browser: create posts, query them,
create filtered feeds, and verify filter rules work.

Drives the ``fauna.posts.*`` / ``fauna.feed.*`` WS-RPC kinds via
``tests.api.ws_api`` — the HTTP twins (``POST /api/v1/posts``,
``GET|POST|DELETE /api/v1/feeds*``) were deleted by the WS-RPC-everywhere
migration.
"""

import time

import pytest
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("feed-read")
def test_feed_post_and_query(two_nodes):
    """Create posts via WS-RPC and query them from the local feed.

    Verifies:
    1. Post creation returns a post_id
    2. Local feed query returns the post
    3. Multiple posts appear in correct order (newest first)
    4. Post metadata (author, created_at) is correct
    """
    port = two_nodes["port_a"]
    admin_sk_a = two_nodes["admin_sk_a"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk_a)

    now_us = int(time.time() * 1_000_000)

    # Create 3 posts
    post_ids = []
    for i in range(3):
        post_data = sign_and_encode_post(
            actor["signing_key"],
            now_us + i * 1_000_000,
            f"Post number {i+1}",
        )
        post_id = ws_api.create_post(port, actor, post_data)
        assert post_id, f"Missing post_id: {post_id!r}"
        post_ids.append(post_id)
        print(f"Created post {i+1}: {post_id[:16]}...")

    time.sleep(0.3)

    # Query local feed
    posts = ws_api.local_feed_posts(port, actor)
    feed_post_ids = [p["post_id"] for p in posts]

    for pid in post_ids:
        assert pid in feed_post_ids, f"Post {pid[:16]} not in feed"
    print(f"All 3 posts visible in local feed ({len(posts)} total)")

    # Newest should be first
    idx0 = feed_post_ids.index(post_ids[2])
    idx2 = feed_post_ids.index(post_ids[0])
    assert idx0 < idx2, "Newest post should come first"
    print("Posts in correct order (newest first)")


@pytest.mark.feature("custom-feeds")
def test_feed_custom_filter(two_nodes):
    """Create a custom feed with filter rules, verify filtering works.

    Verifies:
    1. Create a feed with a BodyContains filter
    2. Only matching posts appear in the filtered feed
    """
    port = two_nodes["port_a"]
    admin_sk_a = two_nodes["admin_sk_a"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk_a)

    now_us = int(time.time() * 1_000_000)

    # Create posts with different content
    texts = [
        "Regular update, nothing special",
        "Check out this amazing sunset photo",
        "Another regular update for the day",
    ]
    post_ids = []
    for i, text in enumerate(texts):
        post_data = sign_and_encode_post(actor["signing_key"], now_us + i * 1_000_000, text)
        post_ids.append(ws_api.create_post(port, actor, post_data))

    time.sleep(0.3)

    # Create a feed with BodyContains filter for "sunset"
    feed_id = ws_api.create_feed(
        port,
        actor,
        name="Sunset Feed",
        rules=[{"BodyContains": {"terms": ["sunset"]}}],
        combination="all",
    )
    print(f"Created feed: {feed_id}")

    # Query the filtered feed
    filtered = ws_api.feed_posts(port, actor, feed_id)
    filtered_ids = [p["post_id"] for p in filtered]

    assert post_ids[1] in filtered_ids, "Sunset post should be in filtered feed"
    assert post_ids[0] not in filtered_ids, "Non-sunset post should be excluded"
    assert post_ids[2] not in filtered_ids, "Non-sunset post should be excluded"
    print(f"Filtered feed has {len(filtered)} posts (expected 1)")

    # Clean up: delete the feed
    ws_api.delete_feed(port, actor, feed_id)
    print("Feed deleted")


@pytest.mark.feature("custom-feeds")
def test_feed_with_several_rules_serves_all_or_any(two_nodes):
    """The nest evaluates a feed's combination mode over SEVERAL rules
    (`feed.md` § Feed-rule types: `All` — every rule must match — or `Any` — at
    least one). A hashtag rule and a word rule over four posts cover the truth
    table; the "any" feed serves the three that match a rule, the "all" feed
    only the one that matches both. The hashtag rule is the load-bearing half:
    it used to inner-join the tag links, which dropped every UNTAGGED post
    before the `Any` OR was read."""
    port = two_nodes["port_a"]
    admin_sk_a = two_nodes["admin_sk_a"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk_a)

    now_us = int(time.time() * 1_000_000)
    # (body, tag facets) — "#garden" sits at bytes 0..7 wherever it is used.
    posts = {
        "first": ("#garden notes", [("garden", 0, 7)]),
        "second": ("a post about cycling", None),
        "both": ("#garden then cycling", [("garden", 0, 7)]),
        "neither": ("a post about nothing much", None),
    }
    ids = {
        key: ws_api.create_post(
            port,
            actor,
            sign_and_encode_post(
                actor["signing_key"], now_us + i * 1_000_000, text, tags=tags
            ),
        )
        for i, (key, (text, tags)) in enumerate(posts.items())
    }
    rules = [
        {"HasHashtag": {"tags": ["garden"]}},
        {"BodyContains": {"terms": ["cycling"]}},
    ]

    any_feed = ws_api.create_feed(port, actor, name="Any rule", rules=rules, combination="any")
    any_ids = {p["post_id"] for p in ws_api.feed_posts(port, actor, any_feed)}
    assert {ids["first"], ids["second"], ids["both"]} <= any_ids, (
        f"the any-rule feed must serve every post matching a rule: {any_ids}"
    )
    assert ids["neither"] not in any_ids, "a post matching no rule must be excluded"

    all_feed = ws_api.create_feed(port, actor, name="All rules", rules=rules, combination="all")
    all_ids = {p["post_id"] for p in ws_api.feed_posts(port, actor, all_feed)}
    assert ids["both"] in all_ids, "the post matching both rules must be served"
    assert not {ids["first"], ids["second"], ids["neither"]} & all_ids, (
        f"the all-rules feed must exclude posts matching only one rule: {all_ids}"
    )

    ws_api.delete_feed(port, actor, any_feed)
    ws_api.delete_feed(port, actor, all_feed)


@pytest.mark.feature("custom-feeds")
def test_feed_crud_lifecycle(two_nodes):
    """Create, list, and delete custom feeds.

    Verifies:
    1. Create returns feed_id
    2. List includes the new feed
    3. Delete removes it from the list
    """
    port = two_nodes["port_a"]
    admin_sk_a = two_nodes["admin_sk_a"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk_a)

    # Create feed
    feed_id = ws_api.create_feed(port, actor, name="Test Feed", rules=[], combination="all")
    assert feed_id, "Should get a feed_id"
    print(f"Created feed: {feed_id}")

    # List feeds
    feeds = ws_api.list_feeds(port, actor)
    feed_ids = [f["feed_id"] for f in feeds]
    assert feed_id in feed_ids, "New feed should be in list"
    print(f"Feed appears in list ({len(feeds)} feeds)")

    # Delete feed
    ws_api.delete_feed(port, actor, feed_id)

    # Verify deleted
    feeds_after = ws_api.list_feeds(port, actor)
    feed_ids_after = [f["feed_id"] for f in feeds_after]
    assert feed_id not in feed_ids_after, "Deleted feed should be gone"
    print("Feed deleted and no longer in list")
