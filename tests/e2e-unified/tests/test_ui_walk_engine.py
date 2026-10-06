"""tier_1: the layer-(c) walk ENGINE's own logic, over a stub driver.

`helpers/ui_walk.py` is a checker, and convention 17's third adoption discipline
applies to checkers with particular force: *a checker that cries wolf gets
deleted*, and its inverse — a checker that reports clean because its own
bookkeeping is broken — is worse, because nothing downstream can tell. Layer (b)
paid for exactly that: a probe that produced **zero** observations across nine
green tests read identically to one that observed everything and found nothing.

So the engine's bookkeeping is pinned here rather than only by the sweep. Every
test below runs in-process against a stub driver — no binary, no nest, no agent —
which is the same tier-down convention 17 applies to walks themselves: breadth
and logic belong where a step costs microseconds, and the live sweep is left to
prove only the thing it alone can, namely that the vocabulary reaches the real
app.

⚠ These tests must never grow a driver. The moment this file needs an app it has
stopped being the cheap half.
"""

import pytest

from helpers import ui_walk

pytestmark = pytest.mark.tier_1


class StubDriver:
    """Records the walk's commands and replays canned frames.

    Deliberately *not* a mock framework: the assertions below are about ORDER and
    COUNT, which a recorded list states directly and a mock states obliquely.
    """

    def __init__(self, states=None):
        self.calls: list[tuple] = []
        self._states = list(states) if states is not None else None

    def focus_move(self, direction="next", times=1):
        self.calls.append(("focus_move", direction, times))

    def switch_pane(self, pane):
        self.calls.append(("switch_pane", pane))

    def get_state(self):
        if self._states is None:
            return {"nav": {"stack": [{"view": "feed"}]}}
        if not self._states:
            raise RuntimeError("stub ran out of frames")
        nxt = self._states.pop(0)
        if isinstance(nxt, Exception):
            raise nxt
        return nxt


class StubApp:
    def __init__(self, driver, error_text=""):
        self.driver = driver
        self._error = error_text

    def error_text(self):
        return self._error


def _clean_frame():
    """A frame every invariant in the catalogue passes."""
    return {
        "focused_line_count": 1,
        "messages": {"error": None},
        "nav": {"stack": [{"view": "feed"}]},
    }


def test_the_page_frontier_comes_from_the_spec_and_is_not_empty():
    """`ui.yaml`'s `navigation.pages`, not a hand-list in the helper.

    The value of reading the spec is that the walk widens when the product does.
    An empty read is treated as a hard error by `canonical_pages` rather than as
    "walk nothing", which would report a clean sweep over no pages at all.
    """
    pages = ui_walk.canonical_pages()
    assert pages, "the spec's page list must be non-empty"
    # Spot-check rather than pin the whole list: pinning it here would recreate
    # the hand-maintained enum this function exists to avoid.
    assert "feed" in pages and "settings" in pages


def test_a_page_sweep_crosses_into_the_content_region_before_tabbing():
    """Order is the contract, not an implementation detail.

    On tui a page reached through the state protocol keeps whatever zone the
    session was already in, so a walk that tabbed first would explore the sidebar
    ring on every page and report having explored the app — under-coverage that
    looks exactly like coverage. The `page` crossing must come first, and the
    `prev` arm must be exercised at least once or half the vocabulary is never
    driven by the sweep at all.
    """
    driver = StubDriver()
    report = ui_walk.WalkReport()
    ui_walk.WalkDriver(StubApp(driver), report).sweep_page("feed")

    assert driver.calls[0] == ("switch_pane", "page"), (
        f"the content-region crossing must be first; got {driver.calls[0]}"
    )
    ring = driver.calls[1:1 + ui_walk.FOCUS_RING_STEPS]
    assert ring == [("focus_move", "next", 1)] * ui_walk.FOCUS_RING_STEPS
    assert driver.calls[-2] == ("switch_pane", "sidebar")
    assert driver.calls[-1] == ("focus_move", "prev", 1), (
        "the `prev` direction is half the contract and must be driven"
    )
    assert report.steps == ui_walk.FOCUS_RING_STEPS + 3
    assert report.frames_read == report.steps
    assert not report.violations


def test_a_refusal_is_reported_once_not_once_per_step():
    """The agent-failure slot PERSISTS, so a naive check floods.

    Every app's refusal slot outranks on-screen errors and survives until the
    next reset — by design, so a refusal cannot be scrolled away. That means a
    single unimplemented command is visible on every subsequent frame, and a
    per-step report would emit hundreds of lines for one root cause, burying the
    first and only informative one.
    """
    driver = StubDriver()
    app = StubApp(driver, error_text='test agent refused command "focus_move": no arm')
    report = ui_walk.WalkReport()
    ui_walk.WalkDriver(app, report).sweep_page("feed")

    focus_refusals = [v for v in report.violations if "focus_move" in v]
    assert len(focus_refusals) == 1, (
        f"expected exactly one focus_move refusal, got {len(focus_refusals)}"
    )
    assert "REFUSED" in focus_refusals[0]
    # The *other* command was never refused, so it must not be reported at all —
    # the dedupe is per action, not a global once-and-done latch.
    assert not [v for v in report.violations if "switch_pane" in v]


def test_an_unreadable_frame_is_unobserved_not_clean():
    """The distinction the whole non-vacuity guard rests on.

    A frame that could not be read has asserted NOTHING. Counting it as a clean
    frame is how a wholly broken probe reports a clean run — so it goes to
    `unobserved`, and the sweep's own assertion is what turns that into a red.
    """
    driver = StubDriver(states=[_clean_frame(), RuntimeError("bridge gone"), _clean_frame()])
    report = ui_walk.WalkReport()
    walker = ui_walk.WalkDriver(StubApp(driver), report)

    walker.switch_pane("p/enter", "page")
    walker.focus_move("p/ring[0]")
    walker.focus_move("p/ring[1]")

    assert report.steps == 3
    assert report.frames_read == 2, "the unreadable frame must not count as read"
    assert report.unobserved == ["p/ring[0]"]
    assert not report.violations, "an unreadable frame is not an invariant violation"


def test_a_violating_frame_is_reported_against_the_step_that_produced_it():
    """A violation with no location is nearly as useless as no violation.

    The walk drives hundreds of steps; `where` is what turns "focus fanned out"
    into "focus fanned out on settings, 12 tabs in", which is the difference
    between a reproducible finding and a shrug.
    """
    bad = _clean_frame() | {"focused_line_count": 7}
    driver = StubDriver(states=[bad])
    report = ui_walk.WalkReport()

    ui_walk.WalkDriver(StubApp(driver), report).focus_move("settings/ring[12]")

    assert len(report.violations) == 1
    assert report.violations[0].startswith("settings/ring[12]: focus-fanout")


def test_an_app_publishing_no_field_is_not_applicable_never_a_violation():
    """Cross-app safety, inherited from layer (b)'s catalogue.

    A toolkit app that paints no focus affordance publishes no such field, and
    that is a permanent reasoned absence (ruled 2026-08-15), not debt. If it
    counted as a violation the sweep would be unusable on five of seven apps.
    """
    driver = StubDriver(states=[{"messages": {"error": None},
                                 "nav": {"stack": [{"view": "feed"}]}}])
    report = ui_walk.WalkReport()

    ui_walk.WalkDriver(StubApp(driver), report).focus_move("feed/ring[0]")

    assert not report.violations
    assert report.not_applicable.get("focus-fanout") == 1
