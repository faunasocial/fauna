// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `feed_cue_hydration_hold` — set or clear the live feed manager's in-memory
/// cue-hydration hold (`FfiFeedManager.setCueHydrationHoldForTest`, over
/// `fauna_feed::FeedManager::set_cue_hydration_hold_for_test`). Set before the
/// feed page first mounts, the seat's rollup fetch parks inside the shared
/// `hydrate_cues` until it is cleared — the window in which a put would
/// overwrite the other device's save (`engagement-cues.md` § At rest). The
/// lever for an app process the harness's environment does not reach (a
/// simulator app's launch env, a sandboxed macOS app); tui and linux take the
/// launch-time hold file instead, web the same predicate through
/// `WasmFeedManager.setCueHydrationHoldForTest`. Payload: `{"held": bool}`.
///
/// A seat signed in off the feed has no manager yet, so the command builds it
/// first (the `configure` `FeedSeedCueRollupTestCommand` and
/// `FeedInjectPostsTestCommand` take); the feed page's own `configure` then
/// finds it built for this actor and keeps it, hold and all.
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2).
public enum FeedCueHydrationHoldTestCommand {
    /// Apply the command. Returns `nil` on success, or a human-readable reason
    /// the caller must surface as a **loud** TestAgent failure (convention 11).
    @MainActor
    public static func apply(
        _ command: [String: Any], feedVM: FeedVM, client: FaunaClient?, session: SessionState
    ) async -> String? {
        guard let held = command["held"] as? Bool else {
            return "feed_cue_hydration_hold: payload carried no boolean `held`"
        }
        if feedVM.manager == nil {
            guard let api = client?.api, let secret = session.secretHex else {
                return "feed_cue_hydration_hold: no live client or session secret, so no "
                    + "feed manager can be built to hold"
            }
            await feedVM.configure(api: api, secretHex: secret, deviceIdHex: client?.deviceId ?? "")
        }
        guard let mgr = feedVM.manager else {
            return "feed_cue_hydration_hold: feedVM manager unavailable after configure"
        }
        mgr.setCueHydrationHoldForTest(held: held)
        return nil
    }
}

#endif
#endif // !FAUNA_KIDS
