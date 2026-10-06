import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `feed_hold_next_reload` / `feed_release_held_reload` — arm or release the
/// feed manager's one-shot reload hold (`FeedManager::hold_next_reload_for_test`
/// over the FFI face): the NEXT reload publishes the list it kept or cleared,
/// then parks before its fetch until the release, so a test can read the page
/// while a refresh is in flight (`ui/feed.md` § The read model — a refresh
/// keeps the posts on screen, only a switch clears them).
///
/// Nothing the apple agent does awaits a feed reload — a page entry or a search
/// starts its reload on the view's own task — so no arm has to start rather than
/// await a feed op while the hold is armed. Twins of tui's, linux's and web's
/// arms of the same names. One implementation for macOS + iOS, like
/// `FeedInjectErrorTestCommand`.
public enum FeedReloadHoldTestCommand {
    /// Returns the refusal sentence, or `nil` once the hold is armed/released —
    /// convention 11: with no feed manager (pre-auth) the command is refused
    /// loudly, never acked.
    @MainActor
    public static func apply(_ action: String, feedVM: FeedVM) -> String? {
        guard let manager = feedVM.manager else {
            return "\(action): no feed manager (pre-auth)"
        }
        if action == "feed_hold_next_reload" {
            manager.holdNextReloadForTest()
        } else {
            manager.releaseHeldReloadForTest()
        }
        return nil
    }
}

#endif
