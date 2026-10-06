import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for apple's convention-14 **counter** observables
// (`e2e-conventions.md` § convention 14, slice D5; contracts in
// `fauna_e2e_agent::SESSION_GENERATION_KEY` and
// `::ACTIVATION_GESTURES_KEY`).
//
// ⚠ **What this file grades that the e2e cannot, and vice versa — they are not
// redundant.** `test_account_switcher_apple.py`'s decline arm grades the
// *product* claim (declining must not tear the session down) by running the
// mutant that makes declining switch: `assert_no_relaunch` then reds on the
// generation moving. What it CANNOT see is the **ordering** the whole helper
// rests on — that the gate counter is bumped only once the verdict has been
// dispatched. A gate that counted itself *first* would still pass the e2e today
// (nothing is dispatched on the decline arm anyway), while silently letting a
// future approve-side assertion observe "settled" before the switch `Task` was
// enqueued, so the `barrier()` behind it would have nothing to order against
// and the negative assert would revert to the race it replaced. That is the
// same "grade the ordering, not the release" rule `e2e-conventions.md` states
// for absence-guarding barriers, and it is gradeable only here.
//
// 🧪 **Mutation-graded by RUNNING both mutants** (2026-08-13), baseline green
// (371/371) first:
//   * **M-A, the ordering mutant** — move `ActivationGestures.recordCompleted()`
//     ABOVE `proceed(...)` on both arms of `AccountSwitcherVM.requestSwitch`.
//     Reds exactly two cases, each at the counter read taken from INSIDE the
//     `proceed` closure and nowhere else:
//     `theUnflaggedArmDispatchesBeforeItCounts` at `(seenInsideProceed → 1) ==
//     (before → 0)` and `theApprovedArmDispatchesBeforeItCounts` at
//     `(seenInsideProceed → 2) == (before → 1)` — the exact reading a too-early
//     bump gives a waiting test. `theDeclinedArmCountsWithoutDispatching`
//     survives it **by design**: that arm dispatches nothing, so it has no
//     ordering to get wrong (and is not counted as coverage of M-A).
//   * **M-B, the missing-bump mutant** — delete the `defer`. Reds both flagged
//     arms at `awaitGestureCount` (`theDeclinedArmCountsWithoutDispatching` and
//     `theApprovedArmDispatchesBeforeItCounts`), which is the signature the e2e
//     would see as a `settled` timeout. The decline arm is the load-bearing
//     half: it is the one with no other observable anywhere in the app.
//
// `.serialized` because every case reads and writes the same process-wide
// counters, mirroring `BarrierTestCommandTests`. Cases assert **deltas**, never
// absolutes: the counters are process-wide by contract and deliberately not
// cleared between tests (`SessionGeneration.count`), so an absolute expectation
// here would be an order-dependent lie.
@MainActor
@Suite("Convention 14 counter observables (session generation + switch gate)",
       .serialized, .timeLimit(.minutes(1)))
struct AutomationCountersTests {
    /// A non-active account entry the switcher will actually route: a fresh VM
    /// has `activeActorId == nil`, so `isActive` is false for any id.
    private func entry(_ actorId: String, requireConfirm: Bool) -> FfiAccountEntry {
        FfiAccountEntry(actorId: actorId, handle: nil, domain: nil, tier: nil,
                        requireConfirmToActivate: requireConfirm)
    }

    /// Let the gate's `Task` run. A bounded yield loop, never a sleep: the job is
    /// a main-actor job and this body holds the main actor, so yielding is the
    /// only thing it is waiting for (convention 14 — no wall-clock dependence,
    /// including in the tests that pin convention 14).
    private func awaitGestureCount(from before: Int) async -> Bool {
        for _ in 0..<1000 {
            if ActivationGestures.count > before { return true }
            await Task.yield()
        }
        return false
    }

    /// Point `AccountReauth` at a temp verdict dir, returning `false` (with the
    /// case failed) if `setenv` is not visible through `ProcessInfo`.
    ///
    /// The guard is not paranoia about `setenv`: without the e2e seam,
    /// `confirmActivation()` falls through to a real `LAContext` policy
    /// evaluation, which on a machine that CAN evaluate would try to raise an OS
    /// prompt and hang the suite. Failing loudly beats hanging (convention 11's
    /// spirit, applied to a test's own preconditions).
    private func withVerdictDir(_ verdict: String?, _ body: () async -> Void) async {
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("fauna-reauth-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer {
            unsetenv("FAUNA_E2E_CREDENTIAL_DIR")
            try? FileManager.default.removeItem(at: dir)
        }
        if let verdict {
            try? verdict.write(to: dir.appendingPathComponent("reauth-result"),
                               atomically: true, encoding: .utf8)
        }
        setenv("FAUNA_E2E_CREDENTIAL_DIR", dir.path, 1)
        guard ProcessInfo.processInfo.environment["FAUNA_E2E_CREDENTIAL_DIR"] == dir.path else {
            Issue.record("""
                setenv is not visible through ProcessInfo.environment, so \
                AccountReauth's e2e verdict seam cannot be driven here. Refusing \
                to fall through to a real LAContext evaluation, which can hang \
                the suite on a machine that can evaluate the policy.
                """)
            return
        }
        // `FaunaE2E.isActive` stays false in `swift test` (it needs
        // FAUNA_E2E_BRIDGE / FAUNA_E2E_AGENT_PORT), so this variable reaches
        // `AccountReauth` — which reads it unconditionally — WITHOUT flipping
        // `KeychainStore`'s e2e file backend, which is gated on `isActive` too.
        await body()
    }

    // MARK: - The session-generation counter

    /// Each recorded teardown is exactly one increment, and they accumulate —
    /// the two halves `assert_no_relaunch` depends on (it reads a delta across
    /// its own gesture, on a counter no `reset`/`logout` ever zeroes).
    @Test func teardownsAccumulateOneAtATime() {
        let before = SessionGeneration.count
        SessionGeneration.recordTeardown()
        #expect(SessionGeneration.count == before + 1)
        SessionGeneration.recordTeardown()
        SessionGeneration.recordTeardown()
        #expect(SessionGeneration.count == before + 3,
                "the counter accumulates across teardowns; it is never reset")
    }

    // MARK: - The account-switch gate (ordering is the graded property)

    /// **Grading case (synchronous arm).** An unflagged row needs no re-auth, so
    /// the gate decides and dispatches inline — and must count itself only
    /// AFTER `proceed` has run. The counter read taken inside `proceed` is what
    /// a mutant that bumps first cannot survive.
    @Test func theUnflaggedArmDispatchesBeforeItCounts() {
        let vm = AccountSwitcherVM()
        let before = ActivationGestures.count
        var seenInsideProceed: Int?
        var confirmedArg: Bool?

        vm.requestSwitch(to: entry("aa11", requireConfirm: false)) { _, confirmed in
            confirmedArg = confirmed
            seenInsideProceed = ActivationGestures.count
        }

        #expect(confirmedArg == false, "an unflagged row switches unconfirmed")
        #expect(seenInsideProceed == before, """
            the gate must dispatch BEFORE counting itself — a test that saw the \
            counter advance here could barrier past a switch that had not been \
            enqueued yet
            """)
        #expect(ActivationGestures.count == before + 1,
                "…and having dispatched, it counts exactly once")
    }

    /// **Grading case (async arm)** — the same ordering claim on the path the
    /// e2e's approve step drives, where `proceed` enqueues the switch `Task`
    /// whose work the following `barrier()` must land behind.
    @Test func theApprovedArmDispatchesBeforeItCounts() async {
        await withVerdictDir("approve") {
            let vm = AccountSwitcherVM()
            let before = ActivationGestures.count
            var seenInsideProceed: Int?
            var confirmedArg: Bool?

            vm.requestSwitch(to: entry("bb22", requireConfirm: true)) { _, confirmed in
                confirmedArg = confirmed
                seenInsideProceed = ActivationGestures.count
            }

            #expect(await awaitGestureCount(from: before),
                    "the flagged arm must count its evaluation once the verdict lands")
            #expect(confirmedArg == true, "an approved re-auth switches CONFIRMED")
            #expect(seenInsideProceed == before,
                    "the gate must dispatch before counting itself on the async arm too")
            #expect(ActivationGestures.count == before + 1)
        }
    }

    /// The arm the e2e's decline step drives, and the reason this counter exists:
    /// apple's re-auth prompt is the OS sheet, so a decline changes NOTHING
    /// observable — no element closes, no state moves, nothing is dispatched.
    /// Without this bump `assert_no_relaunch` would have no `settled` to wait on.
    @Test func theDeclinedArmCountsWithoutDispatching() async {
        await withVerdictDir(nil) {   // absent file = decline, fail-closed
            let vm = AccountSwitcherVM()
            let before = ActivationGestures.count
            let teardownsBefore = SessionGeneration.count
            var proceeded = false

            vm.requestSwitch(to: entry("cc33", requireConfirm: true)) { _, _ in
                proceeded = true
            }

            #expect(await awaitGestureCount(from: before),
                    "a DECLINED gate must still report that it finished evaluating")
            #expect(proceeded == false, "a declined re-auth is a pure no-op")
            #expect(ActivationGestures.count == before + 1)
            #expect(SessionGeneration.count == teardownsBefore,
                    "and it initiates no teardown — the product claim the e2e grades")
        }
    }
}
