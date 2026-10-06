"""tier_3 e2e: what apple's ``/element/visible`` and ``/element/count`` answer for
an element that IS painted but sits below the fold — the running-app measurement
``PlatformDriver.is_absent``'s default (``not is_visible``) rests on for macos + ios
(e2e-conventions.md convention 6's rider; apple-e2e-automation.md rules 2a + 6).

windows' ``/element/visible`` is ``!IsOffscreen`` — a VIEWPORT predicate — so a
painted-but-offscreen element reads "not visible" there and ``assert not
is_visible(x)`` passes against the very defect it names. apple's does not: the
registry decides an attached slot by its frame's **x-origin alone**
(``AutomationRegistry.swift`` ``SentinelGeometry.isOnScreen``; vertical is
deliberately excluded, rule 2a), and ``/element/count`` filters through the SAME
``visibleSlots``. So on an eagerly-realized page (rule 6's ``ScrollView { VStack }``)
a below-the-fold element reads visible, ``count == 1``, and ``is_absent`` False — the
negative read is exact, and no windows-style override is owed. This file pins that
against a RUNNING app, where the headless ``AutomationRegistryTests`` pin of the
predicate cannot see a bridge or a layout regress.

The mirror is NOT measured here and ``is_absent`` cannot see it: an element a LAZY
container has not realized (an iOS ``List``/``Form`` row below the fold — rule 6's
first sentence) never registers, so it reads absent although the app logically
holds it, and a negative read on that surface is vacuous. Where a test's negative
read lives on such a surface it needs a witness that does not depend on the row
being realized; the measurement and the per-read consequence are recorded in this
project's e2e coordination log.

tier_3: a real authenticated app against a real nest; the feed is seeded through
the same inject seam ``test_apple_scroll_into_view.py`` uses (this file measures the
REGISTRY's answer, not a user journey, so convention 8 does not apply).
"""
from __future__ import annotations

import re

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.macos, pytest.mark.ios]

#: Enough posts that the LAST card is below the fold on both apps' windows.
POSTS = 12

#: An id that exists once inside every ``post-card`` (``PostCardBody`` registers it
#: with ``automationText``), so ``post-card[N]`` scopes a real, single element.
PROBE = "post-author"

#: ``/tree`` slot line: ``[i] VISIBLE(geo) geo=(x,y wxh in WxH) votes=… path=…``.
_GEO = re.compile(r"geo=\((-?\d+),(-?\d+) (\d+)x(\d+) in (\d+)x(\d+)\)")


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


def _tree_block(tree: str, element_id: str) -> str:
    """The ``/tree`` block for one id: its ``id (n/m visible)`` header and the
    indented slot lines under it — the failure message's self-diagnosis."""
    lines = tree.splitlines()
    out: list[str] = []
    for i, line in enumerate(lines):
        if line.startswith(f"{element_id} ("):
            out.append(line)
            for follow in lines[i + 1:]:
                if not follow.startswith(" "):
                    break
                out.append(follow)
            break
    return "\n".join(out) or f"<{element_id}: no /tree block>"


def _window_height(tree: str, element_id: str) -> float:
    """The window height the registry decided against, read off the same ``/tree``
    slot lines (``in WxH``) — never assumed, because macOS's window and each iOS
    device differ."""
    m = _GEO.search(_tree_block(tree, element_id))
    assert m, f"no geometry-bearing /tree slot for {element_id!r}:\n{_tree_block(tree, element_id)}"
    return float(m.group(6))


def _frame(driver, element_id: str, scope: str) -> tuple[float, float, float, float]:
    raw = driver.get_attr(element_id, "frame", scope=scope)
    assert raw, (
        f"{element_id!r} in {scope!r} has no frame — it is not in the registry at all; "
        f"{driver.diagnose(element_id, attrs=('frame',), scope=scope)}"
    )
    x, y, w, h = (float(p) for p in raw.split(","))
    return x, y, w, h


def test_a_painted_below_the_fold_element_reads_present_and_is_not_absent(logged_in_app):
    """The apple answer to the windows trap. A card the fold hides is laid out,
    registered and BELOW the window — and every read the driver has says so:
    ``is_visible`` True, ``count`` 1, ``is_absent`` False. A vertical viewport
    predicate creeping into ``isOnScreen`` (or into ``count``'s filter alone) would
    turn every negative read on an eager page into the windows-style vacuous pass;
    this is the running-app tripwire for it, which ``test_driver_is_absent.py``'s
    static "only windows overrides" pin cannot be."""
    app = logged_in_app
    d = app.driver
    _seed_feed(app, POSTS)
    last = POSTS - 1
    first_scope, last_scope = "post-card[0]", f"post-card[{last}]"

    tree = d.tree()
    window_h = _window_height(tree, PROBE)
    _fx, first_y, _fw, _fh = _frame(d, PROBE, first_scope)
    _lx, last_y, _lw, last_h = _frame(d, PROBE, last_scope)

    # The premise, MEASURED rather than assumed: the last card really is below the
    # window while the first is on it — otherwise the reads below prove nothing.
    assert first_y < window_h, (
        f"control: the first card's {PROBE} (y={first_y}) should be inside the "
        f"{window_h}pt window; a frame read that disagrees is a misread, not a finding.\n"
        f"{_tree_block(tree, PROBE)}"
    )
    assert last_y >= window_h, (
        f"{POSTS} posts did not push {last_scope} below the fold (y={last_y}, "
        f"window height {window_h}) — raise POSTS so the read below measures the "
        f"painted-but-offscreen case.\n{_tree_block(tree, PROBE)}"
    )

    assert d.count("post-card") == POSTS, (
        f"only {d.count('post-card')}/{POSTS} cards are registered — the feed is not "
        "eagerly realized on this app, so this is the LAZY case, not the painted-but-"
        f"offscreen one (rule 6).\n{_tree_block(tree, 'post-card')}"
    )
    assert d.is_visible(PROBE, scope=last_scope), (
        f"a painted-but-offscreen {PROBE} (y={last_y}, h={last_h}, window {window_h}) "
        f"reads NOT visible — apple now carries a vertical viewport predicate, which "
        f"makes a bare negative read vacuous here exactly as on windows; add the "
        f"override to drivers/macos.py / drivers/ios.py and update "
        f"test_driver_is_absent.py::test_only_windows_overrides_is_absent.\n"
        f"{_tree_block(d.tree(), PROBE)}"
    )
    assert d.count(PROBE, scope=last_scope) == 1, (
        f"count({PROBE!r}, scope={last_scope!r}) disagrees with is_visible — the two "
        "no longer share one predicate, so count == 0 stops being the same question."
    )
    assert d.is_absent(PROBE, scope=last_scope) is False, (
        f"is_absent answered True for a card that is laid out and registered "
        f"below the fold (y={last_y}, window {window_h})"
    )

    # The negative read is not simply always-False: an element the app has NOT put
    # in its tree reads absent through the very same path.
    ghost = f"post-card[{POSTS + 40}]"
    assert d.is_absent(PROBE, scope=ghost) is True, (
        f"is_absent({PROBE!r}, scope={ghost!r}) answered False for a card that does "
        f"not exist — the negative form cannot tell present from absent.\n"
        f"{_tree_block(d.tree(), PROBE)}"
    )
