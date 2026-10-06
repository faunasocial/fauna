"""Own-post delete (feed.md § State & data shape → Post deletion, IDs
user-approved 2026-07-16): create a post, delete it with confirm through the
post-card ⋯ overflow (`feed-post-actions-menu`, the same flyout the
trained-topic training verbs use — `test_trained_topics.py`), and confirm it
is gone from the feed.

tier_3: a real ``fauna-nest`` binary + a real signed `fauna.posts.delete`
Tombstone round-trip (`FeedManager::delete_post`), not an optimistic
client-side row removal.
"""
import time
import uuid

import pytest

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.waiting import wait_until
from i18n.strings import S
from tests.api import ws_api
from tests.api.bare import post_reference, sign_and_encode_post

pytestmark = pytest.mark.tier_3


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.mark.feature("feed-delete-own-post")
def test_delete_own_post_removes_it_from_the_feed(logged_in_app):
    """Create a post, delete it via the ⋯ menu with confirm, and confirm the
    card disappears — a real fauna.posts.delete round-trip, not a local-only
    row removal (the exact class of bug the mail-filter-edit migration this
    session fixed for filter-delete)."""
    text = _unique("delete me")
    logged_in_app.feed.create_post(text=text)
    initial = logged_in_app.feed.post_count()

    logged_in_app.feed.delete_post(index=0)

    assert logged_in_app.feed.post_count() == initial - 1
    # The deleted post's text must not still be findable anywhere in the feed.
    remaining_texts = [
        logged_in_app.driver.get_text("feed-post-text", index=i)
        for i in range(logged_in_app.feed.post_count())
    ]
    assert text not in remaining_texts, f"deleted post text still present: {remaining_texts}"


@pytest.mark.feature("feed-delete-own-post")
def test_delete_affordance_absent_on_another_authors_post(logged_in_app, nest_instance):
    """The `feed-post-delete-button` (own-post delete, feed.md § State & data
    shape → Post deletion) must NOT render on a post authored by someone else
    — `isOwn`/`is_own` gates it (the card compares `post.author` against the
    local actor). Not app-specific — any GUI app reaches this path:
    the local feed (`fauna.feed.local.posts`, the default `selected_feed`)
    shows every local actor's posts with no follow/contact relationship
    needed, so a headless OTHER actor's post is enough to seed the case.

    `feed-post-actions-menu` itself renders for every post (the training
    verbs apply regardless of authorship — `test_trained_topics.py`); only
    the delete item inside is authorship-gated, so this asserts the item is
    absent, not that the whole menu is."""
    app = logged_in_app
    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    other_actor = ApiActor(
        nest_instance["url"], other["token"], other["actor_id_hex"],
        bytes(other["signing_key"]),
    )
    text = _unique("other author's post")
    other_actor.post_to_feed("", text)

    # Seed-then-reselect: no client live-reloads the feed when another actor's
    # post lands out-of-band (the shared FeedManager has no push/poll for it —
    # verified against nest: the General feed's query DOES return the post),
    # so drive the reload the way a user would: re-select the feed. Without
    # this the wait only ever passed by inheriting a batch-context reload.
    app.feed.open_feed("General")

    assert app.feed.wait_for_post_text(text), (
        f"the OTHER actor's post should appear in the selected feed; "
        f"error={app.error_text()!r}"
    )
    idx = next(
        i for i in range(app.feed.post_count())
        if text in app.feed.post_text(i)
    )

    app.feed.open_post_actions(index=idx)
    assert not app.feed.post_delete_visible(), (
        "feed-post-delete-button must not render on another author's post"
    )


# How long the page may take to follow the state it paints — a ceiling for a
# broken surface, never a subject (convention 14).
REPAINT_BUDGET_S = 30.0


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.feature("feed-delete-own-post")
def test_deleting_your_post_leaves_replies_and_quotes_standing_and_says_it_is_gone(
    logged_in_app, nest_instance
):
    """feed.md § Post deletion: "References TO the deleted post dangle by
    design (replies/quotes by others survive; their embedded target renders as
    the existing not-found state client-side) — deleting must never destroy
    other authors' content."

    Someone else quotes my post and replies to it; I delete it through the
    card's ⋯ menu. My post leaves the feed; theirs stay; and the quote, which
    showed my post inside it, now says the post is not there — never my
    deleted words, which the deleting app had already loaded and could
    otherwise keep painting from what it resolved before the delete.

    A reply has no card surface that shows its parent (tui paints none), so
    for it "standing" is the whole claim; the quote carries the embed half.
    The other person's two posts go over the wire as the ordinary signed posts
    any app would create — they are the setup, not the journey under test.
    """
    app = logged_in_app
    feed = app.feed
    mine = _unique("delete-referenced")
    feed.create_post(text=mine)
    row = feed.wait_for_post_state_by_text(mine)
    assert row is not None and row.get("post_id"), (
        f"own post {mine!r} should be readable from feed state; "
        f"error={app.error_text()!r}"
    )
    target_id = row["post_id"]

    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    now_us = int(time.time() * 1_000_000)
    their_quote = _unique("their-quote")
    their_reply = _unique("their-reply")
    quote_id = ws_api.create_post(
        nest_instance["port"], other,
        sign_and_encode_post(
            other["signing_key"], now_us, their_quote,
            references=[post_reference("Quote", target_id)],
        ),
    )
    reply_id = ws_api.create_post(
        nest_instance["port"], other,
        sign_and_encode_post(
            other["signing_key"], now_us + 1, their_reply,
            references=[post_reference("Reply", target_id)],
        ),
    )

    feed.open_feed("General")

    def embed_of(post_id):
        at = feed.post_index_by_id(post_id)
        return None if at < 0 else feed.quoted_post_text(at)

    wait_until(
        lambda: mine in (embed_of(quote_id) or ""),
        REPAINT_BUDGET_S,
        diagnose=lambda: (
            f"their quote's embed={embed_of(quote_id)!r} "
            f"reply in window={feed.post_state_by_id(reply_id) is not None} "
            f"error={app.error_text()!r}"
        ),
    )
    assert feed.post_state_by_id(reply_id) is not None, (
        "their reply should be in the window before the delete"
    )

    feed.delete_post(index=feed.post_index_by_id(target_id))

    assert feed.post_state_by_id(target_id) is None, (
        "my deleted post must leave the feed"
    )
    assert feed.post_state_by_id(quote_id) is not None, (
        "their quote of my post must survive my delete"
    )
    assert feed.post_state_by_id(reply_id) is not None, (
        "their reply to my post must survive my delete"
    )
    wait_until(
        lambda: embed_of(quote_id) == S.feed.post.post_not_found,
        REPAINT_BUDGET_S,
        diagnose=lambda: (
            f"their quote's embed={embed_of(quote_id)!r} "
            f"(want {S.feed.post.post_not_found!r}) error={app.error_text()!r}"
        ),
    )
