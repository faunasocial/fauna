import SwiftUI

/// The "Muted words" Settings sub-page's thin CRUD glue over the shared sealed
/// `fauna.state.moderation` muted-keyword list (moderation.md § Muted keywords;
/// content-moderation-and-ranking.md § Q3). No client-side state machine — two
/// pure round trips (list / set) over the UniFFI free fns; the shared Rust
/// normalizes on write (trim, drop blanks, case-insensitive dedupe) and this VM
/// always re-renders from the returned record rather than locally guessing the
/// post-save state (mirrors linux/web/android — no client re-derives the
/// normalization).
///
/// The record carries `loaded` beside the terms, which is what lets the view
/// tell "you have muted nothing" from "we have not read your list yet"
/// (`docs/goal/ui/README.md` § *List pages: loading is not empty*). A failed
/// load replaces nothing, so a first-read failure leaves the page unloaded and
/// its `error-message` does the talking.
@MainActor @Observable
public final class MutedWordsVM {
    public private(set) var page = MutedWordsSnapshot(keywords: [], loaded: false)
    public var errorMessage: String?

    /// The persisted terms — the `muted-word-item` rows (each entry's term; its
    /// weight stays on `page.keywords`).
    public var words: [String] { page.keywords.map(\.keyword) }

    /// May `muted-word-empty` paint? Both conditions: the read has resolved AND
    /// it found nothing (the shared `MutedWordsSnapshot::shows_empty_state`,
    /// re-stated here because a `uniffi::Record` carries data, not methods).
    public var showsEmptyState: Bool { page.loaded && page.keywords.isEmpty }

    private var api: APIClient?

    public init() {}

    public func configure(api: APIClient) {
        self.api = api
    }

    /// Load the persisted list. Called on every page appear (mirrors web's
    /// reload-on-enter) so a mute added on another device is picked up.
    public func load() async {
        guard let api else { return }
        do {
            page = try await api.mutedKeywordsList()
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Add `word` (trimmed; a blank add is a no-op) — as a DELTA against the
    /// stored list, never this page's copy wholesale: the
    /// shared seam re-reads the list inside its own CAS update, so a term
    /// another device stored since this page loaded survives this click.
    /// Re-renders from the server-normalized return value.
    public func add(_ word: String) async {
        guard let api else { return }
        let trimmed = word.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        do {
            page = try await api.mutedKeywordsAdd(word: trimmed)
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }

    /// Remove the term at `index` — `add`'s inverse on the same delta seam
    /// (the row's stored spelling crosses, not the whole list); removing a
    /// term another device already deleted is a success no-op.
    public func remove(at index: Int) async {
        guard let api, words.indices.contains(index) else { return }
        do {
            page = try await api.mutedKeywordsRemove(word: words[index])
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
    }
}
