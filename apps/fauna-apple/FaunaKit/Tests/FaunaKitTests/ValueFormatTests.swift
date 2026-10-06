import Testing
import Foundation
@testable import FaunaKit

/// Cross-language conformance for `ValueFormat` + `shortId`: the apple apps
/// must render byte sizes / durations / relative time / short ids identically to
/// shared Rust (`fauna_core::format`, per `docs/goal/behavior/value-formatting.md`).
/// These call the REAL UniFFI exports (the native `fauna_ffi` lib loads via the
/// `FaunaFFI.xcframework`, like windows' `ValueFormatTests` loading the native dll)
/// and resolve the returned `LocalizedText` through the same `renderLocalizedText`
/// → `L.lookup` pipeline production uses. Locks apple to the shared thresholds +
/// the dropped-trailing-`.0` rounding (priority #1/#2/#4).

// MARK: - Byte sizes (largest 1024-unit ≥ 1, ≤ one decimal, trailing .0 dropped)

@Test func byteSizeMatchesSharedRustFormatter() {
    #expect(ValueFormat.byteSize(0) == "0 B")
    #expect(ValueFormat.byteSize(512) == "512 B")
    #expect(ValueFormat.byteSize(1023) == "1023 B")
    #expect(ValueFormat.byteSize(1024) == "1 KB")           // trailing .0 dropped (not "1.0 KB")
    #expect(ValueFormat.byteSize(1536) == "1.5 KB")
    #expect(ValueFormat.byteSize(5 * 1024 * 1024) == "5 MB")
    #expect(ValueFormat.byteSize(3 * 1024 * 1024 * 1024) == "3 GB")
    #expect(ValueFormat.byteSize(1024 * 1024 * 1024 * 1024) == "1 TB")
}

// MARK: - Duration / uptime (coarse d/h/m, full chain, seconds dropped, by-name multi-arg)

@Test func durationSecsMatchesSharedRustFormatter() {
    #expect(ValueFormat.durationSecs(0) == "0m")            // sub-minute → "0m"
    #expect(ValueFormat.durationSecs(59) == "0m")
    #expect(ValueFormat.durationSecs(60) == "1m")
    #expect(ValueFormat.durationSecs(3600) == "1h 0m")      // exactly 1h → uptime_hm, full chain
    #expect(ValueFormat.durationSecs(3661) == "1h 1m")      // seconds dropped
    #expect(ValueFormat.durationSecs(86400) == "1d 0h 0m")  // exactly 1d → uptime_dhm, full chain
    #expect(ValueFormat.durationSecs(90061) == "1d 1h 1m")  // multi-arg: days+hours+mins all present, by-name
}

// MARK: - Relative time (four recent buckets resolve; ≥ 7 d → native absolute date)

@Test func relativeTimeMatchesSharedRustFormatter() {
    let now: Int64 = 1_700_000_000_000

    #expect(ValueFormat.relativeTime(nowMs: now, thenMs: now - 30_000) == "just now")          // < 60 s
    #expect(ValueFormat.relativeTime(nowMs: now, thenMs: now - 5 * 60_000) == "5m ago")        // < 60 min
    #expect(ValueFormat.relativeTime(nowMs: now, thenMs: now - 3 * 3_600_000) == "3h ago")     // < 24 h
    #expect(ValueFormat.relativeTime(nowMs: now, thenMs: now - 2 * 86_400_000) == "2d ago")    // < 7 d

    // ≥ 7 d → native locale-aware absolute date (localized == nil path).
    let then = now - 8 * 86_400_000
    let expected = DateFormatter.localizedString(
        from: Date(timeIntervalSince1970: Double(then) / 1000.0),
        dateStyle: .short, timeStyle: .none)
    #expect(ValueFormat.relativeTime(nowMs: now, thenMs: then) == expected)
}

// MARK: - Conversation timestamp (today 24 h clock / Yesterday / weekday / older native date)

@Test func conversationTimestampMatchesSharedRustFormatter() {
    // Anchor `now` and derive each bucket against the *local* calendar day so the
    // assertions are timezone-stable (the wrapper buckets in `TimeZone.current`).
    let now: Int64 = 1_700_000_000_000
    let offsetMs = Int64(TimeZone.current.secondsFromGMT()) * 1000
    let dayMs: Int64 = 86_400_000
    let nowLocalDay = (now + offsetMs) / dayMs
    // Local noon of a target local-day, expressed back in UTC millis.
    func localNoon(daysAgo: Int64) -> Int64 {
        (nowLocalDay - daysAgo) * dayMs + 12 * 3_600_000 - offsetMs
    }

    // Today → the shared 24 h "HH:MM" clock (NOT a locale-aware 12 h reconstruction).
    let todayLocal = now + offsetMs
    let tod = ((todayLocal % dayMs) + dayMs) % dayMs
    let expectedClock = String(format: "%02d:%02d", tod / 3_600_000, (tod % 3_600_000) / 60_000)
    #expect(ValueFormat.conversationTimestamp(nowMs: now, thenMs: now) == expectedClock)

    // Previous local day → i18n "Yesterday" (resolved through the apple pipeline).
    #expect(ValueFormat.conversationTimestamp(nowMs: now, thenMs: localNoon(daysAgo: 1)) == "Yesterday")

    // 3 local days ago → a weekday name (the localized arm resolves to a non-empty,
    // non-clock, non-"Yesterday" label; exact name is locale/zone-dependent).
    let weekday = ValueFormat.conversationTimestamp(nowMs: now, thenMs: localNoon(daysAgo: 3))
    #expect(!weekday.isEmpty)
    #expect(weekday != "Yesterday")
    #expect(!weekday.contains(":"))

    // ≥ 7 local days ago → native locale-aware absolute date (date-only, .medium).
    let older = localNoon(daysAgo: 8)
    let expectedDate = DateFormatter.localizedString(
        from: Date(timeIntervalSince1970: Double(older) / 1000.0),
        dateStyle: .medium, timeStyle: .none)
    #expect(ValueFormat.conversationTimestamp(nowMs: now, thenMs: older) == expectedDate)

    // Non-positive sentinel (no activity) → "".
    #expect(ValueFormat.conversationTimestamp(nowMs: now, thenMs: 0) == "")
}

// MARK: - Absolute date (mail credential/alias/list/spam timestamps; .medium, optional .short time)

@Test func absoluteDateMatchesNativeFormatter() {
    let epochMs: Int64 = 1_700_000_000_000
    let date = Date(timeIntervalSince1970: Double(epochMs) / 1000.0)

    // Date-only (.medium / no time) — alias last-hit, list last-send, list-member
    // subscribed, credential created. Compared against the same native formatter
    // both sides use, so the assertion is locale/timezone-stable.
    #expect(ValueFormat.absoluteDate(epochMs: epochMs)
        == DateFormatter.localizedString(from: date, dateStyle: .medium, timeStyle: .none))

    // With time (.short) — spam events are time-of-day relevant.
    #expect(ValueFormat.absoluteDate(epochMs: epochMs, withTime: true)
        == DateFormatter.localizedString(from: date, dateStyle: .medium, timeStyle: .short))

    // Seconds-based sources (credential `createdAt`) multiply by 1000 at the call
    // site, landing on the same instant as the equivalent millis.
    let epochSecs: UInt64 = 1_700_000_000
    #expect(ValueFormat.absoluteDate(epochMs: Int64(epochSecs) * 1000)
        == ValueFormat.absoluteDate(epochMs: epochMs))
}

// MARK: - Short id (first 12 hex chars + U+2026; ≤ 12 chars unchanged)

@Test func shortIdMatchesSharedRustFormatter() {
    let hex64 = String(repeating: "a", count: 64)
    #expect(shortId(hex: hex64) == "aaaaaaaaaaaa\u{2026}")     // first 12 + single-char ellipsis
    #expect(shortId(hex: "abcdef012345") == "abcdef012345")    // exactly 12 → unchanged
    #expect(shortId(hex: "abc") == "abc")                      // < 12 → unchanged
    #expect(shortId(hex: "") == "")
}
