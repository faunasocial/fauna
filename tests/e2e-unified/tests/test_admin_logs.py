"""Admin Logs view — the nest's log ring over WS-RPC
(docs/goal/architecture/apps/observability.md § Surfaces).

An admin-scoped page (`admin-logs`) that fetches the nest's in-memory `fauna-log`
ring via the admin WS-RPC `fauna.admin.logs` and renders it with the SAME widget
(and the same `log-entry` / `log-level-filter` / `log-copy-button` component IDs)
as the client's own Settings → Logs page — so the shared `LogsActions` accessors
(`app.logs.*`) drive it once navigated here. No Clear: there is no admin RPC to
wipe the nest ring. linux leads; the other five apps lift this shape
(priority #1).

tier_3 (full stack): a real `fauna-nest` binary whose `RingLayer` captures the
nest's own startup `tracing` output, fetched by a real admin client over WS-RPC.
This is the only tier that exercises the nest→wire→client log path end to end.

The ordering, attribution, filter and copy journeys plant their own lines as a
nest helper would (`helpers/log_plane.py`: an enrolled `mta`-role key reporting
on `fauna.bridges.report_log_events`), so each asserts on rows it knows the
level, time and source of — not on whatever the nest happened to log.
"""
import time

import pytest

from actions.logs import parse_row
from helpers import budgets
from helpers.app_surface import skip_unbuilt
from helpers.log_plane import LogPlaneHelper, helper_event
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _reopen_logs(app) -> None:
    """Leave the Logs page and come back: the nest's ring is fetched on entry
    into the page, so this is how an admin sees lines logged since."""
    app.admin.navigate_dashboard()
    app.admin.navigate_logs()
    assert app.admin.logs_page_visible(), (
        f"admin-logs page not reachable. error: {app.error_text()!r}"
    )


def _rows_once(app, predicate, what: str):
    """Poll the page's rows until `predicate(rows)` holds; return the rows."""
    return wait_until(
        lambda: (rows := app.logs.rows()) and predicate(rows) and rows,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"{what}; rows: {app.logs.rows()[:40]!r} "
        f"error: {app.error_text()!r}",
    )


def _index(rows, target: str) -> int:
    return next(i for i, r in enumerate(rows) if r[2] == target)


def _seconds(hhmmss: str) -> int:
    h, m, s = (int(x) for x in hhmmss.split(":"))
    return h * 3600 + m * 60 + s


def _first_time_inversion(rows):
    """The first adjacent pair (top to bottom) whose lower row is NEWER than
    the one above it, or None. Times are wall-clock `HH:MM:SS`, so one step up
    of more than twelve hours is the list crossing midnight, not an inversion."""
    for upper, lower in zip(rows, rows[1:]):
        step = _seconds(lower[1]) - _seconds(upper[1])
        if 0 < step < 12 * 3600:
            return upper, lower
    return None


@pytest.mark.feature("admin-logs")
def test_admin_logs_page_renders_nest_ring(admin_app):
    """The admin Logs page is reachable and shows the nest's captured activity
    (at minimum the nest's startup log lines)."""
    app = admin_app
    app.admin.navigate_logs()
    assert app.admin.logs_page_visible(), (
        f"admin-logs page not reachable. error: {app.error_text()!r}"
    )
    assert app.admin.wait_for_min_log_entries(1, timeout=15.0), (
        "the nest ring should hold at least the nest's startup log line, "
        f"got {app.logs.entry_count()} entries"
    )


@pytest.mark.feature("admin-logs")
def test_admin_logs_level_filter_narrows(admin_app):
    """Selecting a severity narrows the admin Logs view to that level and
    everything more severe — `Error` is a subset of `All`. The source is fetched
    once (no refetch), so the filter is a deterministic client-side narrowing."""
    app = admin_app
    app.admin.navigate_logs()
    assert app.admin.logs_page_visible()
    assert app.admin.wait_for_min_log_entries(1, timeout=15.0)
    n_all = app.logs.entry_count()
    assert n_all > 0, "expected captured nest log entries to filter"

    app.logs.set_level("Error")
    n_err = app.logs.entry_count()
    assert n_err <= n_all, (
        f"Error-only view ({n_err}) must be a subset of All ({n_all})"
    )

    # Back to All restores the full set — the filter narrows the held source in
    # memory, it does not refetch, so All is exactly the pre-filter count.
    app.logs.set_level("All")
    wait_until(
        lambda: app.logs.entry_count() == n_all,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"All view should restore {n_all} entries, got "
        f"{app.logs.entry_count()}",
    )


@pytest.mark.feature("admin-logs")
def test_admin_logs_copy_present(admin_app):
    """The copy affordance is present + actuable on the admin Logs page (the
    clipboard buffer isn't readable headlessly — we assert the affordance, not
    the OS buffer)."""
    app = admin_app
    app.admin.navigate_logs()
    assert app.admin.logs_page_visible()
    assert app.admin.wait_for_min_log_entries(1, timeout=15.0)
    app.logs.copy()


@pytest.mark.feature("admin-logs")
def test_admin_logs_lists_the_newest_activity_first(admin_app, planted_bridges):
    """observability.md § 3. Surfaces: the nest's activity is listed newest
    first. Two helper lines two minutes apart are planted; the newer one is
    above the older, and no row anywhere on the page sits above a newer one."""
    app = admin_app
    now = int(time.time() * 1000)
    older = helper_event("info", at_ms=now - 120_000)
    newer = helper_event("info", at_ms=now - 1_000)
    LogPlaneHelper(planted_bridges).plant_events(older, newer)

    _reopen_logs(app)
    rows = _rows_once(
        app,
        lambda rows: {older.target, newer.target} <= {r[2] for r in rows},
        "both planted helper lines should be listed",
    )
    assert _index(rows, newer.target) < _index(rows, older.target), (
        f"the newer line {newer.target} must be listed above the older "
        f"{older.target}; rows: {rows[:40]!r}"
    )
    inversion = _first_time_inversion(rows)
    assert inversion is None, (
        f"a row is listed above a newer one: {inversion!r}; rows: {rows[:40]!r}"
    )


@pytest.mark.feature("admin-logs")
def test_admin_logs_shows_a_helpers_event_among_the_nests_own_lines(admin_app, planted_bridges):
    """observability.md § Admission, attribution, and the remote ring: a
    helper's event lands in the one admin log beside the nest's own lines, in
    time order, under the helper's own name (`<source>:<event>`).

    The helper's event is bracketed by two nest-own lines at known moments:
    enrolling the helper makes the nest log `bridge self-enrolled …
    bridge_id=<id>` (target `bridge_rpc`) before the report, and enrolling a
    second helper makes it log another after. Top to bottom the page must read
    second's line, the helper's event, first's line."""
    app = admin_app
    first = LogPlaneHelper(planted_bridges)
    event = helper_event("warn", message="tls handshake failed")
    first.plant_events(event)
    second = LogPlaneHelper(planted_bridges)

    def _nest_line(rows, bridge_id):
        return next(
            (i for i, r in enumerate(rows)
             if r[2] == "bridge_rpc" and f"bridge_id={bridge_id}" in r[3]),
            None,
        )

    _reopen_logs(app)
    rows = _rows_once(
        app,
        lambda rows: event.target in {r[2] for r in rows}
        and _nest_line(rows, first.bridge_id) is not None
        and _nest_line(rows, second.bridge_id) is not None,
        "the helper's event and both nest-own enrollment lines should be listed",
    )
    i_event = _index(rows, event.target)
    level, _, target, message = rows[i_event]
    assert (level, target, message) == ("WARN", f"mta:{event.event}", event.message), (
        f"the helper's event should read WARN under mta:{event.event} with its own "
        f"message; got {rows[i_event]!r}"
    )
    i_after, i_before = _nest_line(rows, second.bridge_id), _nest_line(rows, first.bridge_id)
    assert i_after < i_event < i_before, (
        "the helper's event must sit between the nest's own line logged after it "
        f"(row {i_after}) and the one logged before it (row {i_before}), got row "
        f"{i_event}; rows: {rows[:40]!r}"
    )


@pytest.mark.feature("admin-logs")
def test_admin_logs_severity_filter_shows_only_that_severity(admin_app, planted_bridges):
    """observability.md § 3. Surfaces: choosing Error shows only error lines.
    A helper plants one info and one error line, so the unfiltered page holds
    both and the filter has something to remove: after choosing Error every
    row is an error, the planted error is there, the planted info is gone and
    the count has fallen."""
    app = admin_app
    info, error = helper_event("info"), helper_event("error")
    LogPlaneHelper(planted_bridges).plant_events(info, error)

    _reopen_logs(app)
    rows_all = _rows_once(
        app,
        lambda rows: {info.target, error.target} <= {r[2] for r in rows},
        "both planted lines should be listed before filtering",
    )

    app.logs.set_level("Error")
    rows_err = _rows_once(
        app,
        lambda rows: all(r[0] == "ERROR" for r in rows),
        "after choosing Error every row should be an error",
    )
    targets = {r[2] for r in rows_err}
    assert error.target in targets and info.target not in targets, (
        f"Error should keep {error.target} and drop {info.target}; rows: {rows_err!r}"
    )
    assert len(rows_err) < len(rows_all), (
        f"Error ({len(rows_err)} rows) should show fewer rows than All "
        f"({len(rows_all)}) — the planted info line alone must go"
    )


@pytest.mark.feature("admin-logs")
def test_admin_logs_copy_puts_the_shown_lines_on_the_clipboard(admin_app, planted_bridges):
    """observability.md § 3. Surfaces: copy puts the lines the page shows on
    the clipboard — under a filter, the filtered lines and not the whole ring.
    The clipboard is read where the OS (or, for tui, the terminal) holds it."""
    app = admin_app
    try:
        app.driver.get_clipboard_text()
    except NotImplementedError:
        skip_unbuilt(
            app.driver,
            surface="the driver's clipboard reader (get_clipboard_text)",
            detail="this app's driver cannot read the clipboard yet",
            tracked="tests/e2e-unified/drivers/base.py get_clipboard_text",
        )
    info, error = helper_event("info"), helper_event("error")
    LogPlaneHelper(planted_bridges).plant_events(info, error)

    _reopen_logs(app)
    _rows_once(
        app,
        lambda rows: {info.target, error.target} <= {r[2] for r in rows},
        "both planted lines should be listed before copying",
    )
    app.logs.set_level("Error")
    shown = _rows_once(
        app,
        lambda rows: all(r[0] == "ERROR" for r in rows) and error.target in {r[2] for r in rows},
        "the Error view should list only errors, the planted one among them",
    )

    app.logs.copy()

    def _copied():
        text = app.driver.get_clipboard_text() or ""
        lines = [r for r in map(parse_row, text.splitlines()) if r]
        return lines if error.target in {r[2] for r in lines} else None

    copied = wait_until(
        _copied,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"clipboard: {app.driver.get_clipboard_text()!r}",
    )
    assert copied == shown, (
        "the clipboard should hold exactly the lines shown, in the page's order: "
        f"shown={shown!r} copied={copied!r}"
    )
    assert info.target not in {r[2] for r in copied}, (
        f"the filtered-out info line {info.target} reached the clipboard: {copied!r}"
    )
