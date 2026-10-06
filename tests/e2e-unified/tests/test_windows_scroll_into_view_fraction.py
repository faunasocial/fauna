"""FlaUI-bridge windows e2e test infra: scroll-to-measured-visibility-fraction
primitive.

UIA's ``IsOffscreen`` — which both ``POST /element/scroll-into-view`` and
``wait_for``'s targeted scroll rely on — flips false as soon as ANY sliver of
an element enters the viewport (empirically as low as ~25% in this feed's
post-card layout, traced live while building the windows engagement-cue
capture shell, `test_engagement_cues.py`'s windows dwell test docstring).
A caller that needs a specific mid-list visibility fraction (not merely
"on screen at all") has had no primitive to reach for.
``Actions.ScrollIntoViewFraction`` / ``WindowsBridgeDriver.
scroll_to_visible_fraction`` is that primitive: it measures the actual
overlap between an element's bounding rect and its scroll container's
viewport rect, and sweeps until that MEASURED fraction — not
``IsOffscreen`` — crosses the caller's threshold.

tier_3: a real app + real nest binary (feed page render, real scroll
geometry). Posts are seeded via the ``feed_inject_posts`` test-state command
(a client-side test seam, no nest round trip per post) — this test proves
the SCROLL PRIMITIVE itself, not feed composition.
"""
import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# Long enough, and enough of them, that the list overflows the window and a
# mid-list card starts genuinely off (or only partially on) screen — windows'
# non-virtualizing PostsList (`FeedPage.xaml`) keeps every card realized in
# the UIA tree regardless of scroll position, so a mid-list index is always
# findable; only its ON-SCREEN FRACTION depends on scrolling.
_POST_COUNT = 20
_MID_INDEX = 10


def _seed_many_posts(app) -> None:
    posts = [
        {
            "post_id": f"scroll-fraction-{i}",
            "author": "e2e-user",
            "body": f"post number {i} — enough body text that each card takes real vertical space in the feed",
        }
        for i in range(_POST_COUNT)
    ]
    # `seed_posts` (not a bare `inject_posts` + `wait_post_count`) — the Feed
    # page re-pulls the (empty) nest feed every time it becomes visible, an
    # async reload that can land AFTER a single inject and clear it;
    # `seed_posts` re-injects until the count sticks rather than racing that
    # one-shot reload. A bare `wait_post_count` return is never asserted by
    # its own caller, so a short count would silently make a later
    # `index=_MID_INDEX` lookup "not found" instead of failing loudly here.
    count = app.feed.seed_posts(posts, timeout=20.0)
    assert count == _POST_COUNT, (
        f"expected {_POST_COUNT} seeded posts, feed rendered {count}; "
        f"error={app.error_text()!r}"
    )


def test_scroll_to_visible_fraction_hits_controlled_thresholds(logged_in_app):
    """Scroll a mid-list post to two different controlled visibility
    fractions and assert the primitive actually reaches each one — proving
    it is a real MEASURED-fraction scroll, not the binary any-sliver-visible
    check ``scroll_to``/``wait_for`` are limited to."""
    app = logged_in_app
    _seed_many_posts(app)

    # A LOW threshold: the primitive must land AT LEAST 30% visible.
    low_fraction = app.driver.scroll_to_visible_fraction(
        "post-card", 0.3, index=_MID_INDEX
    )
    assert low_fraction >= 0.3, (
        f"scroll_to_visible_fraction(0.3) landed at {low_fraction:.2f}, "
        f"short of the requested threshold; error={app.error_text()!r}"
    )

    # A HIGH threshold on the SAME card: the primitive must land AT LEAST
    # 90% visible — a target the old binary IsOffscreen check could satisfy
    # by accident at ~25% and never verify.
    high_fraction = app.driver.scroll_to_visible_fraction(
        "post-card", 0.9, index=_MID_INDEX
    )
    assert high_fraction >= 0.9, (
        f"scroll_to_visible_fraction(0.9) landed at {high_fraction:.2f}, "
        f"short of the requested threshold; error={app.error_text()!r}"
    )


def test_scroll_to_visible_fraction_raises_on_missing_element(logged_in_app):
    """Fail-loud contract (mirrors the base ``scroll_to``): a bogus element
    id must raise, never silently report a fraction for nothing found."""
    app = logged_in_app
    _seed_many_posts(app)

    with pytest.raises(LookupError):
        app.driver.scroll_to_visible_fraction("post-card-does-not-exist", 0.5)
