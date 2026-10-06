import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it — the runtime
// `FAUNA_E2E_*` gates are the inner switch WITHIN a test-capable build, never
// the boundary. Also required for correctness: this reads
// `AtprotoSettingsVM.liveInstanceForTest`, itself `#if DEBUG`.
#if DEBUG

/// Shared handler for the cross-app `atproto_delegation_advance_clock` TestAgent
/// command — the D10 delegation row's lapse journey
/// (`tests/e2e-unified/tests/test_atproto_settings.py::
/// test_lapse_reads_as_reauthorize_here`).
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation (and it
/// is covered by `swift-test`, unlike the app targets), mirroring
/// `ConversationsTestInject`. Drives the SAME shared Rust seam tui's
/// `automation.rs` arm does — `fauna_atproto_settings_machine::
/// set_delegation_clock_offset_secs`, reached here through the `test-helpers`
/// UniFFI free function `setDelegationClockOffsetSecs` — so every app moves the
/// render clock through one code path.
///
/// `expiring_soon`/`expired` sit ~76 and ~90 days into the grant window, so a
/// lapse is unreachable by waiting and must never be chased with a sleep
/// (`testing.md` convention 14 — a fake clock, never a sleep). The offset moves
/// the row's **render** clock only, never the mint clock: authorizing always
/// stamps a freshly minted cert with the real wall clock.
///
/// ⚠ The offset is **process-wide and nothing auto-resets it** — the test resets
/// it to `0` in a `finally` before re-authorizing, because a stale offset would
/// silently re-lapse the next cert the process mints.
public enum DelegationClockTestCommand {
    /// Apply the command. Returns `nil` on success, or a human-readable reason
    /// the caller must surface as a **loud** TestAgent failure — never a silent
    /// no-op (`testing.md` convention 11: honour the command or refuse audibly;
    /// a dropped command reads downstream as a real product bug).
    ///
    /// The refresh is awaited inline for the same reason tui awaits its own: the
    /// recomputed liveness must already be painted when this command acks, or
    /// the driver's next `atproto-delegation-status` read races the old frame.
    @MainActor
    public static func apply(_ command: [String: Any]) async -> String? {
        // JSON numbers arrive as `NSNumber`; take the widest read, matching the
        // `ConversationsTestInject.labels` parser. A missing/unparseable value
        // is `0` — the documented reset, and the same default tui's arm uses.
        let offset = (command["now_offset_secs"] as? NSNumber)?.int64Value
            ?? (command["now_offset_secs"] as? Int).map(Int64.init)
            ?? 0
        setDelegationClockOffsetSecs(offsetSecs: offset)

        guard let vm = AtprotoSettingsVM.liveInstanceForTest else {
            return "atproto_delegation_advance_clock: the AT Protocol page is not open "
                + "(no live AtprotoSettingsVM), so the delegation row cannot be repainted"
        }
        guard vm.machine != nil else {
            return "atproto_delegation_advance_clock: no authenticated session"
        }
        await vm.refresh()
        return nil
    }
}

#endif
