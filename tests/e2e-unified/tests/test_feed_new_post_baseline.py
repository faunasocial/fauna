"""Unit tests for the new-post wait's BASELINE (tier_1).

`FeedActions._wait_for_new_post` confirms a caller that has the body by IDENTITY
(the typed text on any rendered card); only a caller without the body falls back
to the list's count rising above a baseline read before submitting. That count
is only a question about the caller's OWN post when the baseline came from a
list that had already loaded.

On web it often had not. The Feed page enables its composer at `feedReady`,
before its `loadAll()` runs four reads and the local-feed query, and every test
mounts the page fresh (the per-test `reset` is a hard reload). A baseline read
right after the composer appeared therefore counted an empty list; the page's
own load then painted the posts earlier journeys left behind, the count rose,
and the wait returned before the new post existed — so `test_post_detail_opens`
opened the previous test's post, and the c2pa badge test typed its second post
while the first was still uploading.

These pin `_settled_post_count`: on web the baseline is read only after the feed
manager's first query has committed (its `feed_reloads` counter, convention 14's
causal anchor), and on every other app it is the plain count it always was.
"""

import pytest

pytestmark = pytest.mark.tier_1

from actions.feed import FeedActions


class _LoadingWebDriver:
    """A web page whose feed query commits after a few polls.

    Before the commit the list is empty and `feed_reloads` reports no committed
    generation; from the commit on, the list holds the posts earlier journeys
    left behind — exactly the fresh-mount shape the baseline must not sample."""

    def __init__(self, polls_until_commit: int, loaded_count: int) -> None:
        self._polls_left = polls_until_commit
        self._loaded_count = loaded_count
        self.committed = polls_until_commit == 0

    def is_web(self) -> bool:
        return True

    def get_state(self, key: str | None = None):
        assert key == "feed_reloads", f"unexpected state read: {key!r}"
        if self._polls_left > 0:
            self._polls_left -= 1
            return {"started": 1, "completed": 0, "committed_gen": 0}
        self.committed = True
        return {"started": 1, "completed": 1, "committed_gen": 1}

    def count(self, element_id: str, **_kw) -> int:
        assert element_id == "feed-post-text"
        return self._loaded_count if self.committed else 0


class _NativeDriver:
    """Every non-web app: the plain count, and no state read at all."""

    def __init__(self, count: int) -> None:
        self._count = count

    def is_web(self) -> bool:
        return False

    def get_state(self, key: str | None = None):
        raise AssertionError("a non-web baseline must not read the reload counter")

    def count(self, element_id: str, **_kw) -> int:
        return self._count


def test_web_baseline_is_read_from_the_loaded_list_not_the_empty_one():
    driver = _LoadingWebDriver(polls_until_commit=3, loaded_count=7)
    baseline = FeedActions(driver)._settled_post_count()
    # Read before the commit, this would be 0 — and any of the 7 old posts
    # painting in afterwards would then "prove" a new post had landed.
    assert baseline == 7


def test_web_baseline_on_an_already_loaded_page_returns_at_once():
    driver = _LoadingWebDriver(polls_until_commit=0, loaded_count=4)
    assert FeedActions(driver)._settled_post_count() == 4


def test_non_web_baseline_is_the_plain_count():
    assert FeedActions(_NativeDriver(count=5))._settled_post_count() == 5


class _IdentityFeed(FeedActions):
    """A feed pinned at its display cap whose top card is NOT the new post.

    The count never grows and the top card is an older post — the shape a
    shared nest at the 50-post page with a ranked feed selected presents."""

    def __init__(self, visible_texts: list[str]) -> None:
        self._texts = visible_texts
        self.driver = type("D", (), {"is_visible": lambda self, _id: False})()

    def post_text_visible(self, text: str) -> bool:
        return any(text in t for t in self._texts)

    def first_post_text(self) -> str:
        return self._texts[0]

    def post_count(self) -> int:
        return 50

    def is_visible(self) -> bool:
        return True

    def _compose_console_tail(self) -> str:
        return ""


def test_new_post_is_confirmed_by_identity_not_top_position_or_count():
    feed = _IdentityFeed(["trending-keep-gated-bb13e8bb", "the typed body"])
    # Count is flat at the cap and the top card is another post: still found.
    feed._wait_for_new_post(50, expected_text="the typed body", timeout=2)


def test_absent_identity_times_out_even_if_count_would_have_grown():
    feed = _IdentityFeed(["trending-keep-gated-bb13e8bb"])
    feed.post_count = lambda: 51  # type: ignore[method-assign]
    with pytest.raises(TimeoutError, match="on any card"):
        feed._wait_for_new_post(50, expected_text="the typed body", timeout=0.5)
