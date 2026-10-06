"""The feed's two empty states — ``feed-empty-state`` and ``feed-no-results``
(``docs/goal/ui/feed.md`` § Errors & edge cases; ids user-approved 2026-09-25).

Both are derived by the shared ``FeedSnapshot::empty_state`` off the snapshot
(``status == Loaded && posts.is_empty()``, the variant picked by whether a
search is active), so each is painted only once the read has LANDED and at most
one of the pair is ever present. Every assertion here is on element presence
and copy after the feed's own committed re-query (``open_feed`` /
``search_feed`` release on the ``feed_reloads`` barrier), never on a timer
(convention 14).

Its own module on purpose: the witnesses leave the shared session's feed on an
empty custom feed or an active search, and the per-module cold relaunch is
what keeps that from narrowing a neighbour's feed (convention 10).
"""
import uuid

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _require_built(app, element_id: str) -> None:
    # tui leads (built 2026-09-26), macOS and iOS lifted through FaunaKit,
    # windows and android off the same UniFFI helper; web and linux paint the
    # copy as unlabelled chrome until their trickle-down lift.
    if app_name(app.driver) not in ("tui", "macos", "ios", "windows", "android"):
        skip_unbuilt(
            app.driver,
            surface=element_id,
            detail="the empty-state copy is not yet carried on the approved id",
            tracked="docs/goal/ui/feed.md § Implementation status today",
        )


def _open_the_chronological_feed(feed) -> None:
    # Compose onto General, whatever a sibling left selected: the other witness
    # leaves the session on its empty custom feed, where a new post never lands
    # (measured on macos: `create_post` timed out at count 0).
    feed.open_feed("General")


def _assert_neither_empty_state(app, why: str) -> None:
    for element_id in ("feed-empty-state", "feed-no-results"):
        assert app.driver.is_absent(element_id), (
            f"{element_id} is painted {why}; "
            f"{app.driver.diagnose(element_id)}; error={app.error_text()!r}"
        )


@pytest.mark.feature("feed-read")
def test_a_feed_with_no_posts_says_so(logged_in_app):
    """A loaded feed holding no posts paints ``feed-empty-state`` with the
    no-posts copy — and never while posts are on screen."""
    app = logged_in_app
    _require_built(app, "feed-empty-state")
    feed = app.feed
    feed.navigate()
    _open_the_chronological_feed(feed)

    # Posts on screen: neither empty state may be painted. The post is the
    # precondition that makes this negative non-vacuous.
    posted = _unique("empty-state-post")
    feed.create_post(text=posted)
    assert feed.wait_for_post_text(posted), (
        f"post {posted!r} never rendered; error={app.error_text()!r}"
    )
    _assert_neither_empty_state(app, "while posts are on screen")

    # A custom feed whose one rule matches nothing on this nest is a feed with
    # no posts in it: the same shared snapshot path as a fresh nest's feed.
    never_posted = _unique("never-posted")
    feed_name = _unique("empty-feed")
    feed.create_feed_with_rule(
        name=feed_name, rule_type="BodyContains", value=never_posted
    )
    feed.open_feed(feed_name)

    assert feed.post_count() == 0, (
        f"feed {feed_name!r} (BodyContains {never_posted!r}) should hold no "
        f"posts, has {feed.post_count()}; error={app.error_text()!r}"
    )
    assert app.driver.is_visible("feed-empty-state"), (
        f"feed {feed_name!r} loaded empty but feed-empty-state is not painted; "
        f"{app.driver.diagnose('feed-empty-state')}; error={app.error_text()!r}"
    )
    assert app.driver.get_text("feed-empty-state") == S.feed.list.no_posts
    assert app.driver.is_absent("feed-no-results"), (
        "feed-no-results is painted with no search active; "
        f"{app.driver.diagnose('feed-no-results')}"
    )


@pytest.mark.feature("feed-search")
def test_a_search_with_no_matches_says_so(logged_in_app):
    """A search the nest answers with no posts paints ``feed-no-results`` with
    the no-matching-posts copy — never the generic ``feed-empty-state`` — and
    clearing the search takes it away again."""
    app = logged_in_app
    _require_built(app, "feed-no-results")
    feed = app.feed
    feed.navigate()
    _open_the_chronological_feed(feed)

    posted = _unique("no-results-post")
    feed.create_post(text=posted)
    assert feed.wait_for_post_text(posted), (
        f"post {posted!r} never rendered; error={app.error_text()!r}"
    )
    _assert_neither_empty_state(app, "while posts are on screen")

    feed.search_feed(_unique("zzz-matches-nothing"))

    assert feed.search_result_count() == 0, (
        f"a search for a never-posted term should match nothing, got "
        f"{feed.search_result_count()} posts; error={app.error_text()!r}"
    )
    assert app.driver.is_visible("feed-no-results"), (
        "the search matched nothing but feed-no-results is not painted; "
        f"{app.driver.diagnose('feed-no-results')}; error={app.error_text()!r}"
    )
    assert app.driver.get_text("feed-no-results") == S.feed.list.no_matching_posts
    assert app.driver.is_absent("feed-empty-state"), (
        "feed-empty-state is painted while a search is active — the search-"
        f"scoped copy must replace it; {app.driver.diagnose('feed-empty-state')}"
    )

    # Clearing restores the posts, and with them neither empty state.
    feed.clear_feed_search()
    assert feed.wait_for_post_text(posted), (
        f"post {posted!r} did not come back after clearing the search; "
        f"error={app.error_text()!r}"
    )
    _assert_neither_empty_state(app, "after the search was cleared")
