import SwiftUI

/// The Personalization home's Trained-topics facet CRUD glue over the sealed
/// `topic:<hex>` factor registry (topic-factors.md § Authoring surface &
/// picker). Mirrors `MutedWordsVM`'s shape — free-function round trips, no
/// client-side state machine, always re-renders from the returned row list
/// rather than locally guessing the post-write state. Trained topics has no
/// stateful machine of its own; the training gestures (train/untrain/mute)
/// ride the shared `FfiFeedManager` owned by `FeedVM`, not this VM.
@MainActor @Observable
public final class TrainedTopicsVM {
    public private(set) var rows: [FfiTrainedTopicRow] = []
    public var errorMessage: String?

    /// True while the publish sheet is open. A publish refusal stays on screen
    /// until the sheet is re-opened (topic-factors.md § Implementation status
    /// today), so an unrelated successful gesture (engagement toggle, create,
    /// rename, delete, reload) must not clear it. Set by `PersonalizationView`.
    public var publishSheetOpen = false

    private var api: APIClient?

    /// Bumped at the start and end of every registry write, so a store-change
    /// re-read that overlapped one drops its (possibly older) answer rather
    /// than painting it over the write's own.
    private var gestureEpoch = 0

    public init() {}

    /// The success-path clear every registry round trip shares; held back
    /// while the publish sheet is open so it cannot erase the sheet's refusal.
    private func clearErrorAfterSuccess() {
        if !publishSheetOpen { errorMessage = nil }
    }

    public func configure(api: APIClient) {
        self.api = api
    }

    /// Load the registry. Called on every page appear (mirrors
    /// `MutedWordsVM.load`) so a factor created on another device is picked up.
    public func load() async {
        guard let api else { return }
        do {
            rows = try await api.trainedTopicsList()
            clearErrorAfterSuccess()
        } catch {
            errorMessage = mapError(error)
        }
    }

    /// The store-change notice's re-read (account-runtime.md § Multi-instance
    /// concurrency, the own-pump notice, part 4 — Personalization's trained
    /// topics): replaces the rows only when they differ, never touches
    /// `errorMessage`, the publish sheet or a typed name (the view's), and
    /// drops its answer if a registry write ran meanwhile — the write's own
    /// answer is the fresher one.
    public func resync() async {
        guard let api else { return }
        let epoch = gestureEpoch
        guard let fresh = try? await api.trainedTopicsList(), epoch == gestureEpoch else { return }
        if fresh != rows { rows = fresh }
    }

    /// Create a new trained factor named `name` (trimmed; a blank create is a
    /// client-side no-op — the boundary's own `BlankName` error is a defensive
    /// fallback that should never actually fire from this guard).
    public func create(name: String) async {
        guard let api else { return }
        gestureEpoch += 1
        defer { gestureEpoch += 1 }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        do {
            rows = try await api.trainedTopicsCreate(name: trimmed)
            clearErrorAfterSuccess()
        } catch {
            errorMessage = mapError(error)
        }
    }

    /// Rename an existing factor's display name (trimmed; blank is a no-op,
    /// same guard as `create`).
    public func rename(id: Data, name: String) async {
        guard let api else { return }
        gestureEpoch += 1
        defer { gestureEpoch += 1 }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        do {
            rows = try await api.trainedTopicsRename(id: id, name: trimmed)
            clearErrorAfterSuccess()
        } catch {
            errorMessage = mapError(error)
        }
    }

    /// Toggle a factor's `learn_from_engagement` opt-in (engagement-cues.md §
    /// Layer A).
    public func setLearnFromEngagement(id: Data, on: Bool) async {
        guard let api else { return }
        gestureEpoch += 1
        defer { gestureEpoch += 1 }
        do {
            rows = try await api.trainedTopicsSetLearnFromEngagement(id: id, on: on)
            clearErrorAfterSuccess()
        } catch {
            errorMessage = mapError(error)
        }
    }

    /// Delete a trained factor (registry remove + `model.delete` — compositions
    /// still referencing the key stay valid via the zero-term seam).
    public func delete(id: Data) async {
        guard let api else { return }
        gestureEpoch += 1
        defer { gestureEpoch += 1 }
        do {
            rows = try await api.trainedTopicsDelete(id: id)
            clearErrorAfterSuccess()
        } catch {
            errorMessage = mapError(error)
        }
    }

    /// Map the boundary's machine-readable error `code` to localized copy —
    /// blank-name and cap are distinct so a user who left the box empty is
    /// never told they ran out of topics (mirrors windows' `TrainedTopicsViewModel`).
    /// Anything not this boundary's own error type (including a cancellation)
    /// falls to `DisplayError.message`, same as every other FaunaKit catch.
    /// `internal` (not `private`) so the unit tests can pin the mapping,
    /// mirroring `BackupDestinationsVM.mapError`.
    func mapError(_ error: Error) -> String? {
        guard let ffi = error as? FfiTrainedTopicsError else { return DisplayError.message(error) }
        switch ffi {
        case .Cap(let max): return L.personalization.trainedFactorCap(max: "\(max)")
        case .General(let msg): return msg
        case .BlankName: return L.personalization.trainedFactorBlankName
        }
    }
}
