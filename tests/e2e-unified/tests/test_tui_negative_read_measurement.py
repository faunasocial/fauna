"""tier_3 e2e: what the tui agent's ``/element/visible`` and ``/element/count`` answer
for an element that IS in the app's tree but sits below the terminal's fold — the
running-app measurement ``PlatformDriver.is_absent``'s default (``not is_visible``)
rests on for tui (``e2e-self-diagnosing-failures.md`` convention 6's negative-
visibility rider; the owed per-app measurement is its § Implementation status today).

windows' ``/element/visible`` is ``!IsOffscreen`` — a VIEWPORT predicate — so a
painted-but-offscreen element reads "not visible" there and ``assert not
is_visible(x)`` passes against the very defect it names. tui's does not: the
registry is never viewport-clipped (``apps/fauna-tui/src/automation.rs``,
``ElementKind::Visible`` is ``registry.matches(..).next().is_some()`` and ``Count``
the same query's length; ``tui.md``: "the element list is the registry; the
viewport clips paint only"). So on the current page a below-the-fold element reads
visible, ``count >= 1`` and ``is_absent`` False — the negative read is exact, and no
windows-style override is owed. The frame's own measurement is the separate
``in-viewport`` attribute (``TuiDriver.in_viewport``), which this file uses as the
control proving the element really is off the painted band.

This pins that against a RUNNING app, where the headless pin of the predicate
(``test_driver_is_absent.py::test_only_windows_overrides_is_absent``) cannot see an
agent regress. The mirror: an element on a page that is NOT current is unregistered,
so it reads absent — correct for "the other page is not showing", vacuous for "this
row was not added beyond the fold"; a negative read there needs the state protocol.

tier_3: a real tui against a real nest; the feed is seeded through the same inject
seam the other feed tests use (this file measures the REGISTRY's answer, not a user
journey, so convention 8 does not apply).
"""
from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

#: Enough posts that the LAST card is below the fold of the e2e terminal.
POSTS = 40

#: An id that exists once inside every ``post-card``.
PROBE = "post-author"


def _seed_feed(app, count: int) -> None:
    posts = [
        {
            "post_id": f"negread-seed-{i:02d}",
            "author": "negread-seed-author",
            "body": f"negative-read measurement seed post number {i}",
        }
        for i in range(count)
    ]
    seeded = app.feed.seed_posts(posts)
    assert seeded == count, (
        f"seeded {count} posts but the feed rendered {seeded}; error={app.error_text()!r}"
    )


def test_a_below_the_fold_element_reads_present_and_is_not_absent(logged_in_app):
    """The tui answer to the windows trap. A card the terminal's height hides is in
    the registry — ``is_visible`` True, ``count`` >= 1, ``is_absent`` False — while
    ``in_viewport`` (the frame's measurement) says it is off the painted band. A
    viewport predicate creeping into the registry query would turn every negative
    read on a long page into the windows-style vacuous pass."""
    app = logged_in_app
    d = app.driver
    _seed_feed(app, POSTS)
    last_scope = f"post-card[{POSTS - 1}]"

    # The premise, MEASURED rather than assumed: the last card is really off the
    # painted band while the first is on it — otherwise the reads prove nothing.
    assert d.in_viewport(PROBE, scope="post-card[0]"), (
        "control: the first card's post-author should be inside the painted band; "
        f"{d.diagnose(PROBE, scope='post-card[0]')}"
    )
    assert not d.in_viewport(PROBE, scope=last_scope), (
        f"{POSTS} posts did not push {last_scope} below the fold — raise POSTS so "
        f"the reads below measure the registered-but-offscreen case; "
        f"{d.diagnose(PROBE, scope=last_scope)}"
    )

    assert d.is_visible(PROBE, scope=last_scope), (
        f"a registered-but-offscreen {PROBE} reads NOT visible — tui now carries a "
        "viewport predicate, which makes a bare negative read vacuous here exactly "
        "as on windows; add the override to drivers/tui.py and update "
        "test_driver_is_absent.py::test_only_windows_overrides_is_absent; "
        f"{d.diagnose(PROBE, scope=last_scope)}"
    )
    assert d.count(PROBE, scope=last_scope) >= 1, (
        f"count({PROBE!r}, scope={last_scope!r}) disagrees with is_visible — the two "
        "no longer share one predicate, so count == 0 stops being the same question."
    )
    assert d.is_absent(PROBE, scope=last_scope) is False, (
        f"is_absent answered True for a card that is registered below the fold; "
        f"{d.diagnose(PROBE, scope=last_scope)}"
    )

    # The negative read is not simply always-False: an element the app has NOT put
    # in its tree reads absent through the very same path.
    ghost = f"post-card[{POSTS + 40}]"
    assert d.is_absent(PROBE, scope=ghost) is True, (
        f"is_absent({PROBE!r}, scope={ghost!r}) answered False for a card that does "
        f"not exist — the negative form cannot tell present from absent; "
        f"{d.diagnose(PROBE, scope=ghost)}"
    )
