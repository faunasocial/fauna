import Foundation

/// The device's UTC offset — the ONE place apple reads it
/// (`value-formatting.md` § Absolute local timestamp display, the app-side
/// one-door rule). Shared Rust is WASM-safe and owns no clock, so every app
/// supplies `utc_offset_seconds` / offset-minutes itself — and supplies it
/// from one door, because the value is not display-only: it rides
/// `fauna.family.usage_report` / `notify_report`, where the nest persists it
/// as the ward-local day bucket, so a second in-app derivation is drift
/// waiting to disagree with the first. `TimeZone` is the ratified apple
/// sourcing API (`value-formatting.md:163`); do not reintroduce a second
/// `TimeZone.current.secondsFromGMT()` read at any call site. Mirrors android
/// `DeviceOffset.kt`.
public enum DeviceOffset {
    /// Seconds east of UTC — the unit the shared formatters take.
    public static func utcOffsetSeconds() -> Int32 {
        Int32(TimeZone.current.secondsFromGMT())
    }

    /// Minutes east of UTC — the unit the family day-bucket rule wants
    /// (`family-safety.md` § Screen time).
    public static func utcOffsetMinutes() -> Int32 {
        utcOffsetSeconds() / 60
    }
}
