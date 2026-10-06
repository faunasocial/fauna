//! Pure, portable calendar/date-time math for calendar & events views.
//!
//! Everything here is deterministic arithmetic over `(year, month, day)` /
//! timestamp strings — **no platform glue**: no libc, no GTK, no system clock,
//! no locale lookups. That keeps the module WASM-safe so the web SPA and the
//! native apps (via UniFFI) share one implementation instead of each
//! re-deriving Gregorian date math (priority #2).
//!
//! The "now"/locale helpers (`today()`, `now_hm()`, `locale_week_start()`) and
//! localized month/weekday *names* stay per-app: each platform supplies its
//! own system time + locale, and localized display strings belong to the
//! client's i18n layer (`events.md` § Where logic lives). This module is the
//! deterministic core those client helpers feed into.
//!
//! Weekday convention throughout: **0 = Monday … 6 = Sunday**.

use std::collections::HashMap;

// ── Basic calendar math ─────────────────────────────────────────────────────

/// Whether `year` is a leap year (proleptic Gregorian).
pub fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Days in a month (handles leap years).
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Day of week for a given date (0=Mon, 6=Sun).
/// Uses Tomohiko Sakamoto's algorithm.
pub fn day_of_week(year: i32, month: u32, day: u32) -> u32 {
    // Sakamoto's algorithm returns 0=Sun..6=Sat; we remap to 0=Mon..6=Sun.
    static T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year - 1 } else { year };
    let d = day as i32;
    let dow_sun = (y + y / 4 - y / 100 + y / 400 + T[(month as usize) - 1] + d).rem_euclid(7);
    // dow_sun: 0=Sun,1=Mon,...,6=Sat → remap to 0=Mon..6=Sun
    ((dow_sun + 6) % 7) as u32
}

/// The month before `(year, month)`, wrapping the year at January.
pub fn prev_month(year: i32, month: u32) -> (i32, u32) {
    if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

/// The month after `(year, month)`, wrapping the year at December.
pub fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

/// Generate the 6×7=42 grid of (year, month, day) for a month view.
/// Pads with days from the previous/next months to fill the grid.
/// `week_start`: 0=Monday, 6=Sunday.
pub fn month_grid(year: i32, month: u32, week_start: u32) -> Vec<(i32, u32, u32)> {
    let first_dow = day_of_week(year, month, 1); // 0=Mon..6=Sun
    // How many cells to fill before the 1st of the month.
    // Offset is relative to week_start.
    let offset = ((first_dow as i32 - week_start as i32).rem_euclid(7)) as u32;

    let mut grid = Vec::with_capacity(42);

    // Fill leading days from previous month.
    if offset > 0 {
        let (prev_year, prev_month) = prev_month(year, month);
        let prev_days = days_in_month(prev_year, prev_month);
        for d in (prev_days - offset + 1)..=prev_days {
            grid.push((prev_year, prev_month, d));
        }
    }

    // Fill current month.
    let cur_days = days_in_month(year, month);
    for d in 1..=cur_days {
        grid.push((year, month, d));
    }

    // Fill trailing days from next month.
    let (next_year, next_month) = next_month(year, month);
    let mut next_day = 1u32;
    while grid.len() < 42 {
        grid.push((next_year, next_month, next_day));
        next_day += 1;
    }

    grid
}

/// Get the start date of the week containing the given date.
/// Returns the date of the `week_start` day (0=Mon, 6=Sun) for that week.
pub fn week_start_date(year: i32, month: u32, day: u32, week_start: u32) -> (i32, u32, u32) {
    let dow = day_of_week(year, month, day); // 0=Mon..6=Sun
    let offset = (dow as i32 - week_start as i32).rem_euclid(7);
    add_days(year, month, day, -offset)
}

// ── Locale week start (the two Rust apps' shared probe) ─────────────────────

/// caltime's `week_start` (`0 = Mon … 6 = Sun`) for a POSIX/BCP-47 locale name
/// such as `en_US.UTF-8`, `ar_EG@islamic`, `en-GB` or `C`.
///
/// Pure string math — no environment, no platform calls — so it stays on the
/// module's WASM-safe surface and is unit-testable without touching
/// process-global state. The environment probe that feeds it is
/// [`locale_week_start`], which is native-only.
///
/// **This table is a curated subset of CLDR `weekData/firstDay`, not a copy of
/// it.** Only the two Rust apps (linux, tui) consume it: the other five read
/// their platform's own full CLDR data — Swift `Calendar.firstWeekday`, .NET
/// `CultureInfo.CurrentCulture.DateTimeFormat.FirstDayOfWeek`, Kotlin
/// `WeekFields` — because Rust ships no locale-aware calendar library
/// (`events.md` § Where logic lives keeps the probe per-app for exactly that
/// reason). An unlisted region, an unparseable value, and the `C`/`POSIX`
/// locale all fall back to **Monday** — ISO-8601's default and the most common
/// answer worldwide, so an unknown region degrades toward the majority rather
/// than toward whichever locale happened to be special-cased. Widening the
/// coverage is a table row plus a test row.
pub fn week_start_for_locale(locale: &str) -> u32 {
    /// Friday-start regions (caltime 4).
    const FRIDAY: &[&str] = &["BD", "MV"];
    /// Saturday-start regions (caltime 5).
    const SATURDAY: &[&str] = &[
        "AE", "AF", "BH", "DZ", "EG", "IQ", "IR", "JO", "KW", "LY", "OM", "QA", "SA", "SD", "SY",
        "YE",
    ];
    /// Sunday-start regions (caltime 6).
    const SUNDAY: &[&str] = &[
        "AU", "BR", "CA", "CO", "DO", "GT", "HK", "HN", "ID", "IL", "IN", "JM", "JP", "KE", "KR",
        "MX", "NI", "PA", "PE", "PH", "PK", "PR", "PY", "SG", "SV", "TH", "TW", "US", "VE", "ZA",
    ];

    let Some(region) = locale_region(locale) else {
        return 0; // Monday
    };
    let region = region.as_str();
    if FRIDAY.contains(&region) {
        4
    } else if SATURDAY.contains(&region) {
        5
    } else if SUNDAY.contains(&region) {
        6
    } else {
        0
    }
}

/// The uppercased region subtag of a locale name, or `None` when it carries no
/// region (`nb`, `C`, `POSIX`, empty, junk).
///
/// Handles the shapes that actually reach us: an encoding suffix (`.UTF-8`), a
/// modifier suffix (`@valencia`), both at once, the BCP-47 hyphen separator,
/// and a lowercase region.
fn locale_region(locale: &str) -> Option<String> {
    // Strip `@modifier`, then `.encoding` — in that order, since a modifier can
    // follow the encoding (`ar_EG.UTF-8@islamic`).
    let base = locale.split('@').next().unwrap_or("");
    let base = base.split('.').next().unwrap_or("");
    let region = base.split(['_', '-']).nth(1)?;
    if region.len() != 2 || !region.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(region.to_ascii_uppercase())
}

/// The client's locale week start (`0 = Mon … 6 = Sun`), probed from the POSIX
/// environment — the locale mechanism a Rust process actually has.
///
/// Precedence is POSIX's own: `LC_ALL` overrides `LC_TIME`, which overrides
/// `LANG`; the first one set to a non-empty value wins. Cheap enough to call
/// per render (an environ scan, no syscall), and deliberately uncached so a
/// test or an e2e run can set the variable for one launch and observe the
/// grid change.
///
/// Native-only by feature gate, not by accident: this is the platform glue
/// `events.md` § Where logic lives keeps out of the shared date math, and the
/// web SPA must read `Intl` rather than this. The classification itself lives
/// in the pure, always-available [`week_start_for_locale`].
#[cfg(feature = "local-clock")]
pub fn locale_week_start() -> u32 {
    for var in ["LC_ALL", "LC_TIME", "LANG"] {
        match std::env::var(var) {
            Ok(v) if !v.is_empty() => return week_start_for_locale(&v),
            _ => continue,
        }
    }
    0 // Monday
}

/// The device's current local UTC offset, in seconds — the ONE place in the
/// tree that reads it, so a nest-facing field (`fauna.family.usage_report`,
/// `fauna.family.notify_report`) and a display path can never independently
/// disagree about it. Before this existed the same expression was hand-rolled
/// three times — `fauna_core::screen_time` and each of tui's and linux's own
/// "device-offset door" — two of them nest-facing on the same
/// `family-safety.md` § Screen time day-bucket rule, and one of the two using
/// **glib** instead of chrono, a genuine divergence surface (they can
/// disagree about a zone the OS reports mid-transition), not just a copy.
///
/// `chrono::Local` reads the system zone **thread-safely** — unlike `time`'s
/// `current_local_offset`, which errors under threads. Do not "simplify" to
/// `time`. Native-only by feature gate: wasm32 has no OS timezone database
/// without a JS-interop bridge this crate doesn't otherwise carry.
///
/// A caller needing **minutes** (every nest-facing field does) divides by 60;
/// real UTC offsets are always whole minutes, so the division is exact.
#[cfg(feature = "local-clock")]
pub fn local_utc_offset_seconds() -> i32 {
    chrono::Local::now().offset().local_minus_utc()
}

/// Add N days to a date (N can be negative). Returns (year, month, day).
pub fn add_days(year: i32, month: u32, day: u32, n: i32) -> (i32, u32, u32) {
    // Convert to a simple day counter, walk, convert back.
    let mut y = year;
    let mut m = month;
    let mut d = day as i32 + n;

    // Walk forward.
    while d > days_in_month(y, m) as i32 {
        d -= days_in_month(y, m) as i32;
        let (ny, nm) = next_month(y, m);
        y = ny;
        m = nm;
    }
    // Walk backward.
    while d < 1 {
        let (py, pm) = prev_month(y, m);
        y = py;
        m = pm;
        d += days_in_month(y, m) as i32;
    }

    (y, m, d as u32)
}

// ── View mode + visible range ────────────────────────────────────────────────
//
// The Events page's *view state*: which of the four views is active, and which
// dates it shows. Shared because it is **policy, not Gregorian date math** —
// `events.md` § Where logic lives keeps date arithmetic per-app on the natives'
// platform date libraries (Swift `Calendar`, .NET `DateTime`, `java.time`), and
// this section respects that split: [`pan_step`] hands a native the *unit and
// magnitude* to step, which it applies with its own library, while the pure-Rust
// and wasm consumers get the whole walk from [`pan`].
//
// It exists because one ui.yaml action — `events-prev-month`/`events-next-month`,
// which `events.md` § User actions defines as **"pan visible range"** — had six
// different behaviours across the seven apps. In week view alone a click moved
// the grid a month (tui/windows/macos/ios), a week (linux/android), or nothing
// at all (web). Only linux and android matched the spec'd semantic.

/// Which of the four Events views is active (`ui.yaml`'s `calendar-view-agenda`
/// / `-month` / `-week` / `-day` toggles).
///
/// [`as_wire`](Self::as_wire) is the single cross-app vocabulary — the string
/// each app's automation surface reports and its view state persists. web's SPA
/// historically called the agenda view `'list'`; that spelling is deliberately
/// not accepted by [`from_wire`](Self::from_wire), so a stale caller fails
/// loudly rather than silently selecting nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum CalendarViewMode {
    /// The date-unfiltered event list ("upcoming") — the cross-app default.
    #[default]
    Agenda,
    Month,
    Week,
    Day,
}

/// How one `events-prev-month`/`events-next-month` click moves the anchor date.
///
/// A native applies this with its own date library (`date.plusMonths(1)`,
/// `Calendar.current.date(byAdding: .month, …)`); Rust/wasm consumers call
/// [`pan`] and skip the unit entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanStep {
    /// Step whole months, clamping the day onto a shorter month (Jan 31 → Feb 28).
    Months(i32),
    Days(i32),
    /// The view shows no date range, so the pan control does nothing.
    None,
}

impl CalendarViewMode {
    /// Every mode, in `calendar-view-*` toggle order.
    pub const ALL: [CalendarViewMode; 4] = [
        CalendarViewMode::Agenda,
        CalendarViewMode::Month,
        CalendarViewMode::Week,
        CalendarViewMode::Day,
    ];

    /// The cross-app vocabulary — see the type docs.
    pub fn as_wire(self) -> &'static str {
        match self {
            CalendarViewMode::Agenda => "agenda",
            CalendarViewMode::Month => "month",
            CalendarViewMode::Week => "week",
            CalendarViewMode::Day => "day",
        }
    }

    /// Parse [`as_wire`](Self::as_wire). Unknown spellings return `None`.
    pub fn from_wire(s: &str) -> Option<CalendarViewMode> {
        CalendarViewMode::ALL.into_iter().find(|m| m.as_wire() == s)
    }
}

/// Which direction a pan control moves the anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanDirection {
    Backward,
    Forward,
}

impl PanDirection {
    fn sign(self) -> i32 {
        match self {
            PanDirection::Backward => -1,
            PanDirection::Forward => 1,
        }
    }
}

/// How far one pan click moves in `mode` — the shared policy, one visible range
/// per click.
///
/// `Agenda` is [`PanStep::None`] because the agenda list is date-unfiltered on
/// every app: linux stepped its anchor 7 days while its own list ignored the
/// date, and tui moved the month label above a list that did not change. A
/// control that mutates invisible state is worse than one that does nothing, so
/// the uniform answer is to do nothing.
pub fn pan_step(mode: CalendarViewMode) -> PanStep {
    match mode {
        CalendarViewMode::Month => PanStep::Months(1),
        CalendarViewMode::Week => PanStep::Days(7),
        CalendarViewMode::Day => PanStep::Days(1),
        CalendarViewMode::Agenda => PanStep::None,
    }
}

/// Apply one pan click to `anchor` — [`pan_step`] walked with this module's own
/// date math, for the Rust and wasm consumers.
///
/// A month step clamps the day onto the target month (Jan 31 → Feb 28), which
/// is lossy on purpose: it is the rule linux and tui both already applied, and
/// the alternative (remembering the original day-of-month) is state a pan
/// control should not carry.
pub fn pan(
    mode: CalendarViewMode,
    anchor: (i32, u32, u32),
    direction: PanDirection,
) -> (i32, u32, u32) {
    let (y, m, d) = anchor;
    match pan_step(mode) {
        PanStep::None => anchor,
        PanStep::Days(n) => add_days(y, m, d, n * direction.sign()),
        PanStep::Months(n) => {
            let mut ym = (y, m);
            for _ in 0..n.abs() {
                ym = if n * direction.sign() < 0 {
                    prev_month(ym.0, ym.1)
                } else {
                    next_month(ym.0, ym.1)
                };
            }
            (ym.0, ym.1, d.min(days_in_month(ym.0, ym.1)))
        }
    }
}

/// The dates `mode` shows for `anchor` — the month grid's 42 cells, the week's
/// 7 day-columns, the single day, or nothing for the date-unfiltered agenda.
///
/// `week_start` is the client's locale week start (0=Mon … 6=Sun), which stays
/// per-app: `events.md` § Week & day timeline views makes week-start a platform
/// lookup, and this function takes the answer rather than deriving it.
pub fn visible_days(
    mode: CalendarViewMode,
    anchor: (i32, u32, u32),
    week_start: u32,
) -> Vec<(i32, u32, u32)> {
    let (y, m, d) = anchor;
    match mode {
        CalendarViewMode::Agenda => Vec::new(),
        CalendarViewMode::Month => month_grid(y, m, week_start),
        CalendarViewMode::Week => {
            let (sy, sm, sd) = week_start_date(y, m, d, week_start);
            (0..7).map(|i| add_days(sy, sm, sd, i)).collect()
        }
        CalendarViewMode::Day => vec![anchor],
    }
}

// ── Timestamp parsing ─────────────────────────────────────────────────────────
//
// Calendar/event timestamps arrive as RFC 3339 / ISO 8601 strings (the
// `fauna_core::ical` writer/parser normalises DTSTART to that shape). The
// epoch-millis branches are a defensive fallback; to stay WASM-safe (no libc)
// they interpret the epoch instant as **UTC** — consistent between
// `parse_time` and `parse_date`. (Clients that need wall-clock-local epoch
// handling do that in their own platform glue.)

/// The day's **working start** as `(hour, minute)` — the time a compose that
/// was given a *day* rather than an *instant* prefills.
///
/// Its one caller class is the month grid's day-cell double-click
/// (`events-day-cell-{date}`, events.md § Layout & flow): the cell names a day,
/// so the compose has to choose an hour, and Outlook — the declared UX
/// reference for this interaction — opens the working day. A click on an
/// `events-time-slot-{HH-MM}` is *not* this case: that carries a real instant
/// and is used verbatim, midnight included.
///
/// Shared because it is one product policy that had been written out
/// independently four times, covering five apps — tui's
/// `DEFAULT_COMPOSE_HOUR`, web's `T09:00` literal, windows'
/// `NewEventAtSlot(date, 9, 0)`, and apple's `bySettingHour: 9` (macos + ios
/// off one Swift path) — while linux, the sixth app, had silently never
/// adopted it at all: it passed no time, composing an **all-day** event where
/// the other five compose at 09:00. That divergence survived because the shared e2e asserted
/// only the prefilled *date*, which a date-only value satisfies trivially —
/// found 2026-08-12 by widening the shared day-cell double-click e2e to assert
/// the whole `dtstart` rather than just its date half.
///
/// Consumed directly by tui + linux; web, apple (macOS + iOS), and windows
/// consume it via the wasm + UniFFI faces — no per-app copy remains.
pub const WORKING_DAY_START: (u32, u32) = (9, 0);

/// Parse a timestamp string to (hour, minute). Returns None if date-only.
/// Handles ISO 8601 "2026-03-22T09:30:00" and epoch-millis strings (UTC).
pub fn parse_time(timestamp: &str) -> Option<(u32, u32)> {
    // Try ISO 8601 with 'T'.
    if let Some(t_pos) = timestamp.find('T') {
        let time_part = &timestamp[t_pos + 1..];
        let parts: Vec<&str> = time_part.splitn(3, ':').collect();
        if parts.len() >= 2 {
            let h: u32 = parts[0].parse().ok()?;
            let m: u32 = parts[1].parse().ok()?;
            return Some((h, m));
        }
    }

    // Try epoch millis: pure numeric string.
    if timestamp.chars().all(|c| c.is_ascii_digit())
        && let Ok(millis) = timestamp.parse::<i64>()
    {
        let secs = millis / 1000;
        let mins_total = secs / 60;
        let h = (mins_total / 60).rem_euclid(24) as u32;
        let m = (mins_total % 60) as u32;
        return Some((h, m));
    }

    None
}

/// Parse a timestamp to (year, month, day). Handles ISO dates and epoch-millis
/// (interpreted as UTC — see module note).
pub fn parse_date(timestamp: &str) -> Option<(i32, u32, u32)> {
    // Try ISO 8601: "YYYY-MM-DD..." (at least 10 chars).
    if timestamp.len() >= 10 && timestamp.as_bytes()[4] == b'-' && timestamp.as_bytes()[7] == b'-' {
        let y: i32 = timestamp[0..4].parse().ok()?;
        let mo: u32 = timestamp[5..7].parse().ok()?;
        let d: u32 = timestamp[8..10].parse().ok()?;
        return Some((y, mo, d));
    }

    // Try epoch millis → UTC civil date (pure, no libc).
    if timestamp.chars().all(|c| c.is_ascii_digit())
        && let Ok(millis) = timestamp.parse::<i64>()
    {
        let secs = millis.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        return Some(civil_from_days(days));
    }

    None
}

/// Convert a count of days since the Unix epoch (1970-01-01) to a proleptic
/// Gregorian `(year, month, day)`, in UTC. Howard Hinnant's `civil_from_days`
/// (public domain) — <http://howardhinnant.github.io/date_algorithms.html>.
/// Correct for every `i64` day count, including the negative (pre-epoch)
/// range. The canonical implementation — every hand-rolled copy elsewhere in
/// the workspace should delegate here instead (priority #2).
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

/// Split epoch seconds into (days since 1970-01-01, hour, minute, second).
/// `div_euclid`/`rem_euclid`, not `/`/`%`: truncating division rounds a
/// pre-1970 instant toward zero and lands it on the wrong day/second. Pair
/// with [`civil_from_days`] for the full `(y, m, d, hh, mm, ss)` breakdown.
pub fn epoch_secs_to_days_and_time(epoch_secs: i64) -> (i64, u32, u32, u32) {
    const SECS_IN_DAY: i64 = 86400;
    let total_days = epoch_secs.div_euclid(SECS_IN_DAY);
    let day_secs = epoch_secs.rem_euclid(SECS_IN_DAY);
    let hh = (day_secs / 3600) as u32;
    let mm = ((day_secs % 3600) / 60) as u32;
    let ss = (day_secs % 60) as u32;
    (total_days, hh, mm, ss)
}

// ── Event helpers ──────────────────────────────────────────────────────────

/// Determine if an event is all-day — the one cross-app classification rule
/// (the union of the per-app rules that had drifted; events.md § Where
/// logic lives). All-day if:
/// - `end_time` is `None`, or
/// - the start is **date-only** (no time component — RFC 5545 `VALUE=DATE`
///   semantics: the DTSTART type governs, regardless of the end's shape), or
/// - the span runs **midnight to midnight over ≥ 1 whole day** (multi-day
///   included — "Vacation Apr 1–4" is an all-day chip, not a timed block).
///
/// Date-only means neither `T` nor `:` — a space-separated timed datetime
/// (`"2026-03-22 10:00"`) carries a time and is NOT all-day.
pub fn is_all_day(start_time: &str, end_time: Option<&str>) -> bool {
    fn date_only(s: &str) -> bool {
        !s.contains('T') && !s.contains(':')
    }

    // No end time → all-day.
    let end = match end_time {
        None => return true,
        Some(e) => e.trim(),
    };
    let start = start_time.trim();

    // A date-only DTSTART is all-day by type.
    if date_only(start) {
        return true;
    }

    // Midnight-to-midnight spanning one or more whole days.
    if let (Some((0, 0)), Some((0, 0))) = (parse_time(start), parse_time(end))
        && let (Some(sd), Some(ed)) = (parse_date(start), parse_date(end))
        && days_from_civil(ed.0, ed.1, ed.2) > days_from_civil(sd.0, sd.1, sd.2)
    {
        return true;
    }

    false
}

/// Normalize the combined datetime text-field input (`event-dtstart` /
/// `event-dtend`, a `YYYY-MM-DDTHH:MM` `text_input` on all 7 apps) into a
/// seconds-bearing RFC 3339 datetime for the API — the one shared input rule
/// (lifted from apple `EventDateInput.parse`, which fixed the A2 regression:
/// a seconds-less `THH:MM` made the nest's iCalendar serializer emit a
/// malformed compact time no client's parser could read, so the event vanished
/// from every time grid).
///
/// - a space-separated date+time (`"2026-04-01 10:00"`, the linux form's
///   legacy placeholder shape) unifies its separator to `T` first;
/// - a bare `…THH:MM` is padded to `…THH:MM:00`;
/// - input already carrying seconds or a `Z`/offset, a date-only value
///   (all-day by type), and anything unrecognizable pass through unchanged —
///   rejection stays the nest-side validator's job.
pub fn normalize_event_datetime_input(input: &str) -> String {
    let trimmed = input.trim();
    // Space-separated date+time → 'T'-separated (only when the prefix is a
    // real date and a time follows, so free text passes through).
    let unified = if !trimmed.contains('T')
        && trimmed.len() > 10
        && trimmed.as_bytes()[10] == b' '
        && parse_date(trimmed).is_some()
        && trimmed[11..].contains(':')
    {
        let mut s = trimmed.to_string();
        s.replace_range(10..11, "T");
        s
    } else {
        trimmed.to_string()
    };
    // Already carries seconds (HH:MM:SS) or a timezone → as-is.
    if unified.contains('Z') || unified.matches(':').count() >= 2 {
        return unified;
    }
    // A `…THH:MM` time → append `:00` seconds. A date-only value (no 'T',
    // no time) stays untouched.
    if unified.contains('T') && unified.contains(':') {
        return format!("{unified}:00");
    }
    unified
}

/// Inverse of [`civil_from_days`] — days since the Unix epoch for a proleptic
/// Gregorian date, in UTC. Howard Hinnant's `days_from_civil` (public domain).
/// The canonical implementation — every hand-rolled copy elsewhere in the
/// workspace should delegate here instead (priority #2).
pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(if month <= 2 { year - 1 } else { year });
    let m = i64::from(month);
    let d = i64::from(day);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// A strict, zero-padded `YYYY-MM-DD` → days since the Unix epoch (UTC).
///
/// The one parser behind every wizard's bare-date field (the archive import's
/// and the mailbox export's scope ranges): such a field names a **whole UTC
/// day**, never a moment, so there is deliberately no time zone, no partial
/// date and no locale — and anything that is not exactly this shape, or not a
/// real calendar day, is `None` rather than a guess. Negative for a pre-epoch
/// date; a caller whose clock is unsigned refuses those itself.
pub fn days_from_ymd(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 {
        return None;
    }
    let shaped = bytes.iter().enumerate().all(|(i, b)| {
        if i == 4 || i == 7 {
            *b == b'-'
        } else {
            b.is_ascii_digit()
        }
    });
    if !shaped {
        return None;
    }
    let year: i32 = value.get(0..4)?.parse().ok()?;
    let month: u32 = value.get(5..7)?.parse().ok()?;
    let day: u32 = value.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// One event's placement in a week/day time-grid column — one entry per input
/// event, input-order aligned (the `find_overlaps` convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventPlacement {
    /// Renders as a chip in the all-day band. Also the visible degrade for an
    /// event whose start time can't be parsed — a broken event shows up in the
    /// band rather than silently vanishing from the grid.
    AllDay,
    /// A positioned block: `[start_min, end_min)` minutes from the column-day
    /// midnight (`end_min` clamped into `[start_min + 30, 1440]`), plus the
    /// [`find_overlaps`] sub-column assignment (block width is
    /// `1 / total_columns` of the day column, x-offset is `column_index`).
    Timed {
        start_min: u32,
        end_min: u32,
        column_index: u32,
        total_columns: u32,
    },
}

/// Complete all-day/timed layout for one day column of a week/day time grid,
/// from the day's events' raw `(start, end)` datetime strings: [`is_all_day`]
/// classification, timed minute geometry, and [`find_overlaps`] column packing
/// in one call, so identical events lay out identically on every app.
///
/// Timed geometry: `start_min` is the start's minutes-from-midnight; `end_min`
/// is the minutes from the **start-day** midnight to the end — so an event
/// crossing midnight renders start → 24:00 (clamped to 1440), never a
/// collapsed 30-minute block (end time alone) or an overflow past the grid
/// (unclamped duration). A missing/unparseable/not-after-start end gets the
/// 30-minute floor. Which events belong to the day (the date filter) stays
/// client-side.
pub fn day_column_layout(events: &[(&str, Option<&str>)]) -> Vec<EventPlacement> {
    let mut placements = vec![EventPlacement::AllDay; events.len()];
    // (input position, start_min, end_min) per timed event.
    let mut timed: Vec<(usize, u32, u32)> = Vec::new();
    for (pos, (start, end)) in events.iter().enumerate() {
        if is_all_day(start, *end) {
            continue;
        }
        let start = start.trim();
        let Some((sh, sm)) = parse_time(start) else {
            continue; // unparseable start time → stays AllDay (visible)
        };
        let start_min = i64::from(sh * 60 + sm).min(1440);
        // Minutes from the start-day midnight to the end (may cross days).
        let end_min_raw = end.map(str::trim).and_then(|e| {
            let (eh, em) = parse_time(e)?;
            let sd = parse_date(start)?;
            let ed = parse_date(e)?;
            let day_span = days_from_civil(ed.0, ed.1, ed.2) - days_from_civil(sd.0, sd.1, sd.2);
            Some(day_span * 1440 + i64::from(eh * 60 + em))
        });
        let end_min = end_min_raw
            .unwrap_or(start_min + 30)
            .max(start_min + 30)
            .min(1440);
        timed.push((pos, start_min as u32, end_min as u32));
    }
    let intervals: Vec<(u32, u32)> = timed.iter().map(|&(_, s, e)| (s, e)).collect();
    for (i, column_index, total_columns) in find_overlaps(&intervals) {
        let (pos, start_min, end_min) = timed[i];
        placements[pos] = EventPlacement::Timed {
            start_min,
            end_min,
            column_index,
            total_columns,
        };
    }
    placements
}

// ── Overlap layout ───────────────────────────────────────────────────────────

/// Find overlapping event groups for sub-column layout in week/day view.
///
/// Input:  slice of (start_minutes, end_minutes) per event (end exclusive).
/// Output: Vec of (event_index, column_index, total_columns_in_group).
pub fn find_overlaps(events: &[(u32, u32)]) -> Vec<(usize, u32, u32)> {
    let n = events.len();
    if n == 0 {
        return vec![];
    }

    // Sort event indices by start time.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| events[i].0);

    // Assign columns greedily.
    let mut col: Vec<u32> = vec![0; n];
    let mut col_end: Vec<u32> = Vec::new(); // col_end[c] = latest end time in column c

    for &i in &order {
        let (start, _end) = events[i];
        // Find the first column whose last event ends by start.
        let mut placed = false;
        for (c, &ce) in col_end.iter().enumerate() {
            if ce <= start {
                col[i] = c as u32;
                col_end[c] = events[i].1;
                placed = true;
                break;
            }
        }
        if !placed {
            col[i] = col_end.len() as u32;
            col_end.push(events[i].1);
        }
    }

    // Group events that overlap into clusters; find max columns per cluster.
    // Two events are in the same cluster if their time ranges intersect (directly
    // or transitively).
    let mut group: Vec<usize> = (0..n).collect(); // group[i] = representative

    let find = |g: &Vec<usize>, mut x: usize| -> usize {
        while g[x] != x {
            x = g[x];
        }
        x
    };

    // Union-find merge for overlapping pairs.
    let mut g = group.clone();
    for i in 0..n {
        for j in (i + 1)..n {
            let (s1, e1) = events[i];
            let (s2, e2) = events[j];
            // Overlap: not (e1 <= s2 || e2 <= s1)
            if e1 > s2 && e2 > s1 {
                let ri = find(&g, i);
                let rj = find(&g, j);
                if ri != rj {
                    g[rj] = ri;
                }
            }
        }
    }
    // Path-compress.
    for (i, slot) in group.iter_mut().enumerate() {
        *slot = find(&g, i);
    }

    // For each group, find how many columns are used.
    let mut group_cols: HashMap<usize, u32> = HashMap::new();
    for i in 0..n {
        let root = group[i];
        let entry = group_cols.entry(root).or_insert(0);
        if col[i] + 1 > *entry {
            *entry = col[i] + 1;
        }
    }

    let mut result = vec![(0usize, 0u32, 0u32); n];
    for i in 0..n {
        let root = group[i];
        let total = *group_cols.get(&root).unwrap_or(&1);
        result[i] = (i, col[i], total);
    }

    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn days_from_ymd_is_strict_and_names_a_real_calendar_day() {
        assert_eq!(days_from_ymd("1970-01-01"), Some(0));
        assert_eq!(days_from_ymd("1970-01-02"), Some(1));
        assert_eq!(days_from_ymd("1969-12-31"), Some(-1));
        assert_eq!(days_from_ymd("2024-02-29"), Some(19_782));
        for refused in [
            "",
            "2020-1-1",
            "2020-13-01",
            "2023-02-29",
            "2020-00-10",
            "2020-01-00",
            "20200101",
            "2020/01/01",
            " 2020-01-01",
            "2020-01-01T00:00",
        ] {
            assert_eq!(days_from_ymd(refused), None, "{refused:?}");
        }
    }

    use super::*;

    // ── View mode + visible range ───────────────────────────────────────────

    #[test]
    fn view_mode_vocabulary_round_trips() {
        for mode in CalendarViewMode::ALL {
            assert_eq!(CalendarViewMode::from_wire(mode.as_wire()), Some(mode));
        }
        // The one vocabulary every app's `calendar-view-*` toggle speaks —
        // web's historical `'list'` for the agenda view is NOT it.
        assert_eq!(CalendarViewMode::Agenda.as_wire(), "agenda");
        assert_eq!(CalendarViewMode::from_wire("list"), None);
    }

    #[test]
    fn pan_step_is_one_visible_range_per_mode() {
        assert_eq!(pan_step(CalendarViewMode::Month), PanStep::Months(1));
        assert_eq!(pan_step(CalendarViewMode::Week), PanStep::Days(7));
        assert_eq!(pan_step(CalendarViewMode::Day), PanStep::Days(1));
        // Agenda is date-unfiltered on every app, so there is no range to pan.
        assert_eq!(pan_step(CalendarViewMode::Agenda), PanStep::None);
    }

    #[test]
    fn pan_month_clamps_onto_a_shorter_month() {
        // Jan 31 → Feb 28 (and back out to Mar 28, not Mar 31: the clamp is
        // lossy by design — the same rule linux and tui already applied).
        assert_eq!(
            pan(
                CalendarViewMode::Month,
                (2026, 1, 31),
                PanDirection::Forward
            ),
            (2026, 2, 28)
        );
        assert_eq!(
            pan(
                CalendarViewMode::Month,
                (2024, 1, 31),
                PanDirection::Forward
            ),
            (2024, 2, 29) // leap
        );
    }

    #[test]
    fn pan_wraps_the_year_in_both_directions() {
        assert_eq!(
            pan(
                CalendarViewMode::Month,
                (2026, 12, 15),
                PanDirection::Forward
            ),
            (2027, 1, 15)
        );
        assert_eq!(
            pan(
                CalendarViewMode::Month,
                (2026, 1, 15),
                PanDirection::Backward
            ),
            (2025, 12, 15)
        );
        assert_eq!(
            pan(CalendarViewMode::Day, (2026, 12, 31), PanDirection::Forward),
            (2027, 1, 1)
        );
    }

    #[test]
    fn pan_week_moves_seven_days_not_a_month() {
        // The divergence this lift exists to kill: tui/windows/apple stepped a
        // whole MONTH in week view, so the week grid jumped ~4 weeks per click.
        assert_eq!(
            pan(CalendarViewMode::Week, (2026, 7, 15), PanDirection::Forward),
            (2026, 7, 22)
        );
        assert_eq!(
            pan(
                CalendarViewMode::Week,
                (2026, 7, 15),
                PanDirection::Backward
            ),
            (2026, 7, 8)
        );
    }

    #[test]
    fn pan_agenda_is_a_no_op() {
        // linux moved `selected_date` by 7 days here while its agenda list
        // ignored the date entirely — an invisible state mutation. tui moved
        // the month label while the list stayed put. Neither is pannable.
        let anchor = (2026, 7, 15);
        assert_eq!(
            pan(CalendarViewMode::Agenda, anchor, PanDirection::Forward),
            anchor
        );
        assert_eq!(
            pan(CalendarViewMode::Agenda, anchor, PanDirection::Backward),
            anchor
        );
    }

    #[test]
    fn visible_days_matches_each_mode_s_grid() {
        let anchor = (2026, 7, 15); // a Wednesday
        // Month → the same 42-cell grid `month_grid` already produces.
        assert_eq!(
            visible_days(CalendarViewMode::Month, anchor, 0),
            month_grid(2026, 7, 0)
        );
        // Week → the 7 dates from the week start.
        let week = visible_days(CalendarViewMode::Week, anchor, 0);
        assert_eq!(week.len(), 7);
        assert_eq!(week[0], (2026, 7, 13)); // Monday
        assert_eq!(week[6], (2026, 7, 19)); // Sunday
        // Day → exactly the anchor.
        assert_eq!(visible_days(CalendarViewMode::Day, anchor, 0), vec![anchor]);
        // Agenda → no date range at all (it is date-unfiltered).
        assert!(visible_days(CalendarViewMode::Agenda, anchor, 0).is_empty());
    }

    #[test]
    fn visible_days_honours_the_locale_week_start() {
        // Sunday-start (week_start = 6) shifts the same week back one day.
        let week = visible_days(CalendarViewMode::Week, (2026, 7, 15), 6);
        assert_eq!(week[0], (2026, 7, 12)); // Sunday
        assert_eq!(week[6], (2026, 7, 18)); // Saturday
    }

    #[test]
    fn leap_years() {
        assert!(is_leap(2000)); // divisible by 400
        assert!(!is_leap(1900)); // divisible by 100, not 400
        assert!(is_leap(2024));
        assert!(!is_leap(2023));
        assert!(is_leap(2400));
    }

    #[test]
    fn days_per_month() {
        assert_eq!(days_in_month(2024, 2), 29); // leap Feb
        assert_eq!(days_in_month(2023, 2), 28); // non-leap Feb
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    #[test]
    fn civil_from_days_known_anchors() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 30 years after epoch: 7 leap years (72/76/80/84/88/92/96) + 23
        // common years = 7*366 + 23*365 = 10_957 days.
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        // 2000 is a leap year: Jan(31) + Feb(29) = 60 days to March 1.
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
    }

    #[test]
    fn days_from_civil_known_anchors() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn civil_from_days_round_trips_days_from_civil() {
        for days in (-800_000..800_000).step_by(97) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "day {days} -> {y}-{m}-{d}");
        }
    }

    #[test]
    fn epoch_secs_to_days_and_time_known_anchors() {
        assert_eq!(epoch_secs_to_days_and_time(0), (0, 0, 0, 0));
        // The pre-epoch cases are the whole point: `div_euclid`/`rem_euclid`
        // must round toward negative infinity, not truncate toward zero, or
        // -1s lands on 1970-01-01 00:00:04294967295 instead of one second
        // before midnight on 1969-12-31.
        assert_eq!(epoch_secs_to_days_and_time(-1), (-1, 23, 59, 59));
        assert_eq!(epoch_secs_to_days_and_time(-86_400), (-1, 0, 0, 0));
        // A positive mid-day instant: 2026-01-01 is day 20_454; 13:45:30 is
        // 49_530s into that day.
        assert_eq!(
            epoch_secs_to_days_and_time(20_454 * 86_400 + 49_530),
            (20_454, 13, 45, 30)
        );
    }

    #[test]
    fn weekday_known_anchors() {
        // 0=Mon..6=Sun
        assert_eq!(day_of_week(2024, 1, 1), 0); // Monday
        assert_eq!(day_of_week(2000, 1, 1), 5); // Saturday
        assert_eq!(day_of_week(2026, 6, 19), 4); // Friday
    }

    #[test]
    fn month_navigation_wraps() {
        assert_eq!(prev_month(2026, 1), (2025, 12));
        assert_eq!(prev_month(2026, 6), (2026, 5));
        assert_eq!(next_month(2026, 12), (2027, 1));
        assert_eq!(next_month(2026, 6), (2026, 7));
    }

    #[test]
    fn month_grid_shape() {
        // June 2026: the 1st is a Monday (day_of_week == 0).
        let grid = month_grid(2026, 6, 0); // week starts Monday
        assert_eq!(grid.len(), 42);
        // Monday-start grid with a Monday 1st → no leading padding.
        assert_eq!(grid[0], (2026, 6, 1));
        assert!(grid.contains(&(2026, 6, 30)));
        // Sunday-start grid for the same month: June 1 is a Monday, so the
        // Sunday-start week has exactly one leading day (Sun, May 31).
        let grid_sun = month_grid(2026, 6, 6);
        assert_eq!(grid_sun.len(), 42);
        assert_eq!(grid_sun[0], (2026, 5, 31));
        assert_eq!(grid_sun[1], (2026, 6, 1));
    }

    #[test]
    fn add_days_crosses_boundaries() {
        assert_eq!(add_days(2026, 1, 31, 1), (2026, 2, 1));
        assert_eq!(add_days(2026, 12, 31, 1), (2027, 1, 1));
        assert_eq!(add_days(2026, 3, 1, -1), (2026, 2, 28)); // non-leap
        assert_eq!(add_days(2024, 3, 1, -1), (2024, 2, 29)); // leap
        assert_eq!(add_days(2026, 6, 15, 0), (2026, 6, 15));
    }

    #[test]
    fn week_start_snaps_back() {
        // 2026-06-19 is a Friday (dow 4). Monday-start week → 2026-06-15.
        assert_eq!(week_start_date(2026, 6, 19, 0), (2026, 6, 15));
        // Sunday-start week (week_start 6) → 2026-06-14.
        assert_eq!(week_start_date(2026, 6, 19, 6), (2026, 6, 14));
    }

    #[test]
    fn locale_week_start_reads_the_region_not_the_language() {
        // The language says nothing: `en` is Monday-start in GB and IE,
        // Sunday-start in the US. linux's shipped probe keyed on the whole
        // `en_US` prefix and so answered Monday for every other Sunday-start
        // region on earth.
        assert_eq!(week_start_for_locale("en_US.UTF-8"), 6);
        assert_eq!(week_start_for_locale("en_GB.UTF-8"), 0);
        assert_eq!(week_start_for_locale("en_IE"), 0);
        assert_eq!(week_start_for_locale("es_MX.UTF-8"), 6);
        assert_eq!(week_start_for_locale("pt_BR"), 6);
        assert_eq!(week_start_for_locale("ja_JP.UTF-8"), 6);
        assert_eq!(week_start_for_locale("he_IL"), 6);
    }

    #[test]
    fn locale_week_start_covers_the_non_monday_non_sunday_regions() {
        // The half of the world a Monday/Sunday-only probe cannot express at
        // all. caltime's convention is 0 = Mon, so Fri = 4, Sat = 5, Sun = 6.
        assert_eq!(week_start_for_locale("ar_EG.UTF-8"), 5); // Saturday
        assert_eq!(week_start_for_locale("fa_IR"), 5);
        assert_eq!(week_start_for_locale("ar_SA"), 5);
        assert_eq!(week_start_for_locale("bn_BD"), 4); // Friday
        assert_eq!(week_start_for_locale("dv_MV"), 4);
    }

    #[test]
    fn locale_week_start_falls_back_to_monday_on_anything_unparseable() {
        // ISO-8601's default, and the most common answer worldwide — so an
        // unknown region degrades to the right guess rather than to en_US.
        assert_eq!(week_start_for_locale("C"), 0);
        assert_eq!(week_start_for_locale("POSIX"), 0);
        assert_eq!(week_start_for_locale(""), 0);
        assert_eq!(week_start_for_locale("nb"), 0); // language only, no region
        assert_eq!(week_start_for_locale("gibberish"), 0);
        assert_eq!(week_start_for_locale("xx_ZZ"), 0); // parseable, unknown region
    }

    #[test]
    fn locale_week_start_strips_encoding_and_modifier_suffixes() {
        // Real-world `LC_TIME` values carry both, and BCP-47 clients hand us
        // the hyphenated spelling.
        assert_eq!(week_start_for_locale("en_US.ISO-8859-1"), 6);
        assert_eq!(week_start_for_locale("ca_ES@valencia"), 0);
        assert_eq!(week_start_for_locale("ar_EG.UTF-8@islamic"), 5);
        assert_eq!(week_start_for_locale("en-US"), 6); // BCP-47 hyphen
        assert_eq!(week_start_for_locale("en_us"), 6); // lowercase region
    }

    #[test]
    fn every_locale_week_start_is_a_valid_caltime_week_start() {
        // A convention mismatch here silently shifts every grid by a day, so
        // pin the range the whole module's `week_start` params are defined on.
        for loc in [
            "en_US", "en_GB", "ar_EG", "bn_BD", "C", "", "xx_ZZ", "ja_JP", "fa_IR", "dv_MV",
        ] {
            let ws = week_start_for_locale(loc);
            assert!(ws <= 6, "{loc} produced out-of-range week_start {ws}");
            // And it must round-trip through the consumers unchanged.
            assert_eq!(
                visible_days(CalendarViewMode::Week, (2026, 6, 19), ws).len(),
                7
            );
            assert_eq!(
                visible_days(CalendarViewMode::Month, (2026, 6, 19), ws).len(),
                42
            );
        }
    }

    #[test]
    fn parse_time_iso_and_epoch() {
        assert_eq!(parse_time("2026-03-22T09:30:00"), Some((9, 30)));
        assert_eq!(parse_time("2026-03-22T00:00:00"), Some((0, 0)));
        assert_eq!(parse_time("2026-03-22"), None); // date-only
        assert_eq!(parse_time("not-a-time"), None);
        // 1970-01-01T01:01:00 UTC = 3660_000 ms.
        assert_eq!(parse_time("3660000"), Some((1, 1)));
    }

    #[test]
    fn parse_date_iso_and_epoch() {
        assert_eq!(parse_date("2026-03-22T09:30:00"), Some((2026, 3, 22)));
        assert_eq!(parse_date("2026-03-22"), Some((2026, 3, 22)));
        assert_eq!(parse_date("garbage"), None);
        // Epoch millis → UTC civil date. 0 ms = 1970-01-01.
        assert_eq!(parse_date("0"), Some((1970, 1, 1)));
        // 1700000000000 ms = 2023-11-14T22:13:20Z → date 2023-11-14.
        assert_eq!(parse_date("1700000000000"), Some((2023, 11, 14)));
    }

    #[test]
    fn all_day_detection() {
        assert!(is_all_day("2026-03-22T00:00:00", None)); // no end
        assert!(is_all_day("2026-03-22", Some("2026-03-23"))); // both date-only
        // Midnight-to-midnight spanning exactly one day.
        assert!(is_all_day(
            "2026-03-22T00:00:00",
            Some("2026-03-23T00:00:00")
        ));
        // A normal timed event is not all-day.
        assert!(!is_all_day(
            "2026-03-22T09:00:00",
            Some("2026-03-22T10:00:00")
        ));
        // Midnight start but not a full-day span.
        assert!(!is_all_day(
            "2026-03-22T00:00:00",
            Some("2026-03-22T00:30:00")
        ));
    }

    #[test]
    fn all_day_union_rule_widening() {
        // Multi-day midnight-to-midnight span (apple's `days >= 1` rule —
        // "Vacation Apr 1–4" from Thunderbird/Apple Calendar). The old
        // exactly-one-day rule classified this as timed.
        assert!(is_all_day(
            "2026-04-01T00:00:00",
            Some("2026-04-05T00:00:00")
        ));
        // A date-only DTSTART is all-day by type (RFC 5545 VALUE=DATE governs),
        // regardless of the end's shape (android's rule; lenient on odd ends).
        assert!(is_all_day("2026-04-01", Some("2026-04-02T10:00:00")));
        // A space-separated timed datetime (the linux form's legacy shape) is
        // NOT date-only — it carries a time component. The old `!contains('T')`
        // check misclassified this pair as all-day.
        assert!(!is_all_day("2026-03-22 10:00", Some("2026-03-22 11:00")));
    }

    #[test]
    fn day_column_layout_splits_and_clamps_like_the_windows_canonical() {
        // Canonical semantics = windows TimeGridLayout.ForDay: all-day/timed
        // split via is_all_day; timed start_min = HH*60+MM of the start; end_min
        // = minutes from the START-day midnight to the end, clamped to
        // [start_min + 30, 1440]; overlap packing via find_overlaps.
        let placements = day_column_layout(&[
            // 22:00 → 02:00 next day: renders 22:00 → midnight (end 1440), NOT
            // a 30-min block (the apple/web/linux drift) and NOT 1560 (android).
            ("2026-07-16T22:00:00", Some("2026-07-17T02:00:00")),
            // Plain one-hour event.
            ("2026-07-16T09:00:00", Some("2026-07-16T10:00:00")),
            // All-day chip.
            ("2026-07-16", None),
            // Zero-duration → 30-minute floor.
            ("2026-07-16T09:15:00", Some("2026-07-16T09:15:00")),
        ]);
        assert_eq!(placements.len(), 4);
        match placements[0] {
            EventPlacement::Timed {
                start_min, end_min, ..
            } => {
                assert_eq!(start_min, 22 * 60);
                assert_eq!(end_min, 1440);
            }
            _ => panic!("22:00→02:00 must be timed"),
        }
        match placements[1] {
            EventPlacement::Timed {
                start_min, end_min, ..
            } => {
                assert_eq!(start_min, 9 * 60);
                assert_eq!(end_min, 10 * 60);
            }
            _ => panic!("09:00→10:00 must be timed"),
        }
        assert!(matches!(placements[2], EventPlacement::AllDay));
        match placements[3] {
            EventPlacement::Timed {
                start_min, end_min, ..
            } => {
                assert_eq!(start_min, 9 * 60 + 15);
                assert_eq!(end_min, 9 * 60 + 45);
            }
            _ => panic!("zero-duration must be timed with the 30-min floor"),
        }
        // The two overlapping morning events (09:00–10:00 and 09:15–09:45) pack
        // into two columns; the 22:00 block stays a single column.
        let cols: Vec<(u32, u32)> = placements
            .iter()
            .filter_map(|p| match p {
                EventPlacement::Timed {
                    column_index,
                    total_columns,
                    ..
                } => Some((*column_index, *total_columns)),
                EventPlacement::AllDay => None,
            })
            .collect();
        assert_eq!(cols[0], (0, 1)); // 22:00 block alone
        assert_eq!(cols[1].1, 2); // morning pair packs to 2 columns
        assert_eq!(cols[2].1, 2);
        assert_ne!(cols[1].0, cols[2].0);
    }

    #[test]
    fn day_column_layout_degrades_unparseable_start_to_all_day() {
        // A start with an unparseable time component lands in the all-day band
        // (visible) instead of being silently dropped from the grid (linux's
        // old `continue` made such events vanish).
        let placements = day_column_layout(&[("garbage-datetime", Some("2026-07-16T10:00:00"))]);
        assert!(matches!(placements[0], EventPlacement::AllDay));
    }

    #[test]
    fn normalize_event_datetime_input_pads_seconds_and_unifies_the_separator() {
        // The A2-regression rule (apple EventDateInput.parse, lifted): a bare
        // `THH:MM` gains `:00` seconds so the nest's iCalendar serializer never
        // emits a malformed compact time.
        assert_eq!(
            normalize_event_datetime_input("2026-04-01T10:00"),
            "2026-04-01T10:00:00"
        );
        // Already carries seconds, or a timezone → unchanged.
        assert_eq!(
            normalize_event_datetime_input("2026-04-01T10:00:00"),
            "2026-04-01T10:00:00"
        );
        assert_eq!(
            normalize_event_datetime_input("2026-06-26T15:43:00Z"),
            "2026-06-26T15:43:00Z"
        );
        // Date-only stays date-only (all-day by type).
        assert_eq!(normalize_event_datetime_input("2026-04-01"), "2026-04-01");
        // The space-separated form (linux's legacy placeholder) unifies to 'T'
        // and gains seconds.
        assert_eq!(
            normalize_event_datetime_input("2026-04-01 10:00"),
            "2026-04-01T10:00:00"
        );
        // Whitespace trims; garbage passes through untouched (the nest-side
        // validation stays the authority on rejection).
        assert_eq!(
            normalize_event_datetime_input("  2026-04-01T10:00  "),
            "2026-04-01T10:00:00"
        );
        assert_eq!(normalize_event_datetime_input("garbage"), "garbage");
    }

    #[test]
    fn overlaps_disjoint_and_overlapping() {
        // Disjoint events → all column 0, group size 1.
        let disjoint = find_overlaps(&[(0, 60), (120, 180)]);
        assert_eq!(disjoint[0], (0, 0, 1));
        assert_eq!(disjoint[1], (1, 0, 1));

        // Two overlapping events → two columns, group total 2.
        let overlap = find_overlaps(&[(0, 120), (60, 180)]);
        assert_eq!(overlap[0].2, 2);
        assert_eq!(overlap[1].2, 2);
        assert_ne!(overlap[0].1, overlap[1].1); // distinct columns

        assert!(find_overlaps(&[]).is_empty());
    }

    /// Pin for row 217: the
    /// device's local UTC offset must be derived in exactly one place in the
    /// tree — [`local_utc_offset_seconds`] — so a future native app or a
    /// shared-Rust lift cannot silently reopen the two-independent-
    /// derivations shape that let tui's and linux's own doors disagree (one
    /// chrono, one glib) on a value two nest-facing fields
    /// (`fauna.family.usage_report`, `fauna.family.notify_report`) must
    /// agree on. Same census needle the finding used to find all three call
    /// sites, walked in-process (no `rg` on PATH required at test time).
    /// Feature-independent on purpose: the census is a text scan of source,
    /// not a compiled check, so it still catches a reintroduced hand-roll
    /// even in a `cargo test -p fauna-core` run with `local-clock` off.
    #[test]
    fn the_devices_utc_offset_is_derived_in_exactly_one_place() {
        // ⚠ The needle text must never appear CONTIGUOUSLY in this file, or the
        // census finds its own array and `at_home` passes whether or not the
        // real derivation still exists (one layer over — a source-text
        // predicate satisfied by the predicate's own text). `concat!` resolves
        // at compile time, so the string actually matched is identical while
        // the source line carries only the halves.
        //
        // This is deliberately how the vacuity is closed, rather than by
        // stopping the walk at `mod tests {`: that break
        // bought `at_home` at the cost of `elsewhere`, and `elsewhere` is the
        // assert that enforces the rule. It removed ~463k lines from the
        // census — including 190 production items across 9 files, headed by
        // `fauna-core/src/format.rs` (132 items after its line-456 test
        // module; the shared value-formatting module implementing the very
        // goal doc § this rule lives in) and `fauna-wasm/src/lib.rs` (web's
        // own boundary). Splitting the literals costs nothing and keeps the
        // whole tree in reach.
        const NEEDLES: [&str; 3] = [
            concat!("local_", "minus_utc"),
            concat!("Local::now()", ".offset()"),
            concat!("utc_offset()", ".as_seconds"),
        ];
        // Build output and vendored/third-party trees the census must never
        // attribute a *dependency's own* matching source to this crate.
        // `.flatpak-builder` is flatpak-builder's own gitignored build cache
        // (`just linux-flatpak`, `tests/platform/linux/test_installer.py`,
        // `tests/real_session/test_sync_agent_flatpak_seam.py`): it vendors a
        // full cargo build tree under the repo root, and any local checkout
        // that has ever run a local flatpak build carries chrono's own
        // `local_minus_utc` field name inside it — a false "found elsewhere"
        // for every run after.
        const SKIP: [&str; 5] = [
            "target",
            ".git",
            "node_modules",
            "vendor",
            ".flatpak-builder",
        ];
        const ALLOWED: &str = "libs/fauna-core/src/caltime.rs";

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("fauna-core sits two directories under the repo root")
            .to_path_buf();

        let mut hits: Vec<(String, usize, String)> = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if SKIP
                    .iter()
                    .any(|s| rel == *s || rel.starts_with(&format!("{s}/")))
                {
                    continue;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (i, line) in src.lines().enumerate() {
                    // Every line of every file, deliberately: the rule binds
                    // the whole tree, so the census must reach all of it. The
                    // pin's own literals are kept out of range by splitting
                    // them (see `NEEDLES`), not by narrowing the walk — the
                    // "test modules run to the end of their file" convention
                    // that would justify a `mod tests {` break is measurably
                    // false here: 9 files carry production items
                    // after their first test module.
                    if NEEDLES.iter().any(|n| line.contains(n)) {
                        hits.push((rel.clone(), i + 1, line.trim().to_string()));
                    }
                }
            }
        }

        let (at_home, elsewhere): (Vec<_>, Vec<_>) =
            hits.into_iter().partition(|(path, _, _)| path == ALLOWED);

        assert!(
            !at_home.is_empty(),
            "expected the derivation inside {ALLOWED} — did it move? Update \
             ALLOWED in this test to its new home."
        );
        assert!(
            elsewhere.is_empty(),
            "found a device-UTC-offset derivation outside {ALLOWED} — route \
             it through fauna_core::caltime::local_utc_offset_seconds \
             instead:\n{}",
            elsewhere
                .iter()
                .map(|(p, l, s)| format!("  {p}:{l}: {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
