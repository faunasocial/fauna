"""FlaUI-bridge windows e2e test infra: the targeted scrolls bring a card in
WHOLE, and the measured one reports what the app measures.

UIA's ``IsOffscreen`` flips false as soon as ANY sliver of an element enters
the viewport, and WinUI clips an element's UIA ``BoundingRectangle`` to its
``ScrollViewer`` — so a card a quarter on screen reads, to the bridge, as a
smaller card wholly on screen. Before row 3662 both targeted scrolls trusted
those readings: ``scroll_to`` stopped at the first sliver, and
``scroll_to_visible_fraction`` divided the clipped rect by itself, answering
1.0 for any visible part of a card (the old threshold test passed only
because every visible card measured whole). Both now centre the element
until its rect stops growing as it moves (``Actions.MakeWhole``; the
arithmetic is ``ScrollPolicy``'s, pinned by ``--self-test-scroll-policy``).

So every assertion here reads the share that shows from the APP's own
capture geometry (``FeedActions.post_card_visible_share``: the engagement-cue
probe, which measures each card's laid-out height, unclipped), never from
the bridge that placed the card.

The read-only ``in-viewport`` attribute (``driver.in_viewport``, the
"brought into view" observable of ``conversations.md`` § The selected message)
sits on the same clipped rects and cannot move the list for a second reading,
so until 2026-11-06 it answered from the clipped rect's centre and read a
sliver as in view. It now answers only once both of the rect's
edges are proven the element's own, and otherwise refuses
(``ViewportUndecided``); two tests pin both halves.

The bottom-edge placement (``scroll_to_bottom_fraction``, the other side of
the gate) learns the card's true height the same way, then leaves the asked
share above the feed's bottom edge; its tests read the reached share from the
app's capture geometry too.

tier_3: a real app + real nest binary (feed page render, real scroll
geometry). Posts are seeded via the ``feed_inject_posts`` test-state command
(a client-side test seam, no nest round trip per post) — this test proves
the SCROLL PRIMITIVES themselves, not feed composition.
"""
import pytest

from drivers.base import ViewportUndecided

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# Long enough, and enough of them, that the list overflows the window and a
# mid-list card starts genuinely off (or only partially on) screen — windows'
# non-virtualizing PostsList (`FeedPage.xaml`) keeps every card realized in
# the UIA tree regardless of scroll position, so a mid-list index is always
# findable; only its ON-SCREEN FRACTION depends on scrolling.
_POST_COUNT = 20
_MID_INDEX = 10

# A card a reader has scrolled mostly out of view — the sliver the old
# scrolls stopped at (28 % measured 2026-11-04).
_SLIVER = 0.25

# Whole, give or take the probe's sub-pixel rounding.
_WHOLE = 0.98

# How far the bridge's reported fraction may sit from the app's own reading.
_AGREE = 0.05


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


def _place_sliver(app) -> float:
    """Leave the mid-list card a sliver above the bottom edge, through the
    app's own pixel step — never through the bridge under test."""
    share = app.feed.place_post_at_bottom_edge_by_pixel_step(_MID_INDEX, _SLIVER)
    assert 0.05 < share < 0.5, (
        f"the pixel step left post-card[{_MID_INDEX}] {share:.0%} visible, not a "
        f"sliver — the setup cannot exercise a partly hidden card; "
        f"error={app.error_text()!r}"
    )
    return share


def test_scroll_to_brings_a_partly_hidden_card_in_whole(logged_in_app):
    """The base driver's ``scroll_to`` contract (centre, whole element) on a
    card a reader has scrolled mostly out of view: UIA calls it on screen
    already, and the app's own geometry must still read it whole after the
    scroll — at least the long-dwell share (75 %) every dwell journey needs,
    and in fact all of it, since a post-card fits the 600 × 600 window."""
    app = logged_in_app
    _seed_many_posts(app)
    before = _place_sliver(app)

    app.driver.scroll_to("post-card", index=_MID_INDEX)

    share = app.feed.post_card_visible_share(_MID_INDEX)
    assert share >= _WHOLE, (
        f"scroll_to(post-card[{_MID_INDEX}]) left the card {share:.0%} visible "
        f"by the app's own measurement (it started at {before:.0%}) — the scroll "
        f"stopped short of the whole card; error={app.error_text()!r}"
    )


def test_scroll_to_visible_fraction_reports_what_the_app_measures(logged_in_app):
    """The measured scroll's reply is a measurement: from a sliver, it must
    reach the asked share AND report a fraction the app's own geometry agrees
    with — a clipped rect divided by itself would answer 1.0 for the sliver
    and never move it."""
    app = logged_in_app
    _seed_many_posts(app)
    before = _place_sliver(app)

    reported = app.driver.scroll_to_visible_fraction("post-card", 0.75, index=_MID_INDEX)

    share = app.feed.post_card_visible_share(_MID_INDEX)
    assert abs(reported - share) <= _AGREE, (
        f"scroll_to_visible_fraction reported {reported:.0%} for post-card"
        f"[{_MID_INDEX}], the app measures {share:.0%} (it started at "
        f"{before:.0%}) — the bridge's reading is not the card's; "
        f"error={app.error_text()!r}"
    )
    assert share >= 0.75, (
        f"scroll_to_visible_fraction(0.75) left post-card[{_MID_INDEX}] "
        f"{share:.0%} visible by the app's own measurement; "
        f"error={app.error_text()!r}"
    )


def test_scroll_to_visible_fraction_hits_controlled_thresholds(logged_in_app):
    """Scroll an off-screen mid-list post to two controlled visibility
    thresholds and assert each reply against the app's own geometry — a
    first sliver entering the viewport must not answer the low threshold as
    a whole card."""
    app = logged_in_app
    _seed_many_posts(app)

    for threshold in (0.3, 0.9):
        reported = app.driver.scroll_to_visible_fraction(
            "post-card", threshold, index=_MID_INDEX
        )
        share = app.feed.post_card_visible_share(_MID_INDEX)
        assert reported >= threshold and share >= threshold, (
            f"scroll_to_visible_fraction({threshold}) reported {reported:.0%}, the "
            f"app measures {share:.0%} — short of the requested threshold; "
            f"error={app.error_text()!r}"
        )
        assert abs(reported - share) <= _AGREE, (
            f"scroll_to_visible_fraction({threshold}) reported {reported:.0%}, the "
            f"app measures {share:.0%}; error={app.error_text()!r}"
        )


def test_in_viewport_does_not_read_a_sliver_as_in_view(logged_in_app):
    """A card the app left a sliver above the bottom edge has its true centre
    below the band — the app's own geometry measures well under half of it
    showing — so ``in_viewport`` must not read it as in view. The clipped rect
    is a smaller card wholly inside the band, whose centre always lies inside
    it: that is what this read answered before. Without moving the list the
    bridge cannot learn the card's true height, so a refusal
    (``ViewportUndecided``) is as honest as ``False``; ``True`` is the bug."""
    app = logged_in_app
    _seed_many_posts(app)
    share = _place_sliver(app)

    try:
        answer = app.driver.in_viewport("post-card", index=_MID_INDEX)
    except ViewportUndecided as undecided:
        answer = undecided
    assert answer is not True, (
        f"in_viewport(post-card[{_MID_INDEX}]) read a card {share:.0%} on screen by "
        f"the app's own measurement as in view — its true centre is below the band, "
        f"so the read answered from the clipped rect's centre; "
        f"error={app.error_text()!r}"
    )


def test_in_viewport_reads_a_centred_card_as_in_view(logged_in_app):
    """The other half: a card brought in whole and centred (``scroll_to``,
    the app's own geometry confirming it whole) is proven whole by one
    reading clear of both edges, and reads in view — the refusal is kept for
    a reading that may be clipped, never spent on one that cannot be."""
    app = logged_in_app
    _seed_many_posts(app)
    _place_sliver(app)
    app.driver.scroll_to("post-card", index=_MID_INDEX)
    share = app.feed.post_card_visible_share(_MID_INDEX)
    assert share >= _WHOLE, (
        f"scroll_to(post-card[{_MID_INDEX}]) left the card {share:.0%} visible, so "
        f"this test cannot ask about a whole card; error={app.error_text()!r}"
    )

    assert app.driver.in_viewport("post-card", index=_MID_INDEX) is True, (
        f"a whole, centred post-card[{_MID_INDEX}] did not read as in view; "
        f"error={app.error_text()!r}"
    )


#: The bottom-edge placement's asked share: the quarter the engagement-cue
#: journey leaves under the 50 % skip gate.
_BOTTOM_QUARTER = 0.25

#: How far the app's own reading of a bottom-edge placement may sit from the
#: asked share — a mid-list card has far more content above it than the
#: placement needs, so the list can always scroll it there.
_PLACED = 0.06


def _assert_placed_at_bottom(app, asked: float, reported: float, started: str) -> None:
    share = app.feed.post_card_visible_share(_MID_INDEX)
    assert abs(reported - share) <= _AGREE, (
        f"scroll_to_bottom_fraction({asked}) reported {reported:.0%} for post-card"
        f"[{_MID_INDEX}], the app measures {share:.0%} (it started {started}) — the "
        f"bridge's reading is not the card's; error={app.error_text()!r}"
    )
    assert abs(share - asked) <= _PLACED, (
        f"scroll_to_bottom_fraction({asked}) left post-card[{_MID_INDEX}] {share:.0%} "
        f"visible by the app's own measurement (it started {started}) — not at the "
        f"asked share above the bottom edge; error={app.error_text()!r}"
    )


def test_scroll_to_bottom_fraction_places_a_whole_card_at_the_bottom_edge(logged_in_app):
    """The bottom-edge placement (``e2e-conventions.md`` § Driver primitives →
    *Placing an element at a measured fraction*) from a whole, centred card:
    a quarter of it is left above the feed's bottom edge, and the bridge's
    answer agrees with the app's own capture geometry — never the clipped
    rect over itself, which reads the quarter as 1.0."""
    app = logged_in_app
    _seed_many_posts(app)
    app.driver.scroll_to("post-card", index=_MID_INDEX)

    reported = app.driver.scroll_to_bottom_fraction(
        "post-card", _BOTTOM_QUARTER, index=_MID_INDEX
    )

    _assert_placed_at_bottom(app, _BOTTOM_QUARTER, reported, "whole and centred")


def test_scroll_to_bottom_fraction_places_a_sliver_at_the_asked_share(logged_in_app):
    """From a sliver the app left above the bottom edge, whose clipped rect
    cannot say how tall the card is: the placement must learn the true height
    first, then leave the asked share showing — more than the sliver here, so
    the list moves the other way."""
    app = logged_in_app
    _seed_many_posts(app)
    before = _place_sliver(app)

    reported = app.driver.scroll_to_bottom_fraction("post-card", 0.4, index=_MID_INDEX)

    _assert_placed_at_bottom(app, 0.4, reported, f"a {before:.0%} sliver")


def test_scroll_to_bottom_fraction_raises_on_missing_element(logged_in_app):
    """The same fail-loud contract as every measured scroll."""
    app = logged_in_app
    _seed_many_posts(app)

    with pytest.raises(LookupError):
        app.driver.scroll_to_bottom_fraction("post-card-does-not-exist", 0.25)


def test_scroll_to_visible_fraction_raises_on_missing_element(logged_in_app):
    """Fail-loud contract (mirrors the base ``scroll_to``): a bogus element
    id must raise, never silently report a fraction for nothing found."""
    app = logged_in_app
    _seed_many_posts(app)

    with pytest.raises(LookupError):
        app.driver.scroll_to_visible_fraction("post-card-does-not-exist", 0.5)
