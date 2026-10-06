import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `feed_seed_cue_rollup_for_test` — seed the live engagement-cue engine with
/// `content_ids` (each recorded a `WatchComplete` verdict) and PUT the sealed
/// rollup to the nest for real, over the same
/// `FeedManager::set_cue_rollup_for_test` shared-Rust seam tui and linux drive
/// (`FfiFeedManager.setCueRollupForTest`; web's `WasmFeedManager
/// .setCueRollupForTest`, windows' `FfiFeedManager.SetCueRollupForTest`).
/// Payload: `{"content_ids": [str]}`.
///
/// Why a capture-less seam exists at all (`engagement-cues.md` § At rest): the
/// witness this serves — `test_engagement_cues.py::
/// test_engagement_toggle_and_clear_activity_data` — never dwells, so without a
/// real `cues:v1` row on the nest "Clear activity data" has nothing to delete
/// and the button's check degenerates to "didn't error". That degenerate check
/// is the exact mutation the test was hardened against in 2026-07-29 (neutering
/// `Action::ClearEngagementData` to a no-op left the suite fully green), so the
/// seed is load-bearing for the assertion, not a convenience.
///
/// **Awaited inline, and a refusal is LOUD** (`testing.md` convention 11). The
/// nest round trip must have LANDED before this command acks, because the
/// caller reads the nest's own `cues:v1` row count on the very next line with
/// no poll around it — a fire-and-forget `Task` here (the shape
/// `FeedInjectErrorTestCommand` can afford, since it only paints) would race
/// that read and report a missing row as a product bug. The shells' ack fires
/// only after `handleTestCommand` returns, so awaiting here is what makes the
/// command synchronous for the driver, exactly as `DelegationClockTestCommand`
/// awaits its own repaint.
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2), and
/// takes `client`/`session` directly rather than `AppState`/`MacAppState` (no
/// common protocol between them) — the same shape `FeedInjectErrorTestCommand`
/// and `FeedInjectPostsTestCommand` take.
public enum FeedSeedCueRollupTestCommand {
    /// Apply the command. Returns `nil` on success, or a human-readable reason
    /// the caller must surface as a **loud** TestAgent failure — never a silent
    /// no-op (a dropped command reads downstream as a real product bug).
    @MainActor
    public static func apply(
        _ command: [String: Any], feedVM: FeedVM, client: FaunaClient?, session: SessionState
    ) async -> String? {
        let contentIds = (command["content_ids"] as? [String]) ?? []
        guard let api = client?.api, let secret = session.secretHex else {
            return "feed_seed_cue_rollup_for_test: no live client or session secret, so the "
                + "cue rollup cannot be sealed or PUT to the nest"
        }
        await feedVM.configure(api: api, secretHex: secret)
        guard let mgr = feedVM.manager else {
            return "feed_seed_cue_rollup_for_test: feedVM manager unavailable after configure"
        }
        do {
            try await mgr.setCueRollupForTest(contentIds: contentIds)
            return nil
        } catch {
            return "feed_seed_cue_rollup_for_test: the sealed rollup PUT failed: \(error)"
        }
    }
}

#endif
