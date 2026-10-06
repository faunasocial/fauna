// Deno test for web's locale week start.
// Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/weekStart.test.ts
//
// `ui/events.md` § Week & day timeline views ("week-start follows the app
// locale") + § Where logic lives, whose 2026-08-01 amendment makes the probe
// per-platform and keeps web off the native-only Rust one.
//
// The conversion is the whole risk surface: `Intl` says `1 = Mon … 7 = Sun`,
// caltime says `0 = Mon … 6 = Sun`, and `Date.getDay()` says `0 = Sun` — three
// origins for one fact. § Where logic lives warns an off-by-one between them
// "is invisible in review and shifts every grid by a day".
//
// `localeWeekStart()` itself is deliberately not asserted: it reads the ambient
// browser locale, which a test cannot pin without stubbing the engine. What it
// delegates to IS pinned, which is why the conversion lives in its own function.

import { WEEK_START_MONDAY, weekStartFromIntlFirstDay, weekdayCells } from './weekStart.ts';

function assertEq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}: expected ${e}, got ${a}`);
}

Deno.test('Intl firstDay converts to a caltime weekday', () => {
  assertEq(weekStartFromIntlFirstDay(1), 0, 'Monday');
  assertEq(weekStartFromIntlFirstDay(6), 5, 'Saturday');
  // Sunday is 7 -> 6, NOT 0: the identity is `firstDay - 1`. Wrapping Sunday
  // to 0 would silently render a Monday grid in every Sunday-start locale —
  // precisely the invisible off-by-one this file exists to prevent.
  assertEq(weekStartFromIntlFirstDay(7), 6, 'Sunday');
});

Deno.test('an out-of-range or non-integer firstDay falls back to Monday', () => {
  for (const bad of [0, 8, -1, 1.5, NaN]) {
    assertEq(weekStartFromIntlFirstDay(bad), WEEK_START_MONDAY, `${bad}`);
  }
});

Deno.test('weekday cells rotate to the grid they label', () => {
  // Asserted as real weekdays (`getDay()`, 0=Sun) rather than indices, so a
  // mistake in the origin cannot cancel itself out.
  const dayOf = (ws: number) => weekdayCells(ws, (d) => d.getDay());
  assertEq(dayOf(0), [1, 2, 3, 4, 5, 6, 0], 'Monday-start: Mon..Sun');
  assertEq(dayOf(6), [0, 1, 2, 3, 4, 5, 6], 'Sunday-start: Sun..Sat');
  assertEq(dayOf(5), [6, 0, 1, 2, 3, 4, 5], 'Saturday-start: Sat..Fri');
});

Deno.test('the first labelled cell is the day the grid actually starts on', () => {
  // The agreement itself, asserted directly rather than inferred from the two
  // properties above: this is the case that dies if either half is reverted to
  // Monday while the other follows the locale — the linux bug, which a
  // header-only assertion passed straight through.
  for (let ws = 0; ws < 7; ws++) {
    const [first] = weekdayCells(ws, (d) => d.getDay());
    assertEq(first, (ws + 1) % 7, `week start ${ws}`);
  }
});
