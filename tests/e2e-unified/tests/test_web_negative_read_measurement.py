"""tier_3 e2e: what the web bridge's ``/element/visible`` and ``/element/count`` answer
for an element that IS in the DOM but sits below the fold — the running-app
measurement ``PlatformDriver.is_absent``'s default (``not is_visible``) rests on for
web (``e2e-self-diagnosing-failures.md`` convention 6's negative-visibility rider; the
owed per-app measurement is its § Implementation status today).

windows' ``/element/visible`` is ``!IsOffscreen`` — a VIEWPORT predicate — so a
painted-but-offscreen element reads "not visible" there and ``assert not
is_visible(x)`` passes against the very defect it names. web's does not:
``/element/visible`` is Playwright's ``Locator.is_visible()`` (a non-empty bounding
box and no ``visibility:hidden``; ``web-bridge/server.py``), which has no viewport
term, and ``/element/count`` is ``Locator.count()``. So on the feed a below-the-fold
card reads visible, ``count >= 1`` and ``is_absent`` False — the negative read is
exact, and no windows-style override is owed. The element's own measurement
(``getBoundingClientRect``, read page-side) is the control proving the
element really is past ``window.innerHeight``.

The lazy case: the Feed route renders its cards through a plain ``{#each}`` with no
virtual scroller (``apps/fauna-web/src/lib/feed-cues.ts``), so every card is in the DOM
whether or not it is painted — asserted below by ``count("post-card") == POSTS``. A
surface that DID lazily realize rows would read absent for an unrealized row although
the app logically holds it; web has no such surface on the feed, and a negative read on
any future one needs a witness that does not depend on the row being realized.

This pins that against a RUNNING app, where the headless pin
(``test_driver_is_absent.py::test_only_windows_overrides_is_absent``) cannot see a
bridge or SPA regress.

tier_3: a real authenticated web app against a real nest; the feed is seeded through
the same inject seam the other feed tests use (this file measures the DOM's answer,
not a user journey, so convention 8 does not apply).
"""
from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

#: Enough posts that the LAST card is below the fold of the e2e browser viewport.
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


def _frame(driver, element_id: str, card_index: int) -> tuple[float, float, float, float]:
    """The element's ``getBoundingClientRect`` (viewport space), read page-side —
    web's ``get_attr`` does not serve ``frame``; only ``/registry`` does, and that
    lists nothing the bridge's own visibility predicate hides."""
    rect = driver.eval_js(
        "(() => { const card = document.querySelectorAll('[data-testid=\"post-card\"]')"
        f"[{card_index}]; const el = card && card.querySelector('[data-testid=\"{element_id}\"]');"
        " if (!el) return null; const r = el.getBoundingClientRect();"
        " return [r.x, r.y, r.width, r.height]; })()"
    )
    assert rect, (
        f"{element_id!r} in post-card[{card_index}] is not in the DOM at all; "
        f"{driver.diagnose(element_id, scope=f'post-card[{card_index}]')}"
    )
    x, y, w, h = (float(v) for v in rect)
    return x, y, w, h


def test_a_below_the_fold_element_reads_present_and_is_not_absent(logged_in_app):
    """The web answer to the windows trap. A card the viewport's height hides is in
    the DOM — ``is_visible`` True, ``count`` >= 1, ``is_absent`` False — while its
    frame (the control) sits past ``window.innerHeight``. A viewport predicate
    creeping into the bridge (or into ``count`` alone) would turn every negative read
    on a long page into the windows-style vacuous pass."""
    app = logged_in_app
    d = app.driver
    _seed_feed(app, POSTS)
    last_scope = f"post-card[{POSTS - 1}]"

    viewport_h = float(d.eval_js("window.innerHeight"))
    _fx, first_y, _fw, _fh = _frame(d, PROBE, 0)
    _lx, last_y, _lw, last_h = _frame(d, PROBE, POSTS - 1)

    # The premise, MEASURED rather than assumed: the last card really is below the
    # viewport while the first is on it — otherwise the reads below prove nothing.
    assert first_y < viewport_h, (
        f"control: the first card's {PROBE} (y={first_y}) should be inside the "
        f"{viewport_h}px viewport; a frame read that disagrees is a misread, not a finding."
    )
    assert last_y >= viewport_h, (
        f"{POSTS} posts did not push {last_scope} below the fold (y={last_y}, "
        f"viewport height {viewport_h}) — raise POSTS so the reads below measure the "
        "painted-but-offscreen case."
    )

    # The feed is not virtualized: every card is in the DOM (the LAZY case, where an
    # unrealized row reads absent although logically present, does not arise here).
    assert d.count("post-card") == POSTS, (
        f"only {d.count('post-card')}/{POSTS} cards are in the DOM — the feed now "
        "realizes rows lazily, so a negative read on it is vacuous for an unrealized "
        "row, and this test must record the new surface in its docstring."
    )
    assert d.is_visible(PROBE, scope=last_scope), (
        f"an in-DOM-but-offscreen {PROBE} (y={last_y}, h={last_h}, viewport "
        f"{viewport_h}) reads NOT visible — the web bridge now carries a viewport "
        "predicate, which makes a bare negative read vacuous here exactly as on "
        "windows; add the override to drivers/web.py and update "
        "test_driver_is_absent.py::test_only_windows_overrides_is_absent. "
        f"{d.diagnose(PROBE, scope=last_scope)}"
    )
    assert d.count(PROBE, scope=last_scope) >= 1, (
        f"count({PROBE!r}, scope={last_scope!r}) disagrees with is_visible — the two "
        "no longer share one predicate, so count == 0 stops being the same question."
    )
    assert d.is_absent(PROBE, scope=last_scope) is False, (
        f"is_absent answered True for a card that is in the DOM below the fold "
        f"(y={last_y}, viewport {viewport_h})"
    )

    # The negative read is not simply always-False: an element the app has NOT put
    # in the DOM reads absent through the very same path.
    ghost = f"post-card[{POSTS + 40}]"
    assert d.is_absent(PROBE, scope=ghost) is True, (
        f"is_absent({PROBE!r}, scope={ghost!r}) answered False for a card that does "
        f"not exist — the negative form cannot tell present from absent. "
        f"{d.diagnose(PROBE, scope=ghost)}"
    )
