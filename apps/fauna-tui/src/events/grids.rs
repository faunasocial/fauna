//! The month/week/day view-rendering block (split out of `events/mod.rs`).

use fauna_core::caltime::{
    EventPlacement, add_days, day_column_layout, day_of_week, days_in_month, is_all_day,
    parse_date, visible_days, week_start_date,
};
use fauna_i18n::strings::events as et;
use fauna_ui_ids as ids;

use super::{EventRow, EventsState, ViewMode, now_hm, today};
use crate::element::Element;
use crate::format::{month_name, month_name_short, weekday_name, weekday_short};

/// ui.yaml's `events` page + `event_detail`/`create_calendar`/`create_event`
/// sub-pages. Renders the view-controls chrome always (view-mode toggles,
/// `calendar-date-label`, `events-prev/next-month`, calendar list, new-calendar/
/// new-event entry points) and, in [`super::Mode::List`], the active `view_mode`'s
/// content: the agenda event-card list, the month grid, or the week/day time
/// grids. `event_detail` paints RSVP + reminder + the attendee roster (all
/// cached from the last fetch — no separate round trip on open), and the
/// agenda card carries its own inline `event-rsvp-*` trio (`agenda.rs`).
///
/// The Outlook **day-cell drill-in** (`events-day-cell-{date}` single/double
/// click) landed 2026-07-31; the grids' own cells are tagged and both gestures
/// are wired (see `Action::DrillIntoDay` / `Action::ComposeOnDay`).
pub(super) fn view_mode_label(mode: ViewMode) -> &'static str {
    match mode {
        ViewMode::Agenda => et::VIEW_AGENDA,
        ViewMode::Month => et::VIEW_MONTH,
        ViewMode::Week => et::VIEW_WEEK,
        ViewMode::Day => et::VIEW_DAY,
    }
}

// ── Calendar/date primitives (client glue over shared `fauna_core::caltime`) ──
//
// The date math is `caltime`'s; the month/weekday **names** are the tui's own
// i18n catalog, resolved through `crate::format` (`ui/events.md` § Where logic
// lives — "i18n belongs to the client, not `fauna_core`"). Nothing here reaches
// for a chrono name specifier: those render English from inside the binary,
// where no catalog can localize them, and `format.rs`'s tree-walking guard
// `no_painted_text_takes_a_month_or_weekday_name_from_chrono` now reds on one.

/// Clamp a day into `(year, month)` — used when month nav lands on a shorter
/// month (Jan 31 → Feb 28).
pub(super) fn clamp_day(y: i32, m: u32, d: u32) -> u32 {
    d.clamp(1, days_in_month(y, m))
}

/// "Mon 13" — a day column's header, and the multi-day block prefix.
///
/// `caltime::add_days` normalizes any `(y, m, d)` it produces into a real date,
/// so every caller here hands this a valid one; there is no unrepresentable-date
/// arm to guard (the shape linux's twin has always had).
fn weekday_and_day(y: i32, m: u32, d: u32) -> String {
    format!("{} {}", weekday_short(day_of_week(y, m, d)), d)
}

/// `HH:MM` from a minute-of-day.
fn hhmm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

/// "July 2026" — the month view's `calendar-date-label`.
pub(super) fn format_month_label(y: i32, m: u32) -> String {
    format!("{} {}", month_name(m), y)
}

/// "Jul 13 – 19, 2026" — the week view's range label (mirrors linux
/// `time_utils::format_week_label`, down to the three range shapes and the
/// en-dash; both read their month names from the same `en.yaml` entries, so the
/// two apps localize together instead of one of them being stuck on English).
///
/// Takes `week_start` rather than probing it, for the reason linux's copy did
/// not and shipped the bug: a label that reads the locale independently of the
/// grid beside it can disagree with that grid about which week is on screen.
/// The explicit param also keeps this function pure, so its pin below asserts a
/// fixed string instead of whatever `LC_TIME` the test host happens to carry.
pub(super) fn format_week_label(y: i32, m: u32, d: u32, week_start: u32) -> String {
    let (sy, sm, sd) = week_start_date(y, m, d, week_start);
    let (ey, em, ed) = add_days(sy, sm, sd, 6);
    if sy == ey && sm == em {
        format!("{} {sd} \u{2013} {ed}, {sy}", month_name_short(sm))
    } else if sy == ey {
        format!(
            "{} {sd} \u{2013} {} {ed}, {sy}",
            month_name_short(sm),
            month_name_short(em)
        )
    } else {
        format!(
            "{} {sd}, {sy} \u{2013} {} {ed}, {ey}",
            month_name_short(sm),
            month_name_short(em)
        )
    }
}

/// "Wednesday, Jul 15, 2026" — the day view's label.
pub(super) fn format_day_label(y: i32, m: u32, d: u32) -> String {
    format!(
        "{}, {} {d}, {y}",
        weekday_name(day_of_week(y, m, d)),
        month_name_short(m)
    )
}

/// The `calendar-date-label` text for the active view mode (agenda shows the
/// month, like Outlook's mini-calendar header).
pub(super) fn date_label(st: &EventsState) -> String {
    let (y, m, d) = st.focus();
    match st.view_mode {
        ViewMode::Agenda | ViewMode::Month => format_month_label(y, m),
        ViewMode::Week => format_week_label(y, m, d, st.week_start),
        ViewMode::Day => format_day_label(y, m, d),
    }
}

/// The timed (non-all-day) events on `day`, each with its shared-Rust
/// `day_column_layout` placement — classification, minute geometry (an event
/// crossing midnight renders start → 24:00, clamped, never the collapsed
/// 30-minute block a naive end-time-of-day read produces), and `find_overlaps`
/// column packing in one call (events map to the grid by **start date only**,
/// no multi-day spanning; events.md § Where logic lives). Sorted by start
/// minute.
fn timed_events_on_day<'a>(
    events: &[&'a EventRow],
    day: (i32, u32, u32),
) -> Vec<(&'a EventRow, u32, u32, u32, u32)> {
    let day_events: Vec<&EventRow> = events
        .iter()
        .copied()
        .filter(|ev| parse_date(&ev.start) == Some(day))
        .collect();
    let inputs: Vec<(&str, Option<&str>)> = day_events
        .iter()
        .map(|ev| (ev.start.as_str(), Some(ev.end.as_str())))
        .collect();
    let mut out: Vec<(&EventRow, u32, u32, u32, u32)> = day_events
        .into_iter()
        .zip(day_column_layout(&inputs))
        .filter_map(|(ev, placement)| match placement {
            EventPlacement::Timed {
                start_min,
                end_min,
                column_index,
                total_columns,
            } => Some((ev, start_min, end_min, column_index, total_columns)),
            EventPlacement::AllDay => None,
        })
        .collect();
    out.sort_by_key(|(_, start_min, ..)| *start_min);
    out
}

/// The all-day events on `day` (for the `calendar-allday-band`).
fn all_day_events_on_day<'a>(events: &[&'a EventRow], day: (i32, u32, u32)) -> Vec<&'a EventRow> {
    events
        .iter()
        .copied()
        .filter(|ev| {
            parse_date(&ev.start) == Some(day) && is_all_day(&ev.start, Some(ev.end.as_str()))
        })
        .collect()
}

/// The month grid's weekday header, aligned to the 6-char day cells. Column
/// order is **rotated to `week_start`**, so the header always names the columns
/// the grid actually paints; the names come from the generated i18n catalog,
/// not a hardcoded English table (events.md:139 — localized weekday names are
/// per-app i18n, not `fauna_core`; linux's `time_utils.rs` fixed the
/// identical bug the same way).
fn weekday_header(week_start: u32) -> String {
    (0..7)
        .map(|c| format!("{:<MONTH_CELL_WIDTH$}", weekday_short((week_start + c) % 7)))
        .collect()
}

/// Paint the month grid: the registered `events-month-grid` marker (its text is
/// the weekday header) followed by 6 week rows of `events-day-cell-{YYYY-MM-DD}`
/// cells (today bracketed, adjacent-month days parenthesized, a `•` on days with
/// events).
///
/// Each cell is a full [`Element`] — its own id, its own gestures, its own place
/// in the focus ring — painted [`inline`](Element::inline) so seven of them still
/// read as one week row. That is what makes the Outlook drill-in addressable:
/// **single** press drills into Day view, **double** press opens the new-event
/// compose prefilled with the cell's date (`ui/events.md` § Layout & flow).
///
/// Cells for adjacent-month days are painted and addressable too — clicking one
/// is how a user reaches the neighbouring month's day directly, and
/// `DrillIntoDay` carries that cell's own `(y, m, d)` rather than the focused
/// month's.
pub(super) fn render_month(st: &EventsState, shown: &[&EventRow], out: &mut Vec<Element>) {
    let td = today();
    let ws = st.week_start;
    out.push(Element::label(ids::EVENTS_MONTH_GRID, weekday_header(ws)));
    let (fy, fm, _) = st.focus();
    for week in visible_days(ViewMode::Month, st.focus(), ws).chunks(7) {
        for (column, &(cy, cm, cd)) in week.iter().enumerate() {
            let is_today = (cy, cm, cd) == td;
            let in_month = cy == fy && cm == fm;
            let mark = if shown
                .iter()
                .any(|ev| parse_date(&ev.start) == Some((cy, cm, cd)))
            {
                "\u{2022}"
            } else {
                " "
            };
            let cell = if is_today {
                format!("[{cd:>2}]{mark}")
            } else if in_month {
                format!(" {cd:>2} {mark}")
            } else {
                format!("({cd:>2}){mark}")
            };
            // Padded to a fixed width here rather than by the painter: the cell
            // text IS the element's `text`, so the padding is what keeps the
            // grid aligned AND what the hit-test band measures — a click in the
            // blank part of a cell is still a click on that day.
            //
            // The week's LEADING cell starts a new painted row. Without that
            // every one of the 42 cells joined a single inline run and the six
            // weeks painted end to end on one clipped line
            // (`Element::starts_row` tells the story) — invisible to the e2e
            // suite, which reads the registry rather than the pixels.
            let cell = Element::label(
                day_cell_id(cy, cm, cd),
                format!("{cell:<MONTH_CELL_WIDTH$}"),
            );
            out.push(
                if column == 0 {
                    cell.starts_row()
                } else {
                    cell.inline()
                }
                .clickable(super::Gesture::Events(super::Action::DrillIntoDay {
                    y: cy,
                    m: cm,
                    d: cd,
                }))
                // Canvas, not a control: the cell is a padded fixed-width box
                // the grid aligns on, and `[15]` already means "today" here.
                // After `clickable`, which is what makes it a button at all.
                .cell()
                .double_clickable(super::Gesture::Events(
                    super::Action::ComposeOnDay {
                        y: cy,
                        m: cm,
                        d: cd,
                        at: None,
                    },
                )),
            );
        }
    }
}

/// `events-day-cell-{YYYY-MM-DD}` — ui.yaml indexes this component by ISO date
/// (`ui.yaml` `events-day-cell`, `indexed: true`), the same id every other app
/// registers, so the shared `EventsActions.has_day_cell`/`click_day_cell`/
/// `double_click_day_cell` reach tui with no per-app branch.
fn day_cell_id(y: i32, m: u32, d: u32) -> String {
    format!("events-day-cell-{y:04}-{m:02}-{d:02}")
}

/// Paint the week grid (`calendar-week-grid` + 7-day time grid).
pub(super) fn render_week(st: &EventsState, shown: &[&EventRow], out: &mut Vec<Element>) {
    let (fy, fm, fd) = st.focus();
    // One value for both the grid and its label — see `format_week_label`.
    let ws = st.week_start;
    let days = visible_days(ViewMode::Week, st.focus(), ws);
    out.push(Element::label(
        ids::CALENDAR_WEEK_GRID,
        format_week_label(fy, fm, fd, ws),
    ));
    render_time_grid(shown, &days, out);
}

/// Paint the day timeline (`calendar-day-timeline` + a single-day time grid,
/// the same renderer as the week grid over one date).
pub(super) fn render_day(st: &EventsState, shown: &[&EventRow], out: &mut Vec<Element>) {
    let (fy, fm, fd) = st.focus();
    out.push(Element::label(
        ids::CALENDAR_DAY_TIMELINE,
        format_day_label(fy, fm, fd),
    ));
    render_time_grid(
        shown,
        &visible_days(ViewMode::Day, st.focus(), st.week_start),
        out,
    );
}

/// The shared week/day time-grid body: a conditional all-day band, a
/// current-time marker when the window contains today, one countable
/// `calendar-event-block` per timed event (classification, minute geometry,
/// and overlap column packing all via the shared `caltime::day_column_layout`
/// — one call per day column, events.md § Where logic lives), and then the
/// **time axis** itself ([`render_time_axis`]).
fn render_time_grid(shown: &[&EventRow], days: &[(i32, u32, u32)], out: &mut Vec<Element>) {
    let all_day: Vec<&EventRow> = days
        .iter()
        .flat_map(|&day| all_day_events_on_day(shown, day))
        .collect();
    if !all_day.is_empty() {
        let names: Vec<&str> = all_day.iter().map(|ev| ev.summary.as_str()).collect();
        out.push(Element::label(
            ids::CALENDAR_ALLDAY_BAND,
            format!("{}: {}", et::ALL_DAY, names.join(", ")),
        ));
    }

    let td = today();
    if days.contains(&td) {
        let (h, m) = now_hm();
        out.push(Element::label(ids::CALENDAR_CURRENT_TIME, hhmm(h * 60 + m)));
    }

    let multi_day = days.len() > 1;
    for &day in days {
        let timed = timed_events_on_day(shown, day);
        for (ev, start_min, end_min, column_index, total_columns) in timed {
            let col_note = if total_columns > 1 {
                format!(" [{}/{}]", column_index + 1, total_columns)
            } else {
                String::new()
            };
            let prefix = if multi_day {
                format!("{} ", weekday_and_day(day.0, day.1, day.2))
            } else {
                String::new()
            };
            out.push(
                Element::label(
                    ids::CALENDAR_EVENT_BLOCK,
                    format!(
                        "{prefix}{}\u{2013}{} {}{col_note}",
                        hhmm(start_min),
                        hhmm(end_min),
                        ev.summary
                    ),
                )
                // ui.yaml: "Positioned timed event block …; click → event_detail"
                // (events.md § Week & day timeline views says the same). Same
                // gesture the agenda `event-card` carries, so the two surfaces
                // open the detail through one door.
                .clickable(super::Gesture::OpenEventDetail(ev.id.clone())),
            );
        }
    }

    render_time_axis(shown, days, out);
}

/// Quarter-hour slots in a day — the granularity ui.yaml ratified for
/// `events-time-slot-{HH-MM}` (24 h ÷ 15 min), the same 96-per-column tiling
/// linux lays under its blocks.
pub(super) const SLOTS_PER_DAY: u32 = 96;
pub(super) const SLOT_MINUTES: u32 = 15;

/// The hour gutter's fixed width — `"08:00 "`, and six blanks on the three
/// quarter-hour rows between.
pub(super) const GUTTER_WIDTH: usize = 6;

/// One day column's width. The week grid's seven columns have to share a
/// terminal (the e2e pty is 120 wide, a human's is often 80), so its cells are
/// narrow; the day timeline gives its single column room for a real summary.
pub(super) const WEEK_COLUMN_WIDTH: usize = 13;
pub(super) const DAY_COLUMN_WIDTH: usize = 40;

/// One month-grid cell's width — shared by the weekday header and the day cells
/// **because the two only align if they agree**. The header is a single label
/// painting seven names side by side; the cells are 42 separate elements
/// painting seven per row. Nothing but this constant relates the two, so a
/// literal in either place is a silently-misaligned grid
/// (`the_month_grid_paints_six_week_rows_under_an_aligned_weekday_header`).
pub(super) const MONTH_CELL_WIDTH: usize = 6;

/// The row the grids scroll to on entry — events.md § Week & day timeline
/// views: "auto-scroll to ~08:00 on first render". A 96-row axis opened at
/// 00:00 would show the user seven hours of empty night, and (unlike a GUI's
/// scrollbar) a terminal cannot mouse-click a row that is not painted, so this
/// is what makes the empty-slot quick-create reachable at all.
pub(super) const GRID_OPENS_AT_MINUTE: u32 = 8 * 60;

/// `events-time-slot-{HH-MM}` — ui.yaml indexes this component by the slot's
/// snapped start time (`indexed: true`), the same id every other app
/// registers, so the shared `EventsActions.has_time_slot`/`click_time_slot`
/// reach tui with no per-app branch.
pub(super) fn time_slot_id(minute_of_day: u32) -> String {
    format!(
        "events-time-slot-{:02}-{:02}",
        minute_of_day / 60,
        minute_of_day % 60
    )
}

/// What a day column's quarter-hour slot holds.
enum SlotFill<'a> {
    /// Empty grid space — the quick-create target.
    Free,
    /// The first slot of a timed event: the label goes here.
    Start(&'a str),
    /// A later slot of an event that began above.
    Busy,
}

/// One day column's 96 quarter-hour slots, resolved against that day's timed
/// events (the same shared `day_column_layout` placements the blocks above are
/// painted from, so the axis and the blocks can never disagree about when the
/// day is busy).
fn slot_fills<'a>(events: &[&'a EventRow], day: (i32, u32, u32)) -> Vec<SlotFill<'a>> {
    let mut fills = Vec::with_capacity(SLOTS_PER_DAY as usize);
    fills.resize_with(SLOTS_PER_DAY as usize, || SlotFill::Free);
    for (ev, start_min, end_min, ..) in timed_events_on_day(events, day) {
        let first = start_min / SLOT_MINUTES;
        // The end is exclusive, but a sub-slot event still owns the slot it
        // starts in — `day_column_layout`'s 30-minute floor means that is
        // always at least two slots, and `div_ceil` keeps the arithmetic
        // honest for any future floor.
        let last = (end_min.div_ceil(SLOT_MINUTES)).min(SLOTS_PER_DAY);
        for slot in first..last {
            fills[slot as usize] = if slot == first {
                SlotFill::Start(&ev.summary)
            } else {
                SlotFill::Busy
            };
        }
    }
    fills
}

/// Truncate-or-pad to exactly `width` display cells, so every column of every
/// row lines up. The cell text IS the element's `text` and the hit-test band
/// measures it, so this padding is what makes a click in the blank half of a
/// cell still land on that cell (the month grid's rule, § `render_month`).
fn cell(text: &str, width: usize) -> String {
    let mut out: String = text.chars().take(width).collect();
    let painted = out.chars().count();
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(painted)));
    out
}

/// Paint the week/day **time axis**: an hour gutter down the left and one
/// quarter-hour row per `events-time-slot-{HH-MM}`, each day column carrying
/// its own cell (`ui/events.md` § Week & day timeline views).
///
/// An EMPTY cell is the ratified Outlook quick-create target: a real
/// [`Element`] with the slot's id and a `ComposeOnDay` gesture carrying that
/// column's date *and* the slot's snapped time. A cell an event occupies is
/// untagged chrome instead — painted, never clickable — so a click on a busy
/// slot cannot spuriously open a compose, which is the same bug linux's
/// column-level background gesture shipped until its 2026-08-01 fix.
///
/// The labelled `calendar-event-block` elements stay full-width lines above
/// the axis rather than moving into the cells: a 13-cell week column cannot
/// hold `"10:00–11:00 Standup [1/2]"`, the element's `text` is exactly what
/// every app's `get_text` contract returns, and truncating it in a week
/// column would silently make the cross-app block assertions read a
/// different string on tui than everywhere else. The axis shows *when* the day
/// is busy; the blocks say *what* with the canonical text.
fn render_time_axis(shown: &[&EventRow], days: &[(i32, u32, u32)], out: &mut Vec<Element>) {
    let width = if days.len() > 1 {
        WEEK_COLUMN_WIDTH
    } else {
        DAY_COLUMN_WIDTH
    };
    let columns: Vec<Vec<SlotFill<'_>>> = days.iter().map(|&day| slot_fills(shown, day)).collect();

    if days.len() > 1 {
        // The week axis needs its columns named, exactly as the month grid's
        // `weekday_header` names its own — untagged chrome, since ui.yaml
        // scopes no id for a grid's column header.
        out.push(Element::chrome(" ".repeat(GUTTER_WIDTH)).starts_row());
        for &(y, m, d) in days {
            out.push(Element::chrome(cell(&weekday_and_day(y, m, d), width)).inline());
        }
    }

    for slot in 0..SLOTS_PER_DAY {
        let minute = slot * SLOT_MINUTES;
        // The hour gutter proper: 00:00–23:00 down the left, blank on the
        // three quarter-hour rows between, so the hours read as gridlines.
        let gutter = if minute.is_multiple_of(60) {
            format!("{:<GUTTER_WIDTH$}", hhmm(minute))
        } else {
            " ".repeat(GUTTER_WIDTH)
        };
        out.push(Element::chrome(gutter).starts_row());

        for (column, &(y, m, d)) in days.iter().enumerate() {
            out.push(match columns[column][slot as usize] {
                SlotFill::Start(summary) => Element::chrome(cell(summary, width)).inline(),
                SlotFill::Busy => Element::chrome(cell("\u{2502}", width)).inline(),
                // A faint tick on the hour keeps the empty grid legible
                // without drawing 96 rows of dots.
                SlotFill::Free => Element::label(
                    time_slot_id(minute),
                    cell(
                        if minute.is_multiple_of(60) {
                            "\u{00b7}"
                        } else {
                            ""
                        },
                        width,
                    ),
                )
                .inline()
                .clickable(super::Gesture::Events(super::Action::ComposeOnDay {
                    y,
                    m,
                    d,
                    at: Some((minute / 60, minute % 60)),
                }))
                // Canvas, not a control — one fixed-width slot box per row.
                // After `clickable`, which is what makes it a button at all.
                .cell(),
            });
        }
    }
}
