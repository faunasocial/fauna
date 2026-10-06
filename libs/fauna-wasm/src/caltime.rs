//! `#[wasm_bindgen]` exposure of `fauna_core::caltime` — the pure, portable
//! Gregorian calendar/date math — to the Svelte SPA's calendar/events views.
//!
//! Web's `MiniCalendar`/`WeekView` hand-rolled the month-grid assembly and the
//! week-start snap in TypeScript over the quirky JS `Date` object (silent month
//! overflow, mutable, locale-fragile). These wrappers let the SPA share the one
//! tested implementation `fauna_core::caltime` already gives linux's
//! `time_utils::*` (priority #2/#4 — resolve drift, one source of truth)
//! instead of re-deriving the layout in `Date`-based TS. The native apps
//! deliberately stay on their platform date libraries (`java.time` / Swift
//! `Calendar` / .NET `DateTime`), so there is no UniFFI twin — see
//! `docs/goal/ui/events.md` § Where logic lives.
//!
//! ## Conventions crossing the boundary
//! - **`month` is 1-based (1 = January … 12 = December)** — the ISO / Rust /
//!   nest convention `caltime` uses everywhere, *not* JavaScript's 0-based
//!   `Date` month. The SPA converts at the seam (pass `jsMonth + 1` in, build
//!   `new Date(year, month - 1, day)` out).
//! - **`weekStart` and the day-of-week scale are `0 = Monday … 6 = Sunday`**
//!   (also `caltime`'s convention). Pass `0` for the current Monday-start grids.
//!
//! Localized month/weekday *names* and the "today"/selection highlight stay
//! client-side (locale + system clock) — see `events.md` § Where logic lives.

use wasm_bindgen::prelude::*;

use crate::rpc::{from_js, to_js};

/// A calendar date crossing the boundary as `{ year, month, day }`
/// (`month` 1-based — see the module note).
#[derive(serde::Serialize)]
struct CalDate {
    year: i32,
    month: u32,
    day: u32,
}

impl From<(i32, u32, u32)> for CalDate {
    fn from((year, month, day): (i32, u32, u32)) -> Self {
        Self { year, month, day }
    }
}

/// A `(year, month)` pair as `{ year, month }` (`month` 1-based).
#[derive(serde::Serialize)]
struct CalMonth {
    year: i32,
    month: u32,
}

/// `fauna_core::caltime::month_grid` → the 6×7 = 42-cell month-view grid as a JS
/// array of `{ year, month, day }`, padded with the trailing days of the
/// previous month and the leading days of the next so the grid always fills six
/// rows. The SPA derives each cell's `inMonth` flag from `cell.month === month`
/// and builds its `Date` from the triple. Replaces `MiniCalendar.svelte`'s
/// hand-rolled `daysInMonth` / `firstDayOfWeek` / cells loop.
#[wasm_bindgen(js_name = monthGrid)]
pub fn month_grid(year: i32, month: u32, week_start: u32) -> Result<JsValue, JsValue> {
    let grid: Vec<CalDate> = fauna_core::caltime::month_grid(year, month, week_start)
        .into_iter()
        .map(CalDate::from)
        .collect();
    to_js(&grid)
}

/// `fauna_core::caltime::prev_month` → `{ year, month }`, the month before
/// `(year, month)` with the year wrapping at January. Replaces
/// `MiniCalendar.svelte`'s inline `if (viewMonth === 0) { viewYear--; viewMonth = 11 }`.
#[wasm_bindgen(js_name = prevMonth)]
pub fn prev_month(year: i32, month: u32) -> Result<JsValue, JsValue> {
    let (year, month) = fauna_core::caltime::prev_month(year, month);
    to_js(&CalMonth { year, month })
}

/// `fauna_core::caltime::next_month` → `{ year, month }`, the month after
/// `(year, month)` with the year wrapping at December.
#[wasm_bindgen(js_name = nextMonth)]
pub fn next_month(year: i32, month: u32) -> Result<JsValue, JsValue> {
    let (year, month) = fauna_core::caltime::next_month(year, month);
    to_js(&CalMonth { year, month })
}

/// `fauna_core::caltime::week_start_date` → `{ year, month, day }`, the date of
/// the `week_start` weekday (`0 = Mon … 6 = Sun`) for the week containing
/// `(year, month, day)`. Paired with `addDays` it replaces `WeekView.svelte`'s
/// `getWeekDays` `Date`-mutation snap.
#[wasm_bindgen(js_name = weekStartDate)]
pub fn week_start_date(
    year: i32,
    month: u32,
    day: u32,
    week_start: u32,
) -> Result<JsValue, JsValue> {
    to_js(&CalDate::from(fauna_core::caltime::week_start_date(
        year, month, day, week_start,
    )))
}

/// `fauna_core::caltime::add_days` → `{ year, month, day }`, `(year, month, day)`
/// shifted by `n` days (negative to go back), crossing month/year boundaries.
/// The SPA calls it for offsets `0..=6` off `weekStartDate` to build the seven
/// week-view columns.
#[wasm_bindgen(js_name = addDays)]
pub fn add_days(year: i32, month: u32, day: u32, n: i32) -> Result<JsValue, JsValue> {
    to_js(&CalDate::from(fauna_core::caltime::add_days(
        year, month, day, n,
    )))
}

/// One timed event's sub-column assignment in a week/day time grid, crossing the
/// boundary as `{ eventIndex, columnIndex, totalColumns }`. Block width is
/// `1 / totalColumns` of the day column; block x-offset is `columnIndex`.
#[derive(serde::Serialize)]
struct OverlapColumn {
    #[serde(rename = "eventIndex")]
    event_index: u32,
    #[serde(rename = "columnIndex")]
    column_index: u32,
    #[serde(rename = "totalColumns")]
    total_columns: u32,
}

/// `fauna_core::caltime::find_overlaps` → side-by-side column packing for the
/// week/day time grid's overlapping timed event blocks. Input is a JS array of
/// `[startMinutes, endMinutes]` pairs (minutes from midnight, end exclusive),
/// one per timed event; output is one `{ eventIndex, columnIndex, totalColumns }`
/// per input event **in input order**. This is the one `caltime` overlap-layout
/// primitive a web surface now consumes — the SPA's week/day timeline rides it
/// (rather than re-deriving union-find clustering in TS) so blocks lay out
/// bit-identically to the native apps (`docs/goal/ui/events.md`
/// § Where logic lives — the named exception that needs cross-app parity;
/// the natives reach the same `find_overlaps` via UniFFI `find_event_overlaps`).
#[wasm_bindgen(js_name = findOverlaps)]
pub fn find_overlaps(intervals: JsValue) -> Result<JsValue, JsValue> {
    let pairs: Vec<(u32, u32)> = from_js(intervals)?;
    let cols: Vec<OverlapColumn> = fauna_core::caltime::find_overlaps(&pairs)
        .into_iter()
        .map(|(event_index, column_index, total_columns)| OverlapColumn {
            event_index: event_index as u32,
            column_index,
            total_columns,
        })
        .collect();
    to_js(&cols)
}

/// One event's placement in a week/day time-grid column
/// (`fauna_core::caltime::EventPlacement`, flattened — the UniFFI
/// `FfiDayEventPlacement` twin). `allDay: true` → an all-day-band chip (the
/// minute/column fields are 0 and meaningless); otherwise a positioned block.
#[derive(serde::Serialize)]
struct DayEventPlacement {
    #[serde(rename = "allDay")]
    all_day: bool,
    #[serde(rename = "startMin")]
    start_min: u32,
    #[serde(rename = "endMin")]
    end_min: u32,
    #[serde(rename = "columnIndex")]
    column_index: u32,
    #[serde(rename = "totalColumns")]
    total_columns: u32,
}

/// `fauna_core::caltime::day_column_layout` → the complete all-day/timed
/// layout for one day column: `isAllDay` classification, timed minute geometry
/// (an event crossing midnight renders start → 24:00 — never the collapsed
/// 30-minute block the SPA's local `minutesOfDay` end-derivation produced —
/// clamped to 1440), and `findOverlaps` column packing in one call. Input is a
/// JS array of `[dtstart, dtend | null]` string pairs for the day's events
/// (the date filter stays client-side); output is one placement per input
/// event **in input order**.
#[wasm_bindgen(js_name = dayColumnLayout)]
pub fn day_column_layout(events: JsValue) -> Result<JsValue, JsValue> {
    let pairs: Vec<(String, Option<String>)> = from_js(events)?;
    let inputs: Vec<(&str, Option<&str>)> = pairs
        .iter()
        .map(|(s, e)| (s.as_str(), e.as_deref()))
        .collect();
    let placements: Vec<DayEventPlacement> = fauna_core::caltime::day_column_layout(&inputs)
        .into_iter()
        .map(|p| match p {
            fauna_core::caltime::EventPlacement::AllDay => DayEventPlacement {
                all_day: true,
                start_min: 0,
                end_min: 0,
                column_index: 0,
                total_columns: 0,
            },
            fauna_core::caltime::EventPlacement::Timed {
                start_min,
                end_min,
                column_index,
                total_columns,
            } => DayEventPlacement {
                all_day: false,
                start_min,
                end_min,
                column_index,
                total_columns,
            },
        })
        .collect();
    to_js(&placements)
}

/// `fauna_core::caltime::is_all_day` — the one cross-app all-day
/// classification rule (no end, a date-only DTSTART, or a midnight-to-midnight
/// span of ≥ 1 whole day), for surfaces that classify outside
/// `dayColumnLayout` (e.g. the wire-flagged `EventSummary.is_all_day` is
/// derived by `parse_ical` from `VALUE=DATE` only — this is the richer rule).
#[wasm_bindgen(js_name = isAllDay)]
pub fn is_all_day(start: &str, end: Option<String>) -> bool {
    fauna_core::caltime::is_all_day(start, end.as_deref())
}

/// `fauna_core::caltime::normalize_event_datetime_input` — the shared
/// `event-dtstart` / `event-dtend` text-field normalization (pad a bare
/// `…THH:MM` to `…THH:MM:00`, unify a space-separated date+time to `T`;
/// seconds-bearing / zoned / date-only / unrecognizable input passes through).
#[wasm_bindgen(js_name = normalizeEventDatetimeInput)]
pub fn normalize_event_datetime_input(input: &str) -> String {
    fauna_core::caltime::normalize_event_datetime_input(input)
}

// ── View mode + visible range ────────────────────────────────────────────────
//
// `events.md` § Where logic lives → *View mode + visible range* splits this
// three ways, and the split decides what belongs here:
//
// - The **vocabulary** (`CalendarViewMode::as_wire`) crosses as the plain
//   `"agenda"|"month"|"week"|"day"` string, so the SPA's `viewMode` state, its
//   `calendar-view-*` toggle ids and this boundary all speak one spelling.
//   `from_wire` rejects the SPA's historical `'list'`, and these wrappers turn
//   that into a thrown JS error rather than a silent no-op — a stale caller
//   fails loudly.
// - The **pan policy** is exposed as `pan`, not `panStep`: § Where logic lives
//   gives `pan_step` (the unit + magnitude) to the *natives*, who apply it with
//   their own platform date library, while "Rust/wasm consumers call `pan` for
//   the whole walk". Web is a `pan` consumer, so `pan_step` deliberately has no
//   wasm twin — exporting one would invite the SPA to re-implement the walk in
//   JS `Date` arithmetic, which is the drift this lift removes.
// - The **visible days** come from `visibleDays`. The month grid keeps using
//   `monthGrid` above, which carries the `inMonth` padding flag the SPA's cells
//   need; `visibleDays` is the mode-generic range the week columns want.

/// `fauna_core::caltime::pan` — apply one `events-prev-month`/`events-next-month`
/// click to `(year, month, day)` in view `mode`, moving **one visible range**:
/// a month in month view, a week in week, a day in day, and nothing at all in
/// the date-unfiltered agenda (`events.md` § User actions).
///
/// `mode` is the [`CalendarViewMode::as_wire`] spelling; `forward` picks the
/// direction (`false` = `events-prev-month`). Returns the new anchor as
/// `{ year, month, day }` — for agenda, the anchor unchanged, so the SPA can
/// wire the buttons unconditionally and let the shared policy decide, exactly
/// as linux and tui do.
///
/// A month step clamps the day onto a shorter month (Jan 31 → Feb 28),
/// deliberately lossily — see the `fauna_core` docs.
#[wasm_bindgen(js_name = pan)]
pub fn pan(mode: &str, year: i32, month: u32, day: u32, forward: bool) -> Result<JsValue, JsValue> {
    let mode = parse_view_mode(mode)?;
    let direction = if forward {
        fauna_core::caltime::PanDirection::Forward
    } else {
        fauna_core::caltime::PanDirection::Backward
    };
    to_js(&CalDate::from(fauna_core::caltime::pan(
        mode,
        (year, month, day),
        direction,
    )))
}

/// `fauna_core::caltime::visible_days` — the dates view `mode` shows for the
/// anchor `(year, month, day)`: the month grid's 42 cells, the week's 7
/// day-columns, the single day, or an empty array for the date-unfiltered
/// agenda. `week_start` is the client's locale week start (`0 = Mon … 6 = Sun`),
/// which stays a per-app lookup (`events.md` § Week & day timeline views).
#[wasm_bindgen(js_name = visibleDays)]
pub fn visible_days(
    mode: &str,
    year: i32,
    month: u32,
    day: u32,
    week_start: u32,
) -> Result<JsValue, JsValue> {
    let mode = parse_view_mode(mode)?;
    let days: Vec<CalDate> =
        fauna_core::caltime::visible_days(mode, (year, month, day), week_start)
            .into_iter()
            .map(CalDate::from)
            .collect();
    to_js(&days)
}

/// A 24-hour time of day crossing the boundary as `{ hour, minute }`.
#[derive(serde::Serialize)]
struct TimeOfDay {
    hour: u32,
    minute: u32,
}

/// `fauna_core::caltime::WORKING_DAY_START` — the day cell's double-click
/// compose prefill hour (`events.md` § User actions: opening a day-cell
/// compose chooses the working start rather than midnight). Exported as the
/// `{ hour, minute }` pair, not a formatted string — the day-origin vs.
/// instant distinction the SPA's compose already draws stays client-side.
#[wasm_bindgen(js_name = workingDayStart)]
pub fn working_day_start() -> Result<JsValue, JsValue> {
    let (hour, minute) = fauna_core::caltime::WORKING_DAY_START;
    to_js(&TimeOfDay { hour, minute })
}

/// Parse a [`CalendarViewMode::as_wire`] spelling, turning an unknown one into a
/// thrown JS error. The SPA's historical `'list'` for the agenda lands here.
fn parse_view_mode(mode: &str) -> Result<fauna_core::caltime::CalendarViewMode, JsValue> {
    fauna_core::caltime::CalendarViewMode::from_wire(mode).ok_or_else(|| {
        JsValue::from_str(&format!(
            "unknown calendar view mode {mode:?} — expected one of \
             \"agenda\", \"month\", \"week\", \"day\""
        ))
    })
}
