import Foundation

// Compiled out of release artifacts (testing.md convention 15), like every
// other file in this directory.
#if DEBUG

/// Argument parsing shared by the TestAgent fake-clock commands.
///
/// Six handlers (`DelegationClock`, `TrustFacetClock`, `MailTokenRefreshClock`,
/// `BackupAudit`, `OfflineShare`, `CustodianPull`) each hand-copied the same
/// three-line read of `now_offset_secs`; one parser keeps their reading of the
/// wire argument identical.
enum TestCommandArgs {
    /// `now_offset_secs` as the fake-clock offset. JSON numbers arrive as
    /// `NSNumber`; take the widest read, then a bare `Int`. A missing or
    /// unparseable value is `0` — the documented reset, and the same default
    /// tui's arm uses.
    static func nowOffsetSecs(_ command: [String: Any]) -> Int64 {
        (command["now_offset_secs"] as? NSNumber)?.int64Value
            ?? (command["now_offset_secs"] as? Int).map(Int64.init)
            ?? 0
    }
}

#endif
