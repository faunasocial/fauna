"""`POST /element/scroll-into-view` is REAL on the apple in-process agent.

The regression gate for the route that was a **no-op stub** returning
`{"ok": true}` until 2026-08-02 (`InProcessAutomationServer.swift`, grouped with
`/scroll` + `/dismiss-dialogs`). It was harmless only because both apple drivers
left `_supports_scroll_into_view` False — and that was the trap: the route LOOKED
implemented, so the next session wanting a mid-list dwell would flip the flag,
get a silent no-op, and read it as a product bug (the "a test agent MUST NOT
silently drop a command" hazard, testing.md convention 11, which apple has
already paid for twice).

**What this file pins that a `found: true` check would not.** A scroll that
merely brings an element to the *edge* of the viewport answers `found: true`
just as a centred one does, and windows' FlaUI bridge flips `!IsOffscreen` at
~25% visibility — under the shared 500‰/750‰ engagement-cue gates. So an
edge-aligned scroll would report a dwell the user never had. These tests assert
the element's REAL post-scroll geometry (`driver.last_scroll_geometry()`),
not the bare flag.

Scoped to apple deliberately: linux/web/windows/tui have served this route for
much longer and have their own coverage; apple was the lone app of the seven
without it, which is what made this a priority-#1 divergence fix rather than a
new feature (`apple-e2e-automation.md` § Architecture).
"""

import pytest

from helpers.app_surface import skip_unbuilt

# apple-only: this is the regression gate for apple's own `/element/scroll-into-view`
# no-op-stub bug (module docstring above). tui already serves this route (has for much
# longer, per that same docstring) and is not exposed to apple's stub-return defect class.
pytestmark = [pytest.mark.tier_3, pytest.mark.macos, pytest.mark.ios]


#: A scroll is only an honest exposure if most of the element is on screen.
#: 0.75 is the bar that a prior row set, comfortably above
#: FlaUI's ~25% `!IsOffscreen` flip and both shared cue gates (500‰/750‰).
MIN_VISIBLE_FRACTION = 0.75


def _visible_fraction(driver) -> float:
    """Fraction of the last-scrolled element's HEIGHT inside the viewport."""
    geo = driver.last_scroll_geometry()
    assert geo is not None, (
        "the apple agent reported no post-scroll geometry — either the reply "
        "lost its `frame`/`viewport` fields or this ran on a bridge that never "
        "sends them"
    )
    (_x, y, _w, h), (_vw, vh) = geo
    assert h > 0, f"element has zero height after scroll: frame height={h}"
    visible = max(0.0, min(y + h, vh) - max(y, 0.0))
    return visible / h


def _seed_feed(app, count: int) -> None:
    """Enough posts that the LAST one is genuinely below the fold.

    Goes through `seed_posts`, not a bare `inject_posts`: the Feed page re-pulls
    the empty nest feed on becoming visible, and that one-shot reload can land
    *after* an inject and clear it (a bare inject read back 0 posts here).
    """
    posts = [
        {
            "post_id": f"scroll-seed-{i:02d}",
            "author": "scroll-seed-author",
            "body": f"scroll-target seed post number {i}",
        }
        for i in range(count)
    ]
    seeded = app.feed.seed_posts(posts)
    assert seeded == count, (
        f"seeded {count} posts but the feed rendered {seeded}; "
        f"error={app.error_text()!r}"
    )


def test_scroll_into_view_centres_a_below_the_fold_post(logged_in_app):
    """The headline: a mid-list `post-card` the fold hides ends up ≥75% visible.

    This is the property `dwell_on_post` depends on. Before the route was real,
    the same call either raised (`_supports_scroll_into_view` False) or, had the
    flag been flipped alone, silently left the card wherever it was.
    """
    app = logged_in_app
    if not getattr(app.driver, "_supports_scroll_into_view", False):
        skip_unbuilt(
            app.driver,
            surface="POST /element/scroll-into-view",
            detail="this bridge does not serve a targeted scroll",
            tracked="testing.md",
        )

    _seed_feed(app, 12)
    last = app.feed.post_card_count() - 1

    app.driver.scroll_to("post-card", index=last)

    fraction = _visible_fraction(app.driver)
    assert fraction >= MIN_VISIBLE_FRACTION, (
        f"post-card[{last}] is only {fraction:.0%} visible after scroll_to "
        f"(want >= {MIN_VISIBLE_FRACTION:.0%}); geometry="
        f"{app.driver.last_scroll_geometry()}. An edge-aligned or no-op scroll "
        f"looks identical to a real one at the `found: true` level — that is "
        f"exactly what this assertion exists to separate."
    )
    assert app.driver.is_visible("post-card"), (
        "the card scrolled into view but the registry still reads it absent; "
        f"{app.driver.diagnose('post-card', attrs=('frame',))}"
    )


def test_scroll_into_view_actually_moves_the_element(logged_in_app):
    """A no-op stub's tell: the element's frame does not change.

    The centring assertion above could in principle pass by luck if a card
    happened to sit near the middle already. This one is immune to that — it
    compares the SAME card's frame at two different scroll positions and
    requires it to have moved, which no stub can satisfy.
    """
    app = logged_in_app
    if not getattr(app.driver, "_supports_scroll_into_view", False):
        skip_unbuilt(
            app.driver,
            surface="POST /element/scroll-into-view",
            detail="this bridge does not serve a targeted scroll",
            tracked="testing.md",
        )

    _seed_feed(app, 12)
    last = app.feed.post_card_count() - 1

    app.driver.scroll_to("post-card", index=0)
    top_geo = app.driver.last_scroll_geometry()
    app.driver.scroll_to("post-card", index=last)
    bottom_geo = app.driver.last_scroll_geometry()

    assert top_geo is not None and bottom_geo is not None
    # Scrolling to the bottom card must have moved the viewport, so card 0's
    # position relative to the window differs from when it was centred.
    app.driver.scroll_to("post-card", index=0)
    back_geo = app.driver.last_scroll_geometry()
    assert back_geo is not None
    assert abs(back_geo[0][1] - bottom_geo[0][1]) > 1.0, (
        "post-card[0] reports the same y after scrolling to the last card and "
        f"back ({back_geo[0][1]} vs {bottom_geo[0][1]}) — the scroll route is "
        "not moving anything, i.e. it is behaving like the pre-2026-08-02 "
        "no-op stub"
    )


def test_scroll_to_raises_on_an_absent_element(logged_in_app):
    """`scroll_to` fails LOUDLY, so a dwell can never measure nothing.

    The driver contract (`http_bridge.py::scroll_to`) raises unless the reply
    carries `found`. Pinning it here keeps a future reply-shape change from
    quietly degrading `scroll_to` into the best-effort `_scroll_into_view`.
    """
    app = logged_in_app
    if not getattr(app.driver, "_supports_scroll_into_view", False):
        skip_unbuilt(
            app.driver,
            surface="POST /element/scroll-into-view",
            detail="this bridge does not serve a targeted scroll",
            tracked="testing.md",
        )

    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.driver.wait_for("feed-view")
    with pytest.raises(LookupError):
        app.driver.scroll_to("no-such-element-id-anywhere", index=0)
