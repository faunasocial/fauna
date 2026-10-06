// JS-`Date` adapters over the shared `fauna_core::caltime` wasm boundary.
//
// The web Events page's month-grid + week-range layout rides the canonical
// Gregorian date math the native apps consume (linux's
// `views/events/time_utils.rs` re-exports the same `fauna_core::caltime` module)
// instead of re-deriving it in JS `Date` arithmetic — priority #2/#4;
// `docs/goal/ui/events.md` § Where logic lives names caltime the home and "web's
// events read-path is the first expected consumer".
//
// caltime uses **1-indexed months** (1 = January) and **0 = Monday … 6 = Sunday**
// weekdays; these helpers convert to/from JS `Date`'s 0-indexed months at the
// seam so the calendar components keep working in `Date` terms. Localized
// month/weekday *names* and the system clock stay per-app (events.md), so they
// are NOT lifted here. Sync: callers run after `ensureWasm()` (the Events page
// gates the calendar UI on `ready`, set only after `await ensureWasm()`).

import {
  monthGrid as wasmMonthGrid,
  prevMonth as wasmPrevMonth,
  nextMonth as wasmNextMonth,
  weekStartDate as wasmWeekStartDate,
  addDays as wasmAddDays,
  findOverlaps as wasmFindOverlaps,
  dayColumnLayout as wasmDayColumnLayout,
  pan as wasmPan,
  visibleDays as wasmVisibleDays,
  type OverlapColumn,
  type DayEventPlacement,
  type CalendarViewMode,
} from './wasm';

export type { OverlapColumn, DayEventPlacement, CalendarViewMode };

// The week start is the LOCALE's, probed once in `$lib/weekStart` (web must
// not consume the native-only Rust probe — see that module). Re-exported here
// so callers reach one name; `WEEK_START_MONDAY` survives as the explicit
// Monday constant and as the fallback, not as anybody's default.
export { WEEK_START_MONDAY, localeWeekStart, weekdayCells } from './weekStart';
import { localeWeekStart } from './weekStart';

/** One month-grid cell: the JS `Date` plus whether it falls in the queried month
 *  (`false` for the prev/next-month padding days). */
export interface MonthCell {
  date: Date;
  inMonth: boolean;
}

/** The 42-cell (6×7) month grid for the **0-indexed** (`Date`-style) `(year,
 *  month0)`, padded with prev/next-month days. `weekStart` is caltime's
 *  `0 = Mon … 6 = Sun` (defaults to Monday, the web calendar's start). */
export function monthGrid(year: number, month0: number, weekStart = localeWeekStart()): MonthCell[] {
  return wasmMonthGrid(year, month0 + 1, weekStart).map((c) => ({
    date: new Date(c.year, c.month - 1, c.day),
    inMonth: c.year === year && c.month === month0 + 1,
  }));
}

/** The month before the **0-indexed** `(year, month0)`, year-wrapped at January. */
export function prevMonth(year: number, month0: number): { year: number; month0: number } {
  const r = wasmPrevMonth(year, month0 + 1);
  return { year: r.year, month0: r.month - 1 };
}

/** The month after the **0-indexed** `(year, month0)`, year-wrapped at December. */
export function nextMonth(year: number, month0: number): { year: number; month0: number } {
  const r = wasmNextMonth(year, month0 + 1);
  return { year: r.year, month0: r.month - 1 };
}

/** The `Date` of the `weekStart` day (`0 = Mon … 6 = Sun`, default Monday) of the
 *  week containing `date`. */
export function weekStart(date: Date, ws = localeWeekStart()): Date {
  const r = wasmWeekStartDate(date.getFullYear(), date.getMonth() + 1, date.getDate(), ws);
  return new Date(r.year, r.month - 1, r.day);
}

/** The `Date` `n` days from `date` (`n` may be negative), crossing month/year
 *  boundaries via shared Gregorian arithmetic. */
export function addDays(date: Date, n: number): Date {
  const r = wasmAddDays(date.getFullYear(), date.getMonth() + 1, date.getDate(), n);
  return new Date(r.year, r.month - 1, r.day);
}

/** Side-by-side column packing for overlapping timed events, over the shared
 *  `fauna_core::caltime::find_overlaps` (union-find clustering) — the one
 *  overlap-layout primitive web shares with the native apps so the week/day
 *  grid lays blocks out identically (events.md § Where logic lives). `intervals`
 *  are `[startMinutes, endMinutes]` pairs (minutes from midnight); the i-th
 *  result corresponds to the i-th interval. */
export function findOverlaps(intervals: [number, number][]): OverlapColumn[] {
  if (intervals.length === 0) return [];
  return wasmFindOverlaps(intervals);
}

/** Complete all-day/timed layout for one day column — `fauna_core::caltime::
 *  day_column_layout` over wasm: classification, timed minute geometry (an
 *  event crossing midnight renders start → midnight, clamped to 1440 — never
 *  a collapsed 30-minute block), and `findOverlaps` column packing, in one
 *  call, so identical events lay out identically to the native apps
 *  (events.md § Where logic lives). `events` are `[dtstart, dtend]` ISO pairs;
 *  the date filter (which events fall on this day) stays client-side. The
 *  i-th result corresponds to the i-th input event. */
export function dayColumnLayout(events: [string, string | null][]): DayEventPlacement[] {
  if (events.length === 0) return [];
  return wasmDayColumnLayout(events);
}

/** Which way an `events-prev-month` / `events-next-month` click moves the anchor. */
export type PanDirection = 'backward' | 'forward';

/** Apply one pan click to `anchor` — `fauna_core::caltime::pan` over wasm, the
 *  shared "one visible range per click" policy: a month in month view, a week in
 *  week, a day in day, and **nothing** in the date-unfiltered agenda
 *  (events.md § User actions). The agenda arm returning the anchor unchanged is
 *  what lets the page wire the buttons unconditionally and let the shared policy
 *  decide, the same way linux and tui do — no per-app `if agenda` branch. */
export function pan(mode: CalendarViewMode, anchor: Date, direction: PanDirection): Date {
  const r = wasmPan(
    mode,
    anchor.getFullYear(),
    anchor.getMonth() + 1,
    anchor.getDate(),
    direction === 'forward',
  );
  return new Date(r.year, r.month - 1, r.day);
}

/** The dates `mode` shows for `anchor` — `fauna_core::caltime::visible_days` over
 *  wasm: the month grid's 42 cells, the week's 7 day-columns, the single day, or
 *  `[]` for the date-unfiltered agenda. `weekStart` is caltime's `0 = Mon … 6 =
 *  Sun` (defaults to Monday, the web calendar's start).
 *
 *  The month *grid* keeps using `monthGrid` above, which also carries each cell's
 *  `inMonth` padding flag; this is the mode-generic range the week columns want. */
export function visibleDays(
  mode: CalendarViewMode,
  anchor: Date,
  weekStart = localeWeekStart(),
): Date[] {
  return wasmVisibleDays(
    mode,
    anchor.getFullYear(),
    anchor.getMonth() + 1,
    anchor.getDate(),
    weekStart,
  ).map((d) => new Date(d.year, d.month - 1, d.day));
}
