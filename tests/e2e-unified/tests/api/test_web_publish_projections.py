"""E2E test: the two additive projections the published-post management surface
reads (``web-content-hosting.md`` § Published-post management).

Both are pure *read* projections over state a creator already writes with the
shipped ``fauna.web.publish.*`` kinds — nothing here is new nest behavior beyond
the joins, which is exactly why they need a wire-level test: a client that
renders the ⋯-overflow verbs off a field the nest silently stops projecting
degrades to "no affordance", which looks identical to "this post isn't
published".

Production data flows asserted end-to-end:

1. ``fauna.web.publish.set`` writes the ``content_links`` web-publish row →
   ``query_feed`` / ``query_local_feed``'s per-row ``project_web_slug`` lookup →
   ``FeedPostRow.web_slug`` → ``FeedPostOut`` → ``post_item()`` → the wire
   ``FeedPostItem.web_slug`` → ``PostSummary.web_slug`` → the ⋯ menu's
   publish/unpublish/copy-link verb presence.
2. ``fauna.web.publish.list``'s ``content_meta`` LEFT JOIN →
   ``PublishedPost.gated_tier`` → the ``web-published-post-item`` row's
   *Copy paywall link* affordance (published **and** gated only).

Both fields are additive: the nest omits ``gated_tier`` on an ungated post and
the client renders the affordances absent, so the assertions below are what pins them present.
"""

import time

import pytest
from clients.ws_rpc_admin_client import RpcCallError
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("personal-website")
def test_feed_projects_web_slug_for_published_posts_only(two_nodes):
    """A post's `web_slug` appears on feed reads exactly while it is published,
    and never leaks onto an unpublished sibling."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    reader = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    published_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "a page-worthy post")
    )
    plain_id = ws_api.create_post(
        port,
        author,
        sign_and_encode_post(author["signing_key"], now_us + 1_000_000, "just a post"),
    )

    feed_id = ws_api.create_feed(port, reader, "All", rules=[])

    # Before publishing: no post carries a slug. `.get` with a default is the
    # honest read — the field is additive, so "absent" and "None" are the same
    # unpublished state a client sees.
    posts = {p["post_id"]: p for p in ws_api.feed_posts(port, reader, feed_id)}
    assert posts[published_id].get("web_slug") is None
    assert posts[plain_id].get("web_slug") is None

    effective = ws_api.web_publish_set(port, author, published_id, "my-page")
    assert effective == "my-page"

    posts = {p["post_id"]: p for p in ws_api.feed_posts(port, reader, feed_id)}
    assert posts[published_id].get("web_slug") == "my-page", (
        "a published post must project its slug — this is what the ⋯ menu "
        "derives Unpublish / Copy web link from"
    )
    assert posts[plain_id].get("web_slug") is None, (
        "the publish row is per-post; an unpublished sibling must stay absent"
    )

    # The local-feed read is a SEPARATE query path in `db/feeds.rs` — a
    # projection wired into only one of them is the classic half-fix.
    local = {p["post_id"]: p for p in ws_api.local_feed_posts(port, author)}
    assert local[published_id].get("web_slug") == "my-page"
    assert local[plain_id].get("web_slug") is None

    # Unpublish is reversible, and the projection follows it back down — the
    # ⋯ menu must return to offering *Publish to web*.
    ws_api.web_publish_unset(port, author, published_id)
    posts = {p["post_id"]: p for p in ws_api.feed_posts(port, reader, feed_id)}
    assert posts[published_id].get("web_slug") is None


@pytest.mark.feature("personal-website")
def test_publish_list_joins_the_gated_tier(two_nodes):
    """`fauna.web.publish.list` carries each row's gating tier, so the
    management surface knows which rows offer *Copy paywall link*."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    author = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    ungated_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "free to read")
    )
    ws_api.web_publish_set(port, author, ungated_id, "free-one")

    rows = {r["post_id"].hex(): r for r in ws_api.web_publish_list(port, author)}
    assert ungated_id in rows, "the caller's published post must be listed"
    assert rows[ungated_id]["slug"] == "free-one"
    assert rows[ungated_id].get("gated_tier") is None, (
        "an ungated row must carry no tier — and must NOT be dropped by the "
        "LEFT JOIN, which an INNER JOIN would do silently"
    )


@pytest.mark.feature("personal-website")
def test_publish_list_is_caller_scoped(two_nodes):
    """One creator's published posts never appear in another's management
    list — the surface is caller-scoped, not nest-wide."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    alice_post = ws_api.create_post(
        port, alice, sign_and_encode_post(alice["signing_key"], now_us, "alice's page")
    )
    ws_api.web_publish_set(port, alice, alice_post, "alice-page")

    bob_rows = {r["post_id"].hex() for r in ws_api.web_publish_list(port, bob)}
    assert alice_post not in bob_rows

    alice_rows = {r["post_id"].hex() for r in ws_api.web_publish_list(port, alice)}
    assert alice_post in alice_rows


@pytest.mark.feature("personal-website")
def test_a_stranger_republish_is_refused_and_the_authors_web_slug_is_unaffected(two_nodes):
    """A stranger publishing someone else's post is refused by the nest, and —
    defense in depth — could not have hijacked the AUTHOR's ⋯ menu even if it
    somehow got through.

    `fauna.web.publish.set` is now authorship-gated (`fauna.web.permission_denied`
    for a non-author caller). The publish row's key is
    `(link_type, source_id, actor_id)`, so two actors could each hold a row for
    one post if the gate were ever bypassed; the feed projection keys on the
    post's **author** as a second layer, so the author's Copy-web-link would
    still never yield a stranger's page and Unpublish would still never no-op
    against a row the caller doesn't own.
    """
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    stranger = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = int(time.time() * 1_000_000)
    post_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "an original post")
    )

    # The stranger tries to publish someone else's post on their own site —
    # refused before any row is written.
    with pytest.raises(RpcCallError) as exc:
        ws_api.web_publish_set(port, stranger, post_id, "stolen-slug")
    assert exc.value.code == "fauna.web.permission_denied"

    feed_id = ws_api.create_feed(port, author, "All", rules=[])
    posts = {p["post_id"]: p for p in ws_api.feed_posts(port, author, feed_id)}
    assert posts[post_id].get("web_slug") is None, (
        "the author has not published this post — a refused stranger publish "
        "must not make the author's menu offer Unpublish / Copy web link"
    )

    # The author themself still succeeds, and THEIR slug is what projects.
    ws_api.web_publish_set(port, author, post_id, "my-own-slug")
    posts = {p["post_id"]: p for p in ws_api.feed_posts(port, author, feed_id)}
    assert posts[post_id].get("web_slug") == "my-own-slug"
