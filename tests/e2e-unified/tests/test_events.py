import uuid
from datetime import datetime, timedelta

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from i18n.strings import S

# reclaim_cycle: the events area's representative in the post-reclaim gate — under
# `--reclaim-cycle` the shared nest is wiped + re-claimed before this runs, so
# enable-mail → MSEK → encrypted-calendar provisioning is exercised post-reclaim
# (the live `CalendarsLoaded: 0` bug class). See `just e2e-reclaim-cycle-test`.
pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.reclaim_cycle]


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _future_dt(days_ahead: int, hour: int, minute: int = 0) -> str:
    """An ISO `YYYY-MM-DDTHH:MM` timestamp `days_ahead` days from now.

    Computed relative to today so the test never rots into the past (a fixed
    date eventually falls outside the visible range of date-filtered views).
    """
    dt = datetime.now() + timedelta(days=days_ahead)
    return dt.replace(hour=hour, minute=minute, second=0, microsecond=0).strftime(
        "%Y-%m-%dT%H:%M"
    )


@pytest.fixture(autouse=True)
def _enable_calendar_backend(calendar_backend):
    """Every events test needs the actor's MSEK before it runs.

    The mechanism (and the per-app gate) moved to the shared `calendar_backend`
    fixture in `conftest.py` 2026-08-02, when `test_calendar_visibility.py`
    turned out to need the same precondition — a second copy would have been the
    per-app-divergence mistake one directory down. This wrapper keeps it
    `autouse` for THIS module, which needs it on every test; a module needing it
    on one test just requests `calendar_backend` directly.
    """


@pytest.mark.feature("calendar-and-events")
def test_month_day_cell_click_opens_day_view(logged_in_app):
    """Single-clicking a month-grid day cell drills into Day view **for that date**.

    Microsoft Outlook month→day interaction (events.md § Layout & flow). Rolled
    out per app; declared ``skip_unbuilt`` on apps that do not yet expose
    ``events-day-cell`` (events.md § Implementation status today), so the staged
    rollout stays a counted, down-only debt rather than an invisible skip.

    ⚠ **Asserting the timeline is merely *visible* cannot catch a wrong-day
    drill-in, and that is all this test did until 2026-08-12.** windows shipped a
    drill-in that painted the *previously* selected day (`SelectDay` assigned
    `ViewMode` before `CurrentDay`, cause (3) of the day-cell row in events.md
    § Implementation status today) and this test stayed green throughout — a Day
    view is a Day view whichever date it paints. The timeline has no date label
    of its own to assert on across apps (windows' `calendar-date-label` still
    renders ``"MMMM yyyy"`` even in Day view — the open question that row
    records), so the day it *actually belongs to* is read back through the one
    surface that does carry it: quick-create from one of its empty slots and read
    the prefilled ``event-dtstart``. This is the widening recipe events.md
    § Implementation status today carries verbatim; existing ratified ids only.
    """
    from datetime import date

    ev = logged_in_app.events
    ev.navigate()
    # The quick-create the read-back goes through lives in the calendar-scoped
    # events section, so it needs a calendar selected — same precondition as the
    # double-click test below.
    cal_name = _unique("drillin-cal")
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    ev.switch_view("month")
    assert ev.is_month_grid_visible(), (
        f"month grid should be visible: {logged_in_app.driver.diagnose('events-month-grid')}"
    )

    # Day 15 is always inside the current month the grid opens on.
    target = date.today().replace(day=15).isoformat()
    if not ev.has_day_cell(target):
        skip_unbuilt(
            logged_in_app.driver,
            surface="events-day-cell",
            detail="month cells paint as untagged chrome, so there is nothing to drill from",
            tracked="events.md § Implementation status today (Month day-cell drill-in row)",
        )

    ev.click_day_cell(target)
    assert logged_in_app.driver.is_visible("calendar-day-timeline"), (
        "single-clicking a month-grid day cell should switch to Day view: "
        f"{logged_in_app.driver.diagnose('calendar-day-timeline')}"
    )

    if not ev.has_time_slot(9, 0):
        skip_unbuilt(
            logged_in_app.driver,
            surface="events-time-slot",
            detail=(
                "the day timeline paints no per-15-min slot targets, so the day it "
                "shows cannot be read back"
            ),
            tracked="events.md § Implementation status today (Month day-cell drill-in row)",
        )

    ev.click_time_slot(9, 0)
    assert logged_in_app.driver.is_visible("event-dtstart"), (
        "clicking an empty slot of the drilled-into day should open the new-event "
        f"compose: {logged_in_app.driver.diagnose('event-dtstart')}"
    )
    value = logged_in_app.driver.get_text("event-dtstart")
    assert target in value, (
        f"the drill-in should show the CLICKED day {target}, but a quick-create from "
        f"its own timeline prefilled {value!r} — the Day view is painting another date"
    )


@pytest.mark.feature("calendar-and-events")
def test_month_double_click_day_cell_opens_new_event_prefilled(logged_in_app):
    """Double-clicking an empty month-grid day cell opens the new-event compose
    prefilled with that date (Outlook day-cell model, events.md § Layout & flow,
    slice 2).

    Single-click drills into Day view (slice 1); double-click opens the new-event
    compose with ``event-dtstart`` prefilled to the cell's date **at the day's
    working start (09:00)**. A calendar must be selected (the compose lives in
    the calendar-scoped events section). Declared ``skip_unbuilt`` on apps that
    do not yet expose ``events-day-cell``.

    ⚠ **The date alone is not a discriminator — asserting it was this test's
    whole content until 2026-08-12, and three apps carried the bug it was written
    for while it stayed green.** When the drill-in unmounts the cell before the
    second physical press lands, that press falls through to an
    ``events-time-slot-*`` of the Day timeline that just repainted underneath, so
    a compose opens on the **correct date** at the *slot's* time with the drill-in
    also standing (tui and linux both presented exactly this; events.md
    § Implementation status today, the day-cell bullet). Only the TIME separates
    the cell's own day-origin prefill from a slot fall-through, which is why the
    working start is asserted here and why tui's tier_1 pair test asserts the
    whole ``2026-07-09T09:00``.

    Date and time are asserted separately rather than as one string: the apps use
    platform-idiomatic input widgets whose serialization differs (web's
    ``datetime-local`` reads ``YYYY-MM-DDTHH:MM``, linux's GTK entry
    ``YYYY-MM-DD HH:MM``), so a single literal would fail on a separator rather
    than on behavior. Same shape as the empty-time-slot tests below.
    """
    from datetime import date

    cal_name = _unique("dblclick-cal")
    ev = logged_in_app.events
    ev.navigate()
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    ev.switch_view("month")
    assert ev.is_month_grid_visible(), (
        f"month grid should be visible: {logged_in_app.driver.diagnose('events-month-grid')}"
    )

    # Day 15 is always inside the current month the grid opens on.
    target = date.today().replace(day=15).isoformat()
    if not ev.has_day_cell(target):
        skip_unbuilt(
            logged_in_app.driver,
            surface="events-day-cell",
            detail="month cells paint as untagged chrome, so there is nothing to double-click",
            tracked="events.md § Implementation status today (Month day-cell drill-in row)",
        )

    ev.double_click_day_cell(target)
    assert logged_in_app.driver.is_visible("event-dtstart"), (
        "double-clicking a month-grid day cell should open the new-event compose: "
        f"{logged_in_app.driver.diagnose('event-dtstart')}"
    )
    value = logged_in_app.driver.get_text("event-dtstart")
    assert target in value, (
        f"new-event start should be prefilled to {target}, got {value!r}"
    )
    assert "09:00" in value, (
        f"the CELL's own gesture prefills the day's working start 09:00, got {value!r} "
        "— a time that is neither 09:00 nor absent means the press fell through to a "
        "time slot of the repainted Day timeline (both gestures fired); no time at all "
        "means this app composes an all-day event where the other five compose at 09:00"
    )


@pytest.mark.feature("calendar-and-events")
def test_event_create_and_delete(logged_in_app):
    """Create a calendar with two events, delete one, verify the other remains."""
    cal_name = _unique("cal")
    logged_in_app.events.navigate()
    logged_in_app.events.create_calendar(cal_name)
    logged_in_app.events.select_calendar(cal_name)

    logged_in_app.events.create_event(
        summary="Meeting A", start=_future_dt(7, 10), end=_future_dt(7, 11)
    )
    logged_in_app.events.create_event(
        summary="Meeting B", start=_future_dt(8, 14), end=_future_dt(8, 15)
    )
    # No count assertion: `select_calendar` narrowed the agenda to this test's own
    # freshly-created calendar, so the two identity asserts below say strictly
    # more than `event_count() >= 2` ever did — and on macOS the agenda is a
    # virtualizing `List`, so a row count is not a measure of the data anyway.
    summaries = logged_in_app.events.event_summaries()
    assert "Meeting A" in summaries
    assert "Meeting B" in summaries

    logged_in_app.events.delete_event("Meeting A")
    summaries = logged_in_app.events.event_summaries()
    assert "Meeting A" not in summaries
    assert "Meeting B" in summaries


# Calendar-level `.ics` import/export (events.md § Import / Export; ids
# user-approved 2026-09-25). tui leads, macos and ios followed through one shared
# FaunaKit `CalendarFileControls`; each remaining app gains its mark here as it
# wires `calendar-import-file` / `calendar-import-button` / `calendar-export-button`.
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.feature("calendar-and-events")
def test_calendar_exported_to_a_file_imports_into_another_calendar(logged_in_app):
    """Export a calendar as one `.ics` file, import that file into a second,
    fresh calendar, and the second calendar lists the same events — the whole
    user promise of outcome 13 in one journey, through the app's own buttons.

    Identity on both ends, never a count: the events are named uniquely, the
    import target is created empty by this test, and the exported file must
    carry both summaries before the import is even attempted, so a green run
    cannot come from the source calendar being shown by mistake."""
    ev = logged_in_app.events
    ev.navigate()
    source = _unique("ics-src")
    first, second = _unique("ics-a"), _unique("ics-b")
    ev.create_calendar(source)
    ev.select_calendar(source)
    ev.create_event(summary=first, start=_future_dt(5, 10), end=_future_dt(5, 11))
    ev.create_event(summary=second, start=_future_dt(6, 14), end=_future_dt(6, 15))

    exported = ev.export_selected_calendar()
    with open(exported, encoding="utf-8") as fh:
        body = fh.read()
    assert first in body and second in body, (
        f"the exported file {exported} must carry both events: {body!r}"
    )

    target = _unique("ics-dst")
    ev.create_calendar(target)
    ev.select_calendar(target)
    ev.import_calendar_file(exported)
    shown = ev.wait_for_listed(present=[first, second])
    assert first in shown and second in shown, (
        f"after importing {exported} into {target!r} it should list both events, "
        f"listed={shown!r}; {logged_in_app.driver.diagnose('error-message')}"
    )


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.feature("calendar-and-events")
def test_calendar_import_with_no_file_chosen_says_so(logged_in_app):
    """`calendar-import-button` pressed with no file chosen answers on
    `error-message` — never a silent no-op (events.md § Import / Export)."""
    ev = logged_in_app.events
    ev.navigate()
    cal = _unique("ics-empty")
    ev.create_calendar(cal)
    ev.select_calendar(cal)
    ev.press_import_with_no_file()
    logged_in_app.driver.wait_for("error-message", timeout=15.0)
    said = logged_in_app.driver.get_text("error-message")
    assert ev.no_ics_chosen_text() in said, (
        f"an import with no file chosen should say so, error-message={said!r}"
    )


@pytest.mark.feature("calendar-and-events")
def test_event_description_location_and_time_reach_the_detail_surface(logged_in_app):
    """An event's description, location and time survive the round trip to detail.

    The three `event-detail-*` text rows and the two `event-form-*` inputs are
    the `event_detail` / `create_event` id family `events.md` § Element IDs
    ratifies (and § Layout & flow's `create_event` bullet: "+ description +
    location"). The VEVENT carries all three — `fauna_core::ical::EventFields`
    has `location`/`description` and `vevent_lines` emits `LOCATION:`/
    `DESCRIPTION:` — so this asserts the app's own read/write halves, not the
    wire's.

    **This journey had no test on any app until 2026-08-10**: the
    `create_event_full` action helper existed with zero callers, and its two
    optional fields were additionally wrapped in `is_visible` guards that turned
    an unbuilt form into a silent pass (e2e convention 7's shape). Both are
    fixed here — the guards are gone, so a missing input fails loudly.

    Time is asserted non-empty rather than by format: each app renders the range
    through its own date library (apple emits raw RFC 3339, web a formatted
    string), so a format assertion here would encode one app's choice as the
    cross-app contract. tui's own range formatting is pinned exactly at tier_1
    (`events::tests::detail_paints_the_time_range_location_and_description`).
    """
    cal_name = _unique("cal")
    summary = _unique("Offsite")
    description = f"Agenda and notes {uuid.uuid4().hex[:8]}"
    location = f"Room {uuid.uuid4().hex[:6]}"

    logged_in_app.events.navigate()
    logged_in_app.events.create_calendar(cal_name)
    logged_in_app.events.select_calendar(cal_name)
    logged_in_app.events.create_event_full(
        summary=summary,
        start=_future_dt(9, 13, 37),
        end=_future_dt(9, 14, 37),
        description=description,
        location=location,
    )

    logged_in_app.events.open_detail_by_summary(summary)

    time_text = logged_in_app.events.detail_time_text()
    assert time_text.strip(), (
        "event_detail must state WHEN the event is — an event surface with no "
        f"time is the gap this test exists to close: "
        f"{logged_in_app.driver.diagnose('event-detail-time')}"
    )
    assert location in logged_in_app.events.detail_location_text(), (
        f"detail must render the typed location {location!r}: "
        f"{logged_in_app.driver.diagnose('event-detail-location')}"
    )
    assert description in logged_in_app.events.detail_description_text(), (
        f"detail must render the typed description {description!r}: "
        f"{logged_in_app.driver.diagnose('event-detail-description')}"
    )


@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("calendar-and-events")
def test_event_detail_back_returns_to_the_list(logged_in_app):
    """``event-detail-back`` returns ``event_detail`` to the list.

    Until this test, ``EventsActions.back_from_detail()`` had zero callers —
    the id it drives was then required by ui.yaml on all 7 apps but built on
    only two, so no test could call it without 404ing on the other five
    (``test_automation_registry_lifecycle.py``'s apple journey explicitly
    routed around it for exactly this reason). That is why this id had never
    been contract-tested on ANY app.

    The scope was RULED 2026-08-14 (user-approved, UI-consistency rule A):
    the id is the in-surface control that returns a NAVIGATED ``event_detail``
    to the list, ``platform_elements``-scoped in ui.yaml to tui/windows/android.
    The other four have no such control, in two distinct classes — structural
    for the split-view apps (linux, web, macos present the detail beside the
    list, so there is no navigation to reverse), HIG-convention for ios (the
    system nav-bar back / edge swipe is the whole affordance). Do NOT "restore
    parity" by building the id on any of the four; that re-opens a settled
    ruling (events.md § Element IDs).

    MARKED for the three apps that paint the control, never gated by a
    ``declared_absence`` on the other four (feature-catalog.md § Cell
    semantics, the marked-witness rule, 2026-09-26): what the four lack is an
    in-surface *control* — a mechanism — while the outcome this test witnesses
    (`calendar-and-events` outcome 1) holds on every column through the
    module's other witnesses, so a skip here would red a column the page owes
    nothing of this test on.
    """
    cal_name = _unique("backcal")
    summary = _unique("BackEvent")
    ev = logged_in_app.events
    ev.navigate()
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    ev.create_event(summary=summary, start=_future_dt(7, 10), end=_future_dt(7, 11))
    ev.open_detail_by_summary(summary)

    assert logged_in_app.driver.is_visible("event-detail-summary"), (
        "should be on event_detail before testing the way out: "
        f"{logged_in_app.driver.diagnose('event-detail-summary')}"
    )

    ev.back_from_detail()

    assert logged_in_app.driver.is_visible("event-card"), (
        "event-detail-back should return to the list (an event-card visible "
        f"again): {logged_in_app.driver.diagnose('event-card')}"
    )


def test_calendar_view_toggles(logged_in_app):
    """Verify all 4 calendar view toggles are visible."""
    logged_in_app.events.navigate()
    for view in ("agenda", "month", "week", "day"):
        eid = f"calendar-view-{view}"
        assert logged_in_app.driver.is_visible(eid), (
            f"view toggle {eid} should be visible: "
            f"{logged_in_app.driver.diagnose(eid)}"
        )


@pytest.mark.feature("calendar-and-events")
def test_switch_to_week_view(logged_in_app):
    """Week view renders a positioned event block for an event in the current week."""
    ev = logged_in_app.events
    ev.navigate()
    # Select the fresh calendar rather than riding the VM's "fall back to the
    # first calendar" path: the actor is session-scoped, so an unselected page
    # shows the union of every calendar, and on macOS that list is a virtualizing
    # `List` whose registered row count saturates at what the pane fits — the
    # create is then unobservable no matter how it is asserted. Scoping to this
    # test's own calendar also makes the block assertion below unambiguous.
    # (The unselected-create fallback keeps its own coverage elsewhere; see
    # `EventsVM.createEvent`.)
    cal = _unique("cal")
    ev.create_calendar(cal)
    ev.select_calendar(cal)
    # Place the event today at 10:00 so it is always inside the current week
    # window and the block position is clearly non-trivial (topPx = 600 inside
    # the day column, which exercises margin-positioning via SizeChanged).
    ev.create_event(summary="Standup", start=_future_dt(0, 10), end=_future_dt(0, 11))
    ev.switch_view("week")
    assert logged_in_app.driver.is_visible("calendar-week-grid"), (
        f"week view should render the grid: {logged_in_app.driver.diagnose('calendar-week-grid')}"
    )
    # Match THIS test's event, not "at least one block". A count threshold was
    # satisfiable by any other event landing in the same week — the grid takes
    # whatever the page is scoped to, and the actor is session-scoped — so it
    # could pass with "Standup" entirely absent.
    ev.wait_for_event_block("Standup")


@pytest.mark.feature("calendar-and-events")
def test_switch_to_day_view(logged_in_app):
    """Day view renders a positioned event block for an event on the selected day."""
    ev = logged_in_app.events
    ev.navigate()
    # Scoped to this test's own calendar for the same reason as the week view
    # above — an unselected page shows every calendar's events, which macOS's
    # virtualizing agenda `List` cannot report a count for.
    cal = _unique("cal")
    ev.create_calendar(cal)
    ev.select_calendar(cal)
    # Create an event today at 14:00 — CurrentDay defaults to today, so it lands
    # in the day column and a calendar-event-block is rendered.
    ev.create_event(summary="Review", start=_future_dt(0, 14), end=_future_dt(0, 15))
    ev.switch_view("day")
    assert logged_in_app.driver.is_visible("calendar-day-timeline"), (
        f"day view should render the timeline: {logged_in_app.driver.diagnose('calendar-day-timeline')}"
    )
    # Identity, not a count: `test_switch_to_week_view` also creates an event
    # today (10:00 "Standup"), so a bare `>= 1` on the day timeline could pass on
    # a neighbouring test's block whether or not "Review" ever rendered.
    ev.wait_for_event_block("Review")


# Marked only where `EventsActions.event_block_layout` can read where a block
# sits: tui's block text, and web's, linux's, macOS's, iOS's and windows' drawn geometry
# measured against the grid's own time-slot markers. The other GUI apps position blocks in pixels
# the action layer does not measure yet, so marking them would record a pass
# that asserted nothing about side by side. They join through the cross-app lift
# row minted with this witness.
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.feature("calendar-and-events")
def test_day_timeline_bands_all_day_events_and_sets_clashes_side_by_side(logged_in_app):
    """An all-day event sits in its own band above the day, and events that clash
    sit side by side — including one that runs past midnight, which is drawn to
    midnight and no further (``docs/goal/ui/events.md`` § Week & day timeline
    views: the ``calendar-allday-band``, "side-by-side column-packing for
    overlaps", and "a 22:00→02:00 event renders 22:00 → midnight everywhere").

    Every event is made through the compose form, in a calendar of this test's
    own that is picked for the whole journey, so no other test's events can join
    a clash group. The all-day event is midnight-to-midnight, the form's way of
    saying "all day" (``caltime::is_all_day``).
    """
    ev = logged_in_app.events
    ev.navigate()
    cal = _unique("layout-cal")
    ev.create_calendar(cal)
    ev.select_calendar(cal)

    all_day = _unique("AllDay")
    clash_a, clash_b = _unique("ClashA"), _unique("ClashB")
    late, late_clash = _unique("PastMidnight"), _unique("LateClash")
    ev.create_event(summary=all_day, start=_future_dt(0, 0), end=_future_dt(1, 0))
    ev.create_event(summary=clash_a, start=_future_dt(0, 10), end=_future_dt(0, 11))
    ev.create_event(summary=clash_b, start=_future_dt(0, 10, 30), end=_future_dt(0, 11, 30))
    ev.create_event(summary=late, start=_future_dt(0, 22), end=_future_dt(1, 2))
    ev.create_event(summary=late_clash, start=_future_dt(0, 23), end=_future_dt(0, 23, 30))

    # The Day view opens on today, the day every event above starts on.
    ev.switch_view("day")
    for summary in (clash_a, clash_b, late, late_clash):
        ev.wait_for_event_block(summary)

    # ── The all-day event is in the band, not in the timed grid.
    band = ev.allday_band_text()
    assert all_day in band, (
        f"the all-day event {all_day!r} should sit in calendar-allday-band; the band "
        f"reads {band!r}; {logged_in_app.driver.diagnose('calendar-allday-band')}"
    )
    blocks = ev.event_block_texts()
    assert not any(all_day in text for text in blocks), (
        f"an all-day event belongs in the band, never as a timed block; blocks={blocks!r}"
    )

    # ── Clashing events sit side by side: same group of two, different columns.
    for first, second in ((clash_a, clash_b), (late, late_clash)):
        a, b = ev.event_block_layout(first), ev.event_block_layout(second)
        assert a["columns"] == b["columns"] == 2 and a["column"] != b["column"], (
            f"{first!r} and {second!r} clash, so they should sit side by side — two "
            f"columns, one each; got {first!r}={a!r}, {second!r}={b!r}"
        )

    # ── The event running past midnight is drawn to midnight and no further.
    past_midnight = ev.event_block_layout(late)
    assert (past_midnight["start"], past_midnight["end"]) == ("22:00", "24:00"), (
        f"a 22:00→02:00 event should be drawn 22:00 → midnight (24:00), never "
        f"collapsed or run off the grid; got {past_midnight!r}"
    )
    lone = ev.event_block_layout(clash_a)
    assert (lone["start"], lone["end"]) == ("10:00", "11:00"), (
        f"a same-day block keeps its own times; got {clash_a!r}={lone!r}"
    )


@pytest.mark.feature("calendar-and-events")
def test_day_view_empty_slot_click_opens_quick_create_prefilled(logged_in_app):
    """Clicking an empty Day-view time slot opens the new-event compose
    prefilled with that date AND time (events.md § Week & day timeline views).

    The day deliberately already holds an event (14:00) — the sharpest case:
    empty-slot quick-create must exist on a NON-empty day, which is exactly
    what linux lacked until 2026-08-01 (its pre-slot-marker background gesture
    shipped, but the whole affordance was absent on windows/android/tui and
    time-less on apple — events.md § Implementation status today). Declared
    ``skip_unbuilt`` on apps that do not yet expose ``events-time-slot``.
    """
    from datetime import date

    ev = logged_in_app.events
    ev.navigate()
    cal = _unique("slot-cal")
    ev.create_calendar(cal)
    ev.select_calendar(cal)
    ev.create_event(summary="Slotted", start=_future_dt(0, 14), end=_future_dt(0, 15))
    ev.switch_view("day")
    ev.wait_for_event_block("Slotted")

    if not ev.has_time_slot(9, 15):
        skip_unbuilt(
            logged_in_app.driver,
            surface="events-time-slot",
            detail="the day timeline paints no per-15-min slot targets, so there is nothing to click",
            tracked="events.md § Implementation status today (empty-time-slot quick-create bullet)",
        )

    ev.click_time_slot(9, 15)
    assert logged_in_app.driver.is_visible("event-dtstart"), (
        "clicking an empty day-view time slot should open the new-event compose: "
        f"{logged_in_app.driver.diagnose('event-dtstart')}"
    )
    value = logged_in_app.driver.get_text("event-dtstart")
    today = date.today().isoformat()
    assert today in value, (
        f"quick-create start should be prefilled with the day's date {today}, got {value!r}"
    )
    assert "09:15" in value, (
        f"quick-create start should be prefilled with the slot's time 09:15, got {value!r}"
    )


@pytest.mark.feature("calendar-and-events")
def test_week_view_empty_slot_click_opens_quick_create_with_slot_time(logged_in_app):
    """Clicking an empty Week-view time slot opens the new-event compose
    prefilled with that slot's time (events.md § Week & day timeline views).

    Only the TIME half is asserted exactly here: week-grid slot ids repeat per
    day column and the driver resolves the first showing match, whose column
    date is locale-dependent (week start) — the exact date+time pairing is
    pinned by the Day-view test above, where the single column is unambiguous.
    Declared ``skip_unbuilt`` on apps that do not yet expose
    ``events-time-slot``.
    """
    ev = logged_in_app.events
    ev.navigate()
    cal = _unique("slot-cal")
    ev.create_calendar(cal)
    ev.select_calendar(cal)
    ev.switch_view("week")
    assert logged_in_app.driver.is_visible("calendar-week-grid"), (
        f"week view should render the grid: {logged_in_app.driver.diagnose('calendar-week-grid')}"
    )

    if not ev.has_time_slot(13, 30):
        skip_unbuilt(
            logged_in_app.driver,
            surface="events-time-slot",
            detail="the week grid paints no per-15-min slot targets, so there is nothing to click",
            tracked="events.md § Implementation status today (empty-time-slot quick-create bullet)",
        )

    ev.click_time_slot(13, 30)
    assert logged_in_app.driver.is_visible("event-dtstart"), (
        "clicking an empty week-view time slot should open the new-event compose: "
        f"{logged_in_app.driver.diagnose('event-dtstart')}"
    )
    value = logged_in_app.driver.get_text("event-dtstart")
    assert "13:30" in value, (
        f"quick-create start should be prefilled with the slot's time 13:30, got {value!r}"
    )


@pytest.mark.feature("calendar-and-events")
def test_event_rsvp(logged_in_app):
    """Create event and RSVP to it."""
    cal_name = _unique("rsvp-cal")
    ev = logged_in_app.events
    ev.navigate()
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    ev.create_event("Lunch", _future_dt(9, 12), _future_dt(9, 13))
    ev.rsvp_event("Lunch", "accepted")
    assert ev.event_rsvp_count("Lunch") >= 1


@pytest.mark.feature("calendar-and-events")
def test_event_card_rsvp(logged_in_app):
    """RSVP inline on the agenda card, without opening the detail page.

    Distinct from `test_event_rsvp` above (which RSVPs via the detail-page
    `event-detail-rsvp-*` trio): this drives the card-level `event-rsvp-*`
    quick action (events.md § components — `rsvp-button-group` is a child of
    BOTH `event-card` and `event_detail`). The assertion opens the detail page
    AFTER the card click to confirm the RSVP actually persisted against the
    right event, not just that the button didn't error.

    Declared ``skip_unbuilt`` on apps that do not paint the card-level trio.
    This file carries no app markers, so it collects everywhere. The gate was
    added 2026-07-31 for tui (found by the day-cell slice's adjacent-suite
    sweep, the same class as `test_contacts.py`'s `contacts-no-matches`) and
    **tui built the surface later that day**, so its one remaining consumer is
    **android**, which paints `event-rsvp-*` only on the separate
    invited-events quick-list and not on the main event-card — events.md
    § Implementation status today, ui.yaml's `event-card` platform note.
    The gate is capability-probed rather than app-name-keyed, so it resolves
    itself when android lifts the surface. Counted debt beats an invisible red.

    The ``>= 1`` is not the vacuous presence check it resembles: a
    Fauna-created event starts with an **empty** roster and no ORGANIZER
    (pinned in `libs/fauna-client-caldav/src/mutate.rs`), so the single
    `attendee-item` can only exist because `apply_rsvp` pushed the
    self-attendee. Zero-to-one is the measurement.
    """
    cal_name = _unique("card-rsvp-cal")
    ev = logged_in_app.events
    ev.navigate()
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    ev.create_event("Standup", _future_dt(10, 9), _future_dt(10, 9, 30))
    if not ev.has_card_rsvp():
        skip_unbuilt(
            logged_in_app.driver,
            surface="event-rsvp-going/-interested/-decline (card-level)",
            detail=(
                "the main agenda card paints no rsvp-button-group, so there "
                "is nothing to click; the detail-page trio is built and "
                "covered by test_event_rsvp (android: the trio exists only on "
                "the separate invited-events quick-list)"
            ),
            tracked="events.md § Implementation status today (event-card-level inline RSVP)",
        )
    ev.rsvp_on_card("Standup", status="going")
    assert ev.event_rsvp_count("Standup") >= 1, (
        f"card-level RSVP should have registered an attendee: "
        f"{logged_in_app.driver.diagnose('attendee-item')}"
    )


@pytest.mark.feature("calendar-and-events")
def test_event_invite_attendee(logged_in_app):
    """Invite an attendee by email on the detail surface; the roster grows.

    The encrypted CalDAV path (events.md Decision B): a Fauna-created event
    starts with an empty roster, so the `attendee-invite-field` email entry +
    `attendee-invite-button` must add an `ATTENDEE;PARTSTAT=NEEDS-ACTION:
    mailto:<email>` line to the canonical VEVENT (read-mutate-rewrite + re-PUT)
    and fan out the iMIP `REQUEST`. The roster is `mailto:` CAL-ADDRESSes and
    email is the universal attendee mechanism — caldav-server.md § Scheduling &
    invitations (in production `alice@example.com` is handle == email ==
    CAL-ADDRESS, so one ATTENDEE line serves Fauna and non-Fauna invitees alike).

    The assertion (the attendee appears in `attendee-list`) proves the
    read-mutate-rewrite re-PUT round-tripped: the roster persists locally even if
    the best-effort iMIP dispatch fails (events.md § Errors). Runs on the apps
    exercising the encrypted email-invite path in e2e: linux + web + **windows**
    (windows lifted at Step 4c — a windows coordination follow-up; `EventDetailViewModel.
    InviteAsync` → `FfiCaldavClient.invite_attendee(email)`) + **tui** (M5 slice 3
    part 3 — `fauna_client_caldav::add_attendee` +
    `dispatch_imip_request`/`NestImipDispatch`, direct Rust like linux) + **macOS
    + iOS** (the gate here
    was simply stale: `MacEventDetailView.swift`/`EventDetailView.swift` already
    wire `attendee-invite-field`/`-button` to `submitInvite()` behind
    `.faunaGate("fauna.bridges.put_event_ciphertext")`, one shared FaunaKit
    `AttendeeRow` renders the resulting roster). android e2e enablement remains
    its own follow-up (the docstring above's per-app split).
    """
    if not (
        logged_in_app.driver.is_linux()
        or logged_in_app.driver.is_web()
        or logged_in_app.driver.is_windows()
        or logged_in_app.driver.is_tui()
        or logged_in_app.driver.is_macos()
        or logged_in_app.driver.is_ios()
    ):
        skip_unbuilt(
            logged_in_app.driver,
            surface="the email-attendee-entry encrypted-CalDAV path",
            detail="linux + web + windows + tui + macOS + iOS have it; android "
            "e2e enablement is the remaining cross-app follow-on (route-3)",
            tracked="caldav-server.md",
        )
    cal_name = _unique("invite-cal")
    ev = logged_in_app.events
    ev.navigate()
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    ev.create_event("Planning", _future_dt(11, 15), _future_dt(11, 16))

    ev.invite_to_event("Planning", "guest@example.com")
    assert ev.event_rsvp_count("Planning") >= 1
    # `attendee-id`/`attendee-status` (tui/web/windows/linux built as of this test): a bare-CalDAV invitee has
    # no CN, so the display-name fallback is the raw email, and a fresh
    # ATTENDEE always starts NEEDS-ACTION → the shared `rsvp_status_verbatim`
    # projection's "invited" arm, localized "Invited" in the e2e's English
    # default. Both ids read one value everywhere per the shared Rust
    # projection (`fauna_core::ical::{attendee_display,rsvp_status_label}`),
    # so this is the one place worth asserting it instead of per-app.
    assert logged_in_app.driver.get_text("attendee-id") == "guest@example.com"
    assert logged_in_app.driver.get_text("attendee-status") == "Invited"


@pytest.mark.feature("calendar-and-events")
def test_switch_to_month_view(logged_in_app):
    """Switch to month view and verify month grid appears."""
    logged_in_app.events.navigate()
    logged_in_app.events.switch_view("month")
    assert logged_in_app.events.is_month_grid_visible(), (
        f"month grid should be visible: {logged_in_app.driver.diagnose('events-month-grid')}"
    )


@pytest.mark.feature("calendar-and-events")
def test_month_navigation(logged_in_app):
    """Navigate between months and verify the date label changes."""
    logged_in_app.events.navigate()
    logged_in_app.events.switch_view("month")
    label_before = logged_in_app.events.date_label()
    logged_in_app.events.next_month()
    label_after = logged_in_app.events.date_label()
    assert label_before != label_after


# The apps consuming the shared `caltime::pan` policy. The ones still absent step
# a whole month (or nothing) in week/day view, so widening this list is a build
# — a lift onto the shared policy — not a marker change.
#
# ⚠ For apple the lift was not only the pan. `calendar-date-label` rendered the
# **month** in every view mode, so the day-view assertions below could not move
# it at all inside one month and the agenda assertion would have been the vacuous
# one this file's docstrings warn about. The label had to become range-aware
# (month / week span / day / "Upcoming") in the same change, exactly as linux's
# `update_date_label` already was.
PAN_POLICY_APPS = ("linux", "tui", "web", "macos", "ios", "windows")


@pytest.mark.feature("calendar-and-events")
def test_pan_moves_one_visible_range_per_view(logged_in_app):
    """`events-prev/next-month` pans **one visible range**, not always a month.

    events.md § User actions defines the pair as "pan visible range", and
    § Where logic lives makes the step policy shared Rust
    (`fauna_core::caltime::pan_step`). The contract is measured without parsing
    any label: **seven clicks in Day view must land on the same date as one
    click in Week view**, since one week is seven days. That equality is
    format- and locale-independent, and it fails on every app that steps a
    month in week view (which was tui, windows, macos and ios) or does not pan
    at all there (which was web — its pan control lived inside the month grid,
    so outside month view it was not even in the DOM).

    `test_month_navigation` covers only Month view, where all seven apps
    already agreed — which is why this divergence survived unmeasured.
    """
    ev = logged_in_app.events
    ev.navigate()

    if app_name(logged_in_app.driver) not in PAN_POLICY_APPS:
        skip_unbuilt(
            logged_in_app.driver,
            surface="the shared caltime::pan visible-range policy",
            detail="events-prev/next-month still steps a month (or nothing) outside month view",
            tracked="events.md § Where logic lives (view-mode + visible-range lift)",
        )

    ev.switch_view("day")
    start = ev.date_label()

    for _ in range(7):
        ev.next_month()
    after_seven_day_steps = ev.date_label()
    assert after_seven_day_steps != start, (
        "seven day-view pans should have moved the day label off its start"
    )

    # Return to the start so the two routes are measured from the same anchor.
    for _ in range(7):
        ev.prev_month()
    assert ev.date_label() == start, (
        f"day-view pan should be symmetric: {start!r} -> {ev.date_label()!r}"
    )

    ev.switch_view("week")
    ev.next_month()
    ev.switch_view("day")
    assert ev.date_label() == after_seven_day_steps, (
        "one week-view pan must move exactly seven days: day view reads "
        f"{ev.date_label()!r} after one week step but "
        f"{after_seven_day_steps!r} after seven day steps"
    )


@pytest.mark.feature("calendar-and-events")
def test_pan_is_inert_in_the_date_unfiltered_agenda(logged_in_app):
    """Panning in Agenda view does nothing — there is no range to pan.

    The agenda list is date-unfiltered on every app, so a pan that moved the
    anchor would mutate state the view cannot show: linux stepped seven days
    behind a list that ignored the date, and tui advanced the month label above
    a list that never changed. `caltime::pan_step` answers `None` here.

    The absence is anchored to a **causal barrier**, not a settle-sleep
    (convention 14): the view-switch that follows is processed after the pan
    click on the same queue, so waiting for the day-timeline marker proves the
    pan click was consumed rather than merely still in flight.

    ⚠ **The assertion reads the DAY label, not the agenda one, and that is
    load-bearing.** Asserting on the agenda's own `calendar-date-label` is
    vacuous: tui renders the *month* there, so an agenda pan of a week — the
    exact pre-lift linux behaviour this test exists to forbid — moves the
    anchor without moving the label whenever it stays inside one month. That
    first cut survived its mutation. The day label is the finest-grained view
    of the same anchor, so it witnesses a move of any size.
    """
    ev = logged_in_app.events
    ev.navigate()

    if app_name(logged_in_app.driver) not in PAN_POLICY_APPS:
        skip_unbuilt(
            logged_in_app.driver,
            surface="the shared caltime::pan visible-range policy",
            detail="agenda-view pan still moves the anchor date invisibly",
            tracked="events.md § Where logic lives (view-mode + visible-range lift)",
        )

    # The anchor as the finest-grained view renders it, before anything moves.
    ev.switch_view("day")
    anchor_before = ev.date_label()

    ev.switch_view("agenda")
    ev.pan_forward_without_waiting()

    # Barrier: this switch is queued behind the pan click, and its marker only
    # appears once the click has been handled — so a still-unchanged label
    # below means the pan did nothing, not that it has yet to be processed.
    ev.switch_view("day")
    assert logged_in_app.driver.is_visible("calendar-day-timeline")

    # The day label must settle back on exactly the pre-pan anchor. A pan that
    # moved the anchor by any amount never reaches that value, so the
    # deadline-poll discriminates while costing a correct app nothing.
    ev.wait_for_date_label(anchor_before)


def test_calendar_date_label_visible(logged_in_app):
    """Verify the calendar date label is visible."""
    logged_in_app.events.navigate()
    assert logged_in_app.driver.is_visible("calendar-date-label"), (
        f"date label should be visible: {logged_in_app.driver.diagnose('calendar-date-label')}"
    )


@pytest.mark.feature("calendar-and-events")
def test_event_reminder(logged_in_app):
    """Set, read, and remove a per-event reminder on the detail surface.

    Drives the encrypted-CalDAV Events backend (the VEVENT VALARM offset on the
    sealed `bridge_caldav_*` body via `fauna.bridges.*`; the legacy plaintext
    `fauna.events.reminder.*` kinds were retired in the § 4c cleanup). Each
    asserted UI transition proves one round-trip: the page mutates local reminder
    state only AFTER the `await` resolves, so the transition cannot appear unless
    the call succeeded.
    """
    cal_name = _unique("rem-cal")
    ev = logged_in_app.events
    ev.navigate()
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    ev.create_event("Standup", _future_dt(10, 9), _future_dt(10, 10))

    # Open the detail: the preset select is shown only after `getReminder`
    # resolves null for this fresh event — exercises the WS get (empty case).
    ev.open_detail_by_summary("Standup")
    assert not ev.has_reminder()

    # Set a 1-hour-before reminder: the current-reminder label appears only
    # after `setReminder`'s WS call resolves — exercises the WS set.
    ev.set_reminder("PT1H")
    assert S.events.reminder.hour_1 in ev.reminder_current_label()

    # Remove it: the preset select returns only after `removeReminder`'s WS call
    # resolves — exercises the WS delete.
    ev.remove_reminder()
    assert not ev.has_reminder()


def test_event_form_fields_visible(logged_in_app):
    """Open the event creation form and verify optional fields exist."""
    cal_name = _unique("form-cal")
    ev = logged_in_app.events
    ev.navigate()
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    logged_in_app.driver.click("new-event-btn")
    logged_in_app.driver.wait_for("event-summary")
    # At minimum, summary and time fields must be present
    assert logged_in_app.driver.is_visible("event-summary"), (
        "event creation form should show the summary field: "
        f"{logged_in_app.driver.diagnose('event-summary')} "
        f"error={logged_in_app.error_text()!r}"
    )
    assert logged_in_app.driver.is_visible("event-dtstart"), (
        "event creation form should show the dtstart field: "
        f"{logged_in_app.driver.diagnose('event-dtstart')} "
        f"error={logged_in_app.error_text()!r}"
    )
