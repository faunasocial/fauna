import Testing

/// Process-wide mutual exclusion for the tests that use `KeychainStore`'s
/// in-memory mode. That store is one static, and every such test opens and closes
/// with `deleteAll()`; a test that `await`s yields the main actor, so a parallel
/// sibling's `deleteAll()` used to wipe the accounts out from under it
/// (`registry.list()` empty mid-test). Hold the lock for the whole test case.
actor SharedKeychainLock {
    static let shared = SharedKeychainLock()
    private var held = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func acquire() async {
        if !held { held = true; return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func release() {
        if waiters.isEmpty { held = false } else { waiters.removeFirst().resume() }
    }
}

struct SharedKeychainTrait: SuiteTrait, TestTrait, TestScoping {
    var isRecursive: Bool { true }

    func provideScope(
        for test: Test, testCase: Test.Case?,
        performing function: @Sendable () async throws -> Void
    ) async throws {
        // Suites and parameterized tests are not units of work; their cases are.
        guard testCase != nil else { return try await function() }
        await SharedKeychainLock.shared.acquire()
        do { try await function() } catch {
            await SharedKeychainLock.shared.release()
            throw error
        }
        await SharedKeychainLock.shared.release()
    }
}

extension Trait where Self == SharedKeychainTrait {
    /// Serialize against every other test holding the shared in-memory keychain.
    static var sharedKeychain: Self { Self() }
}
