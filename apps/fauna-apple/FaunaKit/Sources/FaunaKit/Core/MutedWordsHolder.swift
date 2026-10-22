import SwiftUI

/// The one in-memory copy of the user's muted-word list every collapse and
/// toast gate in this process reads (`docs/goal/behavior/notifications.md`
/// § Muted words → *Where logic lives*: the list each app already holds for its
/// conversation collapse, never a second read) — the conversation bubbles, the
/// notifications page, and the knock toast, which fires with no screen mounted.
/// The apple twin of android's `MutedWordsHolder`, linux's
/// `conversations::muted_keywords_cache()` and tui's `SettingsState::muted_words`.
///
/// Who feeds it: `FaunaClient.startKnockObserver` ``refresh(api:)``es as the
/// knock pump arms (so a fresh launch collapses and gates its toasts without a
/// Settings or conversation visit), a conversation opening refreshes it, every
/// store-change notice (``StoreChangeNotice``, via `FaunaClient.resyncStoreSources`)
/// refreshes it so another device's mute lands with no screen visit, and every
/// `APIClient` muted-keywords write ``publish(_:)``es the list it returns (a
/// gesture's own write needs no re-read). A failed read
/// keeps the list — a store hiccup must not un-collapse every row.
/// `ActorScope.resetSharedState` ``clear()``s it: an incoming account must not
/// collapse by the departing one's words.
@MainActor @Observable
public final class MutedWordsHolder {
    public static let shared = MutedWordsHolder()

    public private(set) var words: [MutedKeyword] = []

    /// Bumped by ``clear()`` so a read still in flight for the departing
    /// account cannot publish its list over the incoming one's.
    private var generation = 0

    init() {}

    /// Re-read the list from the account store (best-effort).
    public func refresh(api: APIClient) async {
        let started = generation
        guard let page = try? await api.mutedKeywordsList() else { return }
        guard generation == started else { return }
        if words != page.keywords { words = page.keywords }
    }

    /// Publish a list the caller already holds (a Settings write's answer).
    public func publish(_ words: [MutedKeyword]) {
        self.words = words
    }

    /// The identity ended.
    public func clear() {
        generation += 1
        words = []
    }
}
