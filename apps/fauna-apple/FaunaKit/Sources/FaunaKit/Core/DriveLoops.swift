import Foundation

/// The background loops a page-owned view model drives (a machine's `run_*`
/// call, the repaint tick beside it), held as ONE set so they retire as one.
///
/// A page-owned view model is on no `ActorScope` list (`ActorScope.swift`
/// § State a *view* owns): its drop rides the page's unmount. Two doors close
/// the loops, and neither needs a caller to remember a handle:
///
/// - **`cancelAll()`** — the view model's `reset()` calls it once, so there is
///   no per-handle cancel a new loop could be left out of.
/// - **`deinit`** — when SwiftUI releases the page's `@State`, the view model
///   and this set go with it, and every loop is cancelled. That holds only
///   while each loop's body captures its view model *weakly*; a strong capture
///   keeps the view model alive through its own loop.
///
/// The in-memory corollary of `account-scoping.md` § The scoping taxonomy: the
/// loops that write account-scoped state retire with the account.
@MainActor
final class DriveLoops<Key: Hashable & Sendable> {
    private var tasks: [Key: Task<Void, Never>] = [:]

    init() {}

    /// Start `body` as the loop named `key`, cancelling any loop already there.
    func spawn(_ key: Key, _ body: @escaping @MainActor () async -> Void) {
        tasks[key]?.cancel()
        tasks[key] = Task { @MainActor in await body() }
    }

    /// Start the repaint tick named `key`: every `everyMs` milliseconds, while
    /// `keepGoing` holds and the task is not cancelled, run `read`. Both
    /// closures must capture their view model weakly (see the type's note).
    func spawnTick(_ key: Key, everyMs: UInt64,
                   while keepGoing: @escaping @MainActor () -> Bool,
                   read: @escaping @MainActor () -> Void) {
        spawn(key) {
            while !Task.isCancelled, keepGoing() {
                try? await Task.sleep(for: .milliseconds(everyMs))
                if Task.isCancelled { break }
                read()
            }
        }
    }

    /// Whether the loop named `key` was spawned and has not been cancelled.
    func isRunning(_ key: Key) -> Bool {
        tasks[key].map { !$0.isCancelled } ?? false
    }

    /// Cancel and forget every loop.
    func cancelAll() {
        for task in tasks.values { task.cancel() }
        tasks.removeAll()
    }

    deinit {
        for task in tasks.values { task.cancel() }
    }
}
