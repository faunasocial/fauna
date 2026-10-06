// The device's UTC offset — the ONE place web reads it (value-formatting.md
// § Absolute local timestamp display, the app-side one-door rule). Shared Rust
// is WASM-safe and owns no clock, so every app supplies `utc_offset_seconds` /
// offset-minutes itself — and supplies it from one door, because the value is
// not display-only: it rides `fauna.family.usage_report` / `notify_report`,
// where the nest persists it as the ward-local day bucket. Two derivations
// that can disagree would let the day the nest stores drift from the day the
// app renders against.
//
// `getTimezoneOffset()` is inverted from the convention — it returns minutes
// to ADD to local to reach UTC — so the sign flip here is load-bearing:
// getting it backwards would bucket a ward's day twice as far from their real
// midnight as no offset at all.

/** Minutes east of UTC — the unit the family day-bucket rule wants
 *  (`family-safety.md` § Screen time). */
export function utcOffsetMinutes(): number {
  return -new Date().getTimezoneOffset();
}

/** Seconds east of UTC — the unit the shared formatters take. */
export function utcOffsetSeconds(): number {
  return utcOffsetMinutes() * 60;
}
