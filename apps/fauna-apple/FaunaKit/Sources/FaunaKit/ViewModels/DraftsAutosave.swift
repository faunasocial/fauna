import Foundation

/// The shared debounced-drafts-autosave shape behind `ConversationsVM.scheduleDraftsSave`
/// and `FeedVM.scheduleDraftsSave` — before this extraction each hand-rolled the identical
/// cancel-then-Task/sleep/`saveIfChanged`/catch-and-log body, and each's own doc comment
/// already cross-referenced the other as a mirror — an acknowledged duplicate nobody had
/// consolidated. `EventsVM.scheduleDraftsSave` is a deliberate near-miss, NOT folded in
/// here: it updates `resumableDraft` immediately, guards on a `draftsGeneration` counter,
/// and calls the differently-shaped `saveDrafts(summary:dtstart:dtend:description:location:)`
/// rather than `saveIfChanged(snapshot:)` — genuinely diverged, the same class of
/// deliberate near-copy as `FaunaMacApp`/`FaunaApp`'s `logoutKeepData`.
///
/// The caller cancels its own previous `draftsSaveTask` before calling this; the returned
/// task becomes the caller's new one. `snapshotBytes` is a closure (not a shared manager
/// type) because `ConversationsManager`'s and `FfiFeedManager`'s `draftsSnapshotBytes()` are
/// two distinct UniFFI-generated protocols with no common supertype to abstract over.
@MainActor
func scheduleDraftsAutosave(
    sync: FfiDraftsSync,
    logTarget: String,
    snapshotBytes: @escaping () -> Data
) -> Task<Void, Never> {
    Task { @MainActor in
        try? await Task.sleep(for: ConversationsVM.draftsSaveDebounce)
        if Task.isCancelled { return }
        do {
            _ = try await sync.saveIfChanged(snapshot: snapshotBytes())
        } catch {
            logMessage(level: .info, target: logTarget,
                       message: "[drafts] autosave failed (transient): \(error)")
        }
    }
}

/// The leave-flush twin of `scheduleDraftsAutosave` — the shared body behind
/// `ConversationsVM.flushDraftsNow` and `FeedVM.flushDraftsNow`
/// (`reserved-folders.md` § The leave-flush promise: "leaving the app never
/// loses the compose input the debounced autosave has not yet caught").
///
/// Cancelling the pending debounced task is the caller's own act (it owns the
/// `draftsSaveTask` handle); this runs the save the cancelled task would have
/// run, immediately and without the sleep. `saveIfChanged` keeps its two
/// no-ops — the pre-restore launch gate and the unchanged-snapshot compare —
/// so a quiet leave costs one cheap snapshot compare and no round trip, which
/// is what lets both macOS's bounded quit gate and iOS's background-task
/// extension fold it in unconditionally.
///
/// Best-effort by the same reasoning as the debounced path: a leave door must
/// not hang or throw on an unreachable nest, and whatever the debounce already
/// landed is still there for the next restore. `EventsVM.flushDraftsNow` is the
/// deliberate near-miss this does not cover, for the same reasons
/// `scheduleDraftsAutosave` does not cover its scheduler.
@MainActor
func flushDraftsAutosave(
    sync: FfiDraftsSync,
    logTarget: String,
    snapshotBytes: () -> Data
) async {
    do {
        _ = try await sync.saveIfChanged(snapshot: snapshotBytes())
    } catch {
        logMessage(level: .info, target: logTarget,
                   message: "[drafts] leave-flush failed (transient): \(error)")
    }
}
