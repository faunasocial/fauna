/** The locale week start, web's own probe — `ui/events.md` § Week & day timeline
 *  views ("week-start follows the app locale") and § Where logic lives, whose
 *  2026-08-01 amendment makes the probe **per-platform**.
 *
 *  **Web is the one app that must NOT consume the shared Rust probe.**
 *  `caltime::locale_week_start` is native-only by design, and the browser
 *  already carries the full CLDR `weekData` that the Rust table deliberately
 *  only approximates for the two Rust apps. Routing this through wasm would
 *  hand web a worse answer than the one under its feet.
 *
 *  Kept in its own module, free of the wasm import chain, so the conversion
 *  below is unit-testable without building wasm. */

/** caltime's convention: `0 = Mon … 6 = Sun`. */
export const WEEK_START_MONDAY = 0;

/** Convert `Intl`'s `firstDay` (`1 = Mon … 7 = Sun`) to caltime's `0 = Mon`.
 *
 *  This is the seam § Where logic lives warns about — every platform uses a
 *  different origin (.NET `0 = Sun`, Swift `1 = Sun`, `Date.getDay()` `0 = Sun`,
 *  caltime `0 = Mon`) and an off-by-one here is invisible in review while
 *  shifting every grid by a day. One conversion, one place, pinned by test.
 *
 *  `7` (Sunday) maps to `6`, not `0`: the identity is `firstDay - 1`. */
export function weekStartFromIntlFirstDay(firstDay: number): number {
  if (!Number.isInteger(firstDay) || firstDay < 1 || firstDay > 7) return WEEK_START_MONDAY;
  return firstDay - 1;
}

/** The browser's locale week start in caltime's convention.
 *
 *  Falls back to Monday when the engine has no `getWeekInfo` (it is newer than
 *  `Intl.Locale` itself) or when the locale string will not parse — the same
 *  default the shared Rust classifier takes for an unlisted region, so the two
 *  disagree only where the browser genuinely knows better. */
export function localeWeekStart(): number {
  try {
    const locale = new Intl.Locale(
      typeof navigator !== 'undefined' && navigator.language ? navigator.language : 'en-US',
    );
    // `getWeekInfo` is a method on newer engines and an accessor on some older
    // ones; both shapes answer here, and anything else falls through.
    const info = (
      locale as unknown as { getWeekInfo?: () => { firstDay?: number }; weekInfo?: { firstDay?: number } }
    );
    const firstDay = info.getWeekInfo?.().firstDay ?? info.weekInfo?.firstDay;
    return typeof firstDay === 'number' ? weekStartFromIntlFirstDay(firstDay) : WEEK_START_MONDAY;
  } catch {
    return WEEK_START_MONDAY;
  }
}

/** The seven weekday cells in column order for a grid starting on `weekStart`
 *  (caltime `0 = Mon`), rendered by `render` from a Date.
 *
 *  Takes `weekStart` rather than probing, deliberately: linux shipped a range
 *  label that probed independently of its own grid and could name a different
 *  week than the columns beneath it. One value in, no disagreement possible.
 *
 *  2024-01-01 is a Monday, so it is the natural origin for a `0 = Mon` offset. */
export function weekdayCells<T>(weekStart: number, render: (d: Date) => T): T[] {
  return Array.from({ length: 7 }, (_, i) => render(new Date(2024, 0, 1 + weekStart + i)));
}
