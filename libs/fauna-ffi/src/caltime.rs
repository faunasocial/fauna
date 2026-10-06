//! The parity-bearing `caltime` primitives exposed to UniFFI natives:
//! week/day overlap column packing, the full day-column layout (all-day/timed
//! classification + timed minute geometry + packing), and the event-datetime
//! input normalization. `events.md` § Where logic lives keeps natives on their
//! platform date library for Gregorian *date math* (week dates, month grids —
//! conceptual layout where platform libraries are strictly better); what
//! crosses here is the **classification / geometry / normalization** set,
//! where per-app re-derivation drifted into real rendering bugs and the
//! results need bit-identical cross-app parity. The records are
//! fauna-ffi-LOCAL (no bare `fauna_core` type crosses the boundary), so the Go
//! binding stays self-contained — no Cargo feature gate (mirrors `markdown`).

/// One timed event's `[start, end)` minutes-from-midnight, for overlap layout.
#[derive(uniffi::Record)]
pub struct FfiEventInterval {
    pub start_min: u32,
    pub end_min: u32,
}

/// Sub-column assignment for one event in a week/day time grid. Block width is
/// `1 / total_columns` of the day column; block x-offset is `column_index`.
#[derive(uniffi::Record)]
pub struct FfiOverlapColumn {
    pub event_index: u32,
    pub column_index: u32,
    pub total_columns: u32,
}

/// Side-by-side column packing for overlapping timed events
/// (`fauna_core::caltime::find_overlaps`). One entry per input event, in input
/// order.
#[uniffi::export]
pub fn find_event_overlaps(events: Vec<FfiEventInterval>) -> Vec<FfiOverlapColumn> {
    let intervals: Vec<(u32, u32)> = events.iter().map(|e| (e.start_min, e.end_min)).collect();
    fauna_core::caltime::find_overlaps(&intervals)
        .into_iter()
        .map(|(i, col, total)| FfiOverlapColumn {
            event_index: i as u32,
            column_index: col,
            total_columns: total,
        })
        .collect()
}

/// One event's raw `(dtstart, dtend)` strings, as stored/synced — the input to
/// [`day_column_layout`]. Which events belong to the day (the date filter)
/// stays client-side.
#[derive(uniffi::Record)]
pub struct FfiDayEvent {
    pub start: String,
    pub end: Option<String>,
}

/// One event's placement in a week/day time-grid column
/// ([`fauna_core::caltime::EventPlacement`] flattened). `all_day: true` → an
/// all-day-band chip (the minute/column fields are 0 and meaningless);
/// otherwise a positioned block: `[start_min, end_min)` minutes from the
/// column-day midnight (`end_min` clamped into `[start_min + 30, 1440]`), with
/// block width `1 / total_columns` of the day column and x-offset
/// `column_index`. One entry per input event, in input order.
#[derive(uniffi::Record)]
pub struct FfiDayEventPlacement {
    pub all_day: bool,
    pub start_min: u32,
    pub end_min: u32,
    pub column_index: u32,
    pub total_columns: u32,
}

/// Shared all-day classification (`fauna_core::caltime::is_all_day`) — the one
/// cross-app union rule: no end, a date-only DTSTART (RFC 5545 `VALUE=DATE`
/// semantics), or a midnight-to-midnight span of ≥ 1 whole day.
#[uniffi::export]
pub fn event_is_all_day(start: String, end: Option<String>) -> bool {
    fauna_core::caltime::is_all_day(&start, end.as_deref())
}

/// Complete all-day/timed layout for one day column
/// (`fauna_core::caltime::day_column_layout`): [`event_is_all_day`]
/// classification, timed minute geometry (an event crossing midnight renders
/// start → 24:00, never a collapsed 30-minute block or an overflow past the
/// grid), and [`find_event_overlaps`] column packing in one call.
#[uniffi::export]
pub fn day_column_layout(events: Vec<FfiDayEvent>) -> Vec<FfiDayEventPlacement> {
    let inputs: Vec<(&str, Option<&str>)> = events
        .iter()
        .map(|e| (e.start.as_str(), e.end.as_deref()))
        .collect();
    fauna_core::caltime::day_column_layout(&inputs)
        .into_iter()
        .map(|p| match p {
            fauna_core::caltime::EventPlacement::AllDay => FfiDayEventPlacement {
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
            } => FfiDayEventPlacement {
                all_day: false,
                start_min,
                end_min,
                column_index,
                total_columns,
            },
        })
        .collect()
}

/// Shared event-datetime input normalization
/// (`fauna_core::caltime::normalize_event_datetime_input`) for the combined
/// `event-dtstart` / `event-dtend` text fields: pads a bare `…THH:MM` to
/// `…THH:MM:00` (the A2-regression rule apple's `EventDateInput.parse`
/// pioneered) and unifies a space-separated date+time to `T`; seconds-bearing,
/// zoned, date-only, and unrecognizable input passes through unchanged.
#[uniffi::export]
pub fn normalize_event_datetime_input(input: String) -> String {
    fauna_core::caltime::normalize_event_datetime_input(&input)
}

/// The cross-app calendar view-mode vocabulary
/// (`fauna_core::caltime::CalendarViewMode`).
///
/// A native holding its own `LIST`/`'list'` spelling is exactly the drift this
/// replaces: `Agenda` is the one name for the date-unfiltered list.
///
/// ⚠ This doc used to link `calendar_view_mode_from_wire`, **which has never
/// existed** — `fauna_core::caltime::CalendarViewMode::from_wire` is deliberately
/// unexposed over UniFFI. Both natives whose view mode was a bare string (apple
/// 2026-08-23, windows 2026-08-24) adopted this enum without needing it: a native
/// never parses a string into a mode, so holding the enum deletes the strings
/// instead. `from_wire`'s refusal of an unknown spelling still guards the
/// Rust/wasm side, where wire input really is parsed.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiCalendarViewMode {
    Agenda,
    Month,
    Week,
    Day,
}

impl From<FfiCalendarViewMode> for fauna_core::caltime::CalendarViewMode {
    fn from(m: FfiCalendarViewMode) -> Self {
        match m {
            FfiCalendarViewMode::Agenda => fauna_core::caltime::CalendarViewMode::Agenda,
            FfiCalendarViewMode::Month => fauna_core::caltime::CalendarViewMode::Month,
            FfiCalendarViewMode::Week => fauna_core::caltime::CalendarViewMode::Week,
            FfiCalendarViewMode::Day => fauna_core::caltime::CalendarViewMode::Day,
        }
    }
}

impl From<fauna_core::caltime::CalendarViewMode> for FfiCalendarViewMode {
    fn from(m: fauna_core::caltime::CalendarViewMode) -> Self {
        match m {
            fauna_core::caltime::CalendarViewMode::Agenda => FfiCalendarViewMode::Agenda,
            fauna_core::caltime::CalendarViewMode::Month => FfiCalendarViewMode::Month,
            fauna_core::caltime::CalendarViewMode::Week => FfiCalendarViewMode::Week,
            fauna_core::caltime::CalendarViewMode::Day => FfiCalendarViewMode::Day,
        }
    }
}

/// How far one `events-prev-month`/`events-next-month` click moves the anchor
/// date, in the unit the caller's own date library speaks
/// (`fauna_core::caltime::PanStep`).
///
/// **The magnitude only — the sign is the caller's**, exactly as the Rust
/// [`fauna_core::caltime::pan_step`] returns it: a native applies it with
/// `date.plusWeeks(1)` / `minusWeeks(1)` (or `Calendar.current.date(byAdding:)`,
/// or `.NET DateTime.AddMonths`). That split is what keeps Gregorian date math
/// on the platform library while the *policy* stays shared — `events.md`
/// § Where logic lives. `pan()` itself is deliberately NOT exported here: it is
/// for the Rust and wasm consumers, which have this module's date math.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiPanStep {
    /// Step whole months, clamping the day onto a shorter month (Jan 31 → Feb 28).
    Months {
        count: i32,
    },
    Days {
        count: i32,
    },
    /// The view shows no date range, so the pan control must do nothing.
    None,
}

/// The shared pan policy: how far one click moves in `mode`
/// (`fauna_core::caltime::pan_step`) — one visible range per click.
///
/// `Agenda` answers [`FfiPanStep::None`] because the agenda list is
/// date-unfiltered on every app, so a pan there would mutate state nothing
/// renders. A caller must branch on that arm rather than treating it as zero:
/// "do nothing" is the contract, not "move by 0".
#[uniffi::export]
pub fn calendar_pan_step(mode: FfiCalendarViewMode) -> FfiPanStep {
    match fauna_core::caltime::pan_step(mode.into()) {
        fauna_core::caltime::PanStep::Months(count) => FfiPanStep::Months { count },
        fauna_core::caltime::PanStep::Days(count) => FfiPanStep::Days { count },
        fauna_core::caltime::PanStep::None => FfiPanStep::None,
    }
}

/// The mode's cross-app wire spelling — the string the automation surface
/// reports and view state persists (`calendar-view-{wire}` element ids are
/// built from it).
#[uniffi::export]
pub fn calendar_view_mode_wire(mode: FfiCalendarViewMode) -> String {
    let core: fauna_core::caltime::CalendarViewMode = mode.into();
    core.as_wire().to_string()
}

/// Every mode in `calendar-view-*` toggle order — so a native builds its view
/// switcher from the shared vocabulary instead of hand-listing an order that
/// can drift from the other six apps.
#[uniffi::export]
pub fn calendar_view_modes() -> Vec<FfiCalendarViewMode> {
    fauna_core::caltime::CalendarViewMode::ALL
        .into_iter()
        .map(Into::into)
        .collect()
}

/// A 24-hour time of day, as an `(hour, minute)` pair crossing to natives.
#[derive(uniffi::Record, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiTimeOfDay {
    pub hour: u32,
    pub minute: u32,
}

/// The day cell's double-click compose prefill hour
/// (`fauna_core::caltime::WORKING_DAY_START`, 09:00) — `events.md` § User
/// actions: opening a day-cell compose chooses this hour rather than midnight.
/// Exported as the `(hour, minute)` pair, not a formatted string — the
/// day-origin vs. instant distinction each app's compose already draws stays
/// in the caller.
#[uniffi::export]
pub fn working_day_start() -> FfiTimeOfDay {
    let (hour, minute) = fauna_core::caltime::WORKING_DAY_START;
    FfiTimeOfDay { hour, minute }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_disjoint_and_overlapping() {
        // Disjoint → single column each.
        let disjoint = find_event_overlaps(vec![
            FfiEventInterval {
                start_min: 0,
                end_min: 60,
            },
            FfiEventInterval {
                start_min: 120,
                end_min: 180,
            },
        ]);
        assert_eq!(disjoint[0].total_columns, 1);
        assert_eq!(disjoint[1].total_columns, 1);

        // Two overlapping → two columns, distinct indices, in input order.
        let overlap = find_event_overlaps(vec![
            FfiEventInterval {
                start_min: 0,
                end_min: 120,
            },
            FfiEventInterval {
                start_min: 60,
                end_min: 180,
            },
        ]);
        assert_eq!(overlap[0].event_index, 0);
        assert_eq!(overlap[1].event_index, 1);
        assert_eq!(overlap[0].total_columns, 2);
        assert_ne!(overlap[0].column_index, overlap[1].column_index);
    }

    #[test]
    fn day_column_layout_face_delegates_and_flattens() {
        let placements = day_column_layout(vec![
            FfiDayEvent {
                start: "2026-07-16T22:00:00".into(),
                end: Some("2026-07-17T02:00:00".into()),
            },
            FfiDayEvent {
                start: "2026-07-16".into(),
                end: None,
            },
        ]);
        assert!(!placements[0].all_day);
        assert_eq!(placements[0].start_min, 22 * 60);
        assert_eq!(placements[0].end_min, 1440); // clamped, not 30-min or 1560
        assert_eq!(placements[0].total_columns, 1);
        assert!(placements[1].all_day);
    }

    #[test]
    fn is_all_day_and_normalize_faces_delegate() {
        assert!(event_is_all_day(
            "2026-04-01T00:00:00".into(),
            Some("2026-04-05T00:00:00".into())
        ));
        assert!(!event_is_all_day(
            "2026-03-22T09:00:00".into(),
            Some("2026-03-22T10:00:00".into())
        ));
        assert_eq!(
            normalize_event_datetime_input("2026-04-01T10:00".into()),
            "2026-04-01T10:00:00"
        );
    }

    /// The policy face answers one visible range per mode — and `Agenda` is
    /// `None`, not a zero step. A native that treated it as zero would still
    /// call its date library and still repaint; the contract is to do nothing.
    #[test]
    fn the_pan_policy_crosses_the_boundary_one_visible_range_per_mode() {
        assert_eq!(
            calendar_pan_step(FfiCalendarViewMode::Month),
            FfiPanStep::Months { count: 1 }
        );
        assert_eq!(
            calendar_pan_step(FfiCalendarViewMode::Week),
            FfiPanStep::Days { count: 7 }
        );
        assert_eq!(
            calendar_pan_step(FfiCalendarViewMode::Day),
            FfiPanStep::Days { count: 1 }
        );
        assert_eq!(
            calendar_pan_step(FfiCalendarViewMode::Agenda),
            FfiPanStep::None
        );
    }

    /// The boundary reports the shared wire word — `agenda`, never the `list`
    /// spelling android and web each invented independently. (Parsing the other
    /// direction is deliberately NOT exported yet: no native consumes it, and
    /// this doc's exposure rule mints a face with its first consumer. windows
    /// and apple, whose view mode is a bare string, will want it.)
    #[test]
    fn the_boundary_reports_the_shared_wire_word() {
        assert_eq!(
            calendar_view_mode_wire(FfiCalendarViewMode::Agenda),
            "agenda"
        );
        assert_eq!(calendar_view_mode_wire(FfiCalendarViewMode::Month), "month");
        assert_eq!(calendar_view_mode_wire(FfiCalendarViewMode::Week), "week");
        assert_eq!(calendar_view_mode_wire(FfiCalendarViewMode::Day), "day");
    }

    /// The working-start face delegates verbatim — the (hour, minute) pair, not
    /// a formatted string, so the day-origin-vs-instant call stays with the
    /// caller.
    #[test]
    fn working_day_start_face_delegates() {
        assert_eq!(
            working_day_start(),
            FfiTimeOfDay {
                hour: fauna_core::caltime::WORKING_DAY_START.0,
                minute: fauna_core::caltime::WORKING_DAY_START.1,
            }
        );
    }

    /// The toggle order is the shared one, so seven switchers cannot drift.
    #[test]
    fn the_mode_list_is_the_shared_toggle_order() {
        let wires: Vec<String> = calendar_view_modes()
            .into_iter()
            .map(calendar_view_mode_wire)
            .collect();
        assert_eq!(wires, vec!["agenda", "month", "week", "day"]);
    }
}
