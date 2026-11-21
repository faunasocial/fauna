import Testing

/// Yield the main actor until `condition` holds — the bounded form of
/// `while !condition() { await Task.yield() }`, which every test that waits for
/// another main-actor task to reach a point must use.
///
/// The bare loop is a run-wide hazard, not a local one. If the awaited point is
/// never reached (a refused create that never re-queries, say), the loop never
/// ends, and `.timeLimit` cannot stop it: a time limit only CANCELS the test's
/// task, and `Task.yield()` ignores cancellation. The spinning test keeps
/// re-enqueueing itself on the main actor, and the `swift test` run as a whole
/// never finishes. This form stops at a yield budget or at cancellation and
/// records the miss as the test's own failure, naming `what` it waited for.
///
/// Returns whether `condition` came true; a caller returns early on `false`.
@MainActor
func yieldUntil(
    _ what: String,
    maxYields: Int = 100_000,
    sourceLocation: SourceLocation = #_sourceLocation,
    _ condition: () -> Bool
) async -> Bool {
    for _ in 0..<maxYields {
        if condition() { return true }
        if Task.isCancelled { break }
        await Task.yield()
    }
    if condition() { return true }
    Issue.record("never reached: \(what)", sourceLocation: sourceLocation)
    return false
}
