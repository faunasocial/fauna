import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it — the runtime
// `FAUNA_E2E_*` gates are the inner switch WITHIN a test-capable build, never
// the boundary. Also required for correctness: this reads
// `LinkedNestsVM.liveInstanceForTest`, itself `#if DEBUG`.
#if DEBUG

/// Shared handler for the cross-app `trust_facet_advance_clock` TestAgent
/// command — the Nests trust facet's RENDER clock (`nests.md` § Implementation
/// status today; `testing.md` convention 14 — a fake clock, never a sleep).
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation (and it
/// is covered by `swift-test`, unlike the app targets), mirroring
/// `BackupAuditTestCommand`. Drives the same shared Rust seam tui / linux /
/// android reach — `fauna_client_capabilities::trust_clock::set_clock_offset_secs`,
/// through the `test-helpers` UniFFI free function `setTrustClockOffsetSecs` —
/// which moves grant liveness, the auto-renew due decision and custody receipt
/// freshness, never the mint clock.
///
/// ⚠ The offset is **process-wide and nothing auto-resets it** — the test
/// resets it to `0` once its lapse assertions are done.
public enum TrustFacetClockTestCommand {
    /// Apply the command. Returns `nil` on success, or a human-readable reason
    /// the caller must surface as a **loud** TestAgent failure (convention 11).
    ///
    /// When the Nests page is open its VM is re-folded inline, so the moved
    /// clock is already painted when this command acks; with no page open there
    /// is nothing to repaint (the page folds against the clock on its next
    /// hydrate), which is not a refusal.
    @MainActor
    public static func apply(_ command: [String: Any]) async -> String? {
        let offset = (command["now_offset_secs"] as? NSNumber)?.int64Value
            ?? (command["now_offset_secs"] as? Int).map(Int64.init)
            ?? 0
        setTrustClockOffsetSecs(offsetSecs: offset)
        if let vm = LinkedNestsVM.liveInstanceForTest, vm.machine != nil {
            await vm.hydrate()
        }
        return nil
    }
}

#endif
