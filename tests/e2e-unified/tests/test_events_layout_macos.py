"""macOS Events: the calendar grid fills the window — measured, not eyeballed.

`docs/goal/ui/events.md` § Layout & flow: the calendar list and controls are
the page's sidebar / top, and the MAIN AREA is the month / week / day / agenda
grid, Microsoft Outlook being the calendar UX reference. macOS used to squeeze
the toolbar AND the grid into a left pane capped at 400pt while most of the
window showed a "select an event" placeholder; the owner reported (2026-11-16)
that the calendar did not fit the window at all.

The observable is geometry, and the in-process driver publishes it: every
registered element's window-space frame (``registry_snapshot`` / the ``frame``
attribute, top-left origin) and, after a targeted scroll, the hosting window's
size (``last_scroll_geometry``). So no human eye is needed to tell a grid that
fills the window from one that does not:

* with nothing selected, in month and in week mode, the grid container is
  wider than half the window and runs to its right edge;
* the toolbar's view toggle and new-event button sit inside the grid's column
  and do not overlap;
* the month grid's six rows share the grid's height (the last row's cells
  reach the grid's bottom) instead of stopping at a fixed cell height;
* an open event detail is a TRAILING pane beside the grid, which stays wider
  than the pane; dismissed — by selecting a calendar, an existing interaction
  with an existing id (macOS paints no ``event-detail-back``: a declared
  absence, ruled 2026-08-14, events.md § Element IDs) — the grid has the full
  width again.

macOS-only: the defect and its layout are the macOS shell's
(`Fauna-macOS/Views/Events/EventSplitView.swift`); the other apps lay the page
out their own way and are covered by their own surfaces.
"""

import time
import uuid
from datetime import datetime, timedelta

import pytest

pytestmark = [pytest.mark.tier_3]

#: Points of slack for "reaches the edge": the main area's own padding and the
#: split view's divider, never a whole cell.
_EDGE_SLACK = 24

#: How long a relayout may take to show in the published frames. A ceiling, not
#: a target: a green read returns at once (convention 14).
_LAYOUT_BUDGET_S = 10.0


@pytest.fixture(autouse=True)
def _enable_calendar_backend(calendar_backend):
    """The events page needs the actor's MSEK first (see `test_events.py`)."""


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _future_dt(days_ahead: int, hour: int) -> str:
    dt = datetime.now() + timedelta(days=days_ahead)
    return dt.replace(hour=hour, minute=0, second=0, microsecond=0).strftime("%Y-%m-%dT%H:%M")


def _frames(driver) -> dict[str, list[tuple[float, float, float, float]]]:
    """Every registered element's frames, by id (indexed ids keep their order)."""
    records = driver.registry_snapshot()
    assert records is not None, "the macOS bridge serves no /registry to measure"
    out: dict[str, list[tuple[float, float, float, float]]] = {}
    for r in records:
        if r.get("frame"):
            out.setdefault(r["id"], []).append(tuple(float(v) for v in r["frame"].split(",")))
    return out


def _window_width(driver) -> float:
    """The hosting window's width, read off a targeted scroll's reply.

    `new-calendar-btn` sits in the calendar pane's eager ScrollView, so it always
    has a scrollable ancestor to answer with the window's size."""
    driver.scroll_to("new-calendar-btn")
    geo = driver.last_scroll_geometry()
    assert geo, "scroll_to(new-calendar-btn) reported no window geometry"
    return float(geo[1][0])


def _one(frames: dict, element_id: str, driver) -> tuple[float, float, float, float]:
    found = frames.get(element_id)
    assert found, f"{element_id} publishes no frame: {driver.diagnose(element_id)}"
    return found[0]


def _overlap(a, b) -> bool:
    return a[0] < b[0] + b[2] and b[0] < a[0] + a[2] and a[1] < b[1] + b[3] and b[1] < a[1] + a[3]


def _settled(check):
    """Run ``check`` until it stops raising ``AssertionError`` or the budget
    ends; the last failure is the one reported. A relayout (a pane opening or
    closing) lands a frame or two after the state change that causes it."""
    deadline = time.monotonic() + _LAYOUT_BUDGET_S
    while True:
        try:
            return check()
        except AssertionError:
            if time.monotonic() >= deadline:  # deadline-ok: re-raises the real assertion
                raise
            time.sleep(0.3)


def _assert_grid_fills(driver, grid_id, window_w, *, detail_open=False):
    return _settled(lambda: _check_grid_fills(driver, grid_id, window_w, detail_open=detail_open))


def _check_grid_fills(driver, grid_id: str, window_w: float, *, detail_open: bool = False):
    frames = _frames(driver)
    gx, gy, gw, gh = _one(frames, grid_id, driver)
    layout = {k: frames.get(k) for k in (grid_id, "events-view-toggle", "new-event-btn",
                                          "event-detail-summary")}
    if detail_open:
        # The detail pane runs from its content's left edge to the window's
        # edge; the grid must stay the wider of the two and run up to it.
        detail = _one(frames, "event-detail-summary", driver)
        pane_w = window_w - detail[0]
        assert gw > pane_w, (
            f"the grid ({gw:.0f}pt) must stay wider than the detail pane "
            f"(~{pane_w:.0f}pt); {layout}"
        )
        assert detail[0] >= gx + gw - _EDGE_SLACK, (
            f"the event detail must be a trailing pane right of the grid; {layout}"
        )
        assert detail[0] - (gx + gw) <= 2 * _EDGE_SLACK, (
            f"the grid must run up to the detail pane; {layout}"
        )
    else:
        assert gw > window_w / 2, (
            f"{grid_id} is {gw:.0f}pt wide in a {window_w:.0f}pt window — the grid "
            f"must be the main area (events.md § Layout & flow); frames: {layout}"
        )
        assert gx + gw >= window_w - _EDGE_SLACK, (
            f"{grid_id} ends at x={gx + gw:.0f} in a {window_w:.0f}pt window with nothing "
            f"selected — the grid must run to the window's edge; frames: {layout}"
        )
    toggle = _one(frames, "events-view-toggle", driver)
    new_btn = _one(frames, "new-event-btn", driver)
    for name, f in (("events-view-toggle", toggle), ("new-event-btn", new_btn)):
        assert f[0] >= gx - _EDGE_SLACK and f[0] + f[2] <= gx + gw + _EDGE_SLACK, (
            f"{name} must sit in the grid's column (x {gx:.0f}..{gx + gw:.0f}); {layout}"
        )
    assert not _overlap(toggle, new_btn), f"view toggle and new-event button overlap; {layout}"
    return frames, (gx, gy, gw, gh)


@pytest.mark.macos
@pytest.mark.feature("calendar-and-events")
def test_macos_events_grid_fills_the_window(logged_in_app):
    driver = logged_in_app.driver
    ev = logged_in_app.events
    ev.navigate()
    cal_name = _unique("layout-cal")
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    window_w = _window_width(driver)

    ev.switch_view("month")

    def month_rows_fill():
        frames, (gx, gy, gw, gh) = _check_grid_fills(driver, "events-month-grid", window_w)
        cells = [f for k, fs in frames.items() if k.startswith("events-day-cell-") for f in fs]
        assert len(cells) == 42, f"the month grid should publish 42 day cells, got {len(cells)}"
        bottom = max(c[1] + c[3] for c in cells)
        assert bottom >= gy + gh - _EDGE_SLACK, (
            f"the month grid's rows stop at y={bottom:.0f} in a grid ending at "
            f"y={gy + gh:.0f} — the six rows must share the grid's height"
        )

    _settled(month_rows_fill)

    ev.switch_view("week")
    _assert_grid_fills(driver, "calendar-week-grid", window_w)

    # `create_event` waits for the new event's agenda card, so compose from the
    # agenda view.
    summary = _unique("layout-ev")
    ev.switch_view("agenda")
    ev.create_event(summary, _future_dt(1, 10), _future_dt(1, 11))
    ev._click_event_card_by_summary(summary)
    driver.wait_for("event-detail-summary")
    ev.switch_view("month")
    _assert_grid_fills(driver, "events-month-grid", window_w, detail_open=True)

    # Dismiss by an existing interaction with an existing id: selecting a
    # calendar clears the selected event (`EventsVM.selectCalendar`).
    ev.select_calendar(cal_name)
    _assert_grid_fills(driver, "events-month-grid", window_w)
