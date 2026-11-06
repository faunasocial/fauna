// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `feed_cue_verdict_for_test` — the live engagement-cue engine's verdict for
/// `content_id` and whether it has hydrated from the nest yet, read-only:
/// `{"hydrated": bool, "verdict": "watch-complete" | "skip" | null}` on the
/// shell's machine-method result. How a second seat witnesses that it starts
/// from the rollup the first collected (`engagement-cues.md` § At rest), over
/// the same `FeedManager::cue_verdict_for_test` shared-Rust seam tui and linux
/// drive (`FfiFeedManager.cueVerdictForTest`; web's `WasmFeedManager
/// .cueVerdictForTest`). Payload: `{"content_id": str}`.
///
/// Pre-auth (no feed manager yet) reads as not hydrated, the shape linux's arm
/// answers — the caller polls on `hydrated`, so a not-yet-configured manager
/// is a wait, not a refusal.
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2).
public enum FeedCueVerdictTestCommand {
    @MainActor
    public static func apply(_ command: [String: Any], feedVM: FeedVM) -> [String: Any] {
        let contentId = (command["content_id"] as? String) ?? ""
        guard let mgr = feedVM.manager else {
            return ["hydrated": false, "verdict": NSNull()]
        }
        let json = mgr.cueVerdictForTest(contentId: contentId)
        return (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any]
            ?? ["hydrated": false, "verdict": NSNull()]
    }
}

#endif
#endif // !FAUNA_KIDS
