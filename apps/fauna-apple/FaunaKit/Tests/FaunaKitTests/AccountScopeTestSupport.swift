import Foundation
@testable import FaunaKit

// Shared doubles for the page-owned view models' account-scope pins
// (`MediaMachineVMAccountScopeTests`, `BackupsMachineVMAccountScopeTests`,
// `AddressBookVMAccountScopeTests`) — `docs/goal/architecture/apps/account-scoping.md`
// § The scoping taxonomy, the in-memory corollary. `SearchVMAccountScopeTests` keeps
// its own file-private copies of the same shapes (it landed first); these are named
// apart so the two never shadow each other.

/// A scripted failure whose text names the account it was scripted for, so a test
/// can tell WHICH account's failure a page is rendering.
struct ScopeTestFailure: Error, CustomStringConvertible {
    let account: String
    var description: String { "scripted failure for \(account)" }
}

/// A `FfiNestClient` that never opened a socket and is already torn down, so a
/// request through a machine built on it fails at once instead of waiting out a dial
/// (`fauna-client`: a request on a disconnected client fails immediately). What lets
/// a REAL offline machine stand in for an account's live one with nothing dialing
/// `nest.invalid`.
func tornDownNest() async throws -> FfiNestClient {
    let nest = try FfiNestClient(nestUrl: "https://nest.invalid", secret: Data(repeating: 7, count: 32))
    await nest.disconnect()
    return nest
}

/// Makes an in-flight window deterministic with two `AsyncStream`s — the double says
/// when it has reached a call, the test says when that call may return — so there is
/// no sleep and no wall-clock assertion (e2e-conventions.md convention 14).
final class ScopeTestParking: @unchecked Sendable {
    private let entered: AsyncStream<Void>
    private let enteredSignal: AsyncStream<Void>.Continuation
    private let released: AsyncStream<Void>
    private let releaseSignal: AsyncStream<Void>.Continuation

    init() {
        let entering = AsyncStream.makeStream(of: Void.self)
        self.entered = entering.stream
        self.enteredSignal = entering.continuation
        let releasing = AsyncStream.makeStream(of: Void.self)
        self.released = releasing.stream
        self.releaseSignal = releasing.continuation
    }

    /// Suspends until a parked call has reached its suspension point.
    func waitUntilParked() async {
        for await _ in entered { return }
    }

    /// Lets the parked call return.
    func release() {
        releaseSignal.yield()
    }

    /// Park the calling double after it announces itself, until ``release()``.
    ///
    /// The wait runs in a detached task ON PURPOSE: a `for await` in the caller's own
    /// task ends at once when that task is cancelled, and `AddressBookVM.reset()`
    /// cancels its memoized run — which would release the "in-flight" build early and
    /// let the outgoing account's late result land BEFORE the incoming account's, not
    /// after it, hiding exactly the ordering the in-flight guards exist for.
    func parkIfAsked(_ park: Bool) async {
        guard park else { return }
        enteredSignal.yield()
        let released = self.released
        await Task.detached { for await _ in released { return } }.value
    }
}
