"""Feed search tests — a search is a RE-QUERY of the nest, never a client-side
filter (``docs/goal/ui/feed.md`` § Where logic lives → *Search filter*).

``FeedActions.search_feed`` returns only once the feed has re-queried for the
term and that re-query has committed (the ``feed_reloads`` barrier), so every
test here also fails on an app that narrows its loaded posts locally: such an
app starts no reload, and the barrier refuses by name.
"""
import uuid
import time
import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


@pytest.mark.feature("feed-search")
def test_feed_search_filters_posts(logged_in_app):
    """Search filters posts by body text."""
    # Create posts with distinct text
    needle = _unique("needle")
    haystack = _unique("haystack")
    logged_in_app.feed.create_post(text=f"Finding the {needle} in posts")
    logged_in_app.feed.create_post(text=f"This is just {haystack}")

    initial_count = logged_in_app.feed.post_count()
    assert initial_count >= 2

    # Search for the needle — returns once the nest's narrowed page landed.
    logged_in_app.feed.search_feed(needle)

    filtered_count = logged_in_app.feed.search_result_count()
    assert filtered_count >= 1
    assert filtered_count < initial_count

    # Verify the matching post is visible
    found = False
    for i in range(filtered_count):
        text = logged_in_app.feed.post_text(i)
        if needle in text:
            found = True
            break
    assert found, f"Expected to find post containing '{needle}'"

    # Leave the shared `logged_in_app` unfiltered: this module's other tests
    # create+read posts against the SAME session, so a search left active here
    # narrows their feed and makes them order-dependent (brittle-tests duty 2 —
    # assert on latency-independent state, never on a neighbour's leftover UI).
    logged_in_app.feed.clear_feed_search()


@pytest.mark.feature("feed-search")
def test_feed_search_clear_restores_feed(logged_in_app):
    """Clearing search restores the full post list.

    ``create_post``/``search_feed`` already wait for their own effect to land
    (post-appear / the search's committed re-query); ``clear_feed_search`` does not, so a
    fixed-delay sleep there under-shot on a loaded machine (harness bug, not a
    product gap — assert latency-independent state via a deadline poll, never
    a bare sleep-then-assert).
    """
    text = _unique("cleartest")
    logged_in_app.feed.create_post(text=text)

    initial_count = logged_in_app.feed.post_count()
    assert initial_count >= 1

    # Search for something unlikely
    logged_in_app.feed.search_feed("zzz-nonexistent-zzz")

    # Clear search
    logged_in_app.feed.clear_feed_search()
    deadline = time.time() + 10.0
    restored_count = logged_in_app.feed.post_count()
    while time.time() < deadline and restored_count != initial_count:
        time.sleep(0.3)
        restored_count = logged_in_app.feed.post_count()
    assert restored_count == initial_count


@pytest.mark.feature("feed-search")
def test_feed_search_by_tag(logged_in_app):
    """Search matches posts by tag content.

    ``create_post_with_tags`` already waits for the post to land; poll for the
    search result rather than a bare sleep-then-assert (same convention-14
    hardening as ``test_feed_search_clear_restores_feed`` above).
    """
    unique_tag = _unique("tag")
    text = _unique("tagged-search")
    logged_in_app.feed.create_post_with_tags(text=text, tags=unique_tag)

    logged_in_app.feed.search_feed(unique_tag)
    deadline = time.time() + 10.0
    count = logged_in_app.feed.search_result_count()
    while time.time() < deadline and count < 1:
        time.sleep(0.3)
        count = logged_in_app.feed.search_result_count()
    assert count >= 1
