import Combine
import Foundation

/// A fresh once-per-second Combine ticker (mac/iOS, priority #2 — was
/// duplicated verbatim as each provisioning view's own private `elapsedTimer`
/// before this lift). Each call returns a new `autoconnect()`'d publisher —
/// callers `.onReceive` it directly, matching the original per-view shape.
public func everySecondTicker() -> Publishers.Autoconnect<Timer.TimerPublisher> {
    Timer.publish(every: 1.0, on: .main, in: .common).autoconnect()
}

public extension Date {
    /// Relative-time display for a `Date`, routed through the shared
    /// `fauna_core::format` thresholds (uniform across all 7 apps) rather
    /// than the native `RelativeDateTimeFormatter` (whose bucketing diverged).
    /// Thin adapter to `ValueFormat.relativeTime` — the threshold decision lives
    /// in shared Rust, per `docs/goal/behavior/value-formatting.md`.
    var relativeFormatted: String {
        ValueFormat.relativeTime(self)
    }

    /// Construct from a whole-second Unix epoch — nest snapshot `createdAt`
    /// fields are serialized as integer seconds, unlike the millisecond
    /// `Int64` most other FFI timestamps use.
    init(epochSeconds: Int) {
        self.init(timeIntervalSince1970: Double(epochSeconds))
    }

    /// Same, from the `Int64` the shared page machines carry. Every UniFFI record
    /// spells a whole-second epoch `i64` (`SnapshotRow.createdAt`,
    /// `BackupsSnapshot.lastBackedUp`, the `SnapshotState` deadlines, …), so
    /// without this overload every machine-adopting view sprinkles `Int(…)` at
    /// each call site — a conversion that is noise on 64-bit and easy to get
    /// subtly wrong. Added with the Backups machine adoption; use it for any
    /// further machine timestamps.
    init(epochSeconds: Int64) {
        self.init(timeIntervalSince1970: Double(epochSeconds))
    }

    /// Current wall-clock time as Unix epoch milliseconds — the polling unit
    /// the onboarding provisioning views' elapsed timer re-reads on every tick.
    static var nowEpochMillis: UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000)
    }
}
