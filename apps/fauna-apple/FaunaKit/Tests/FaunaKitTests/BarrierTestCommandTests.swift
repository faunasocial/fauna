import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for apple's `barrier` leg (`e2e-conventions.md` § convention
// 14; contract in `fauna_e2e_agent::BARRIER`).
//
// ⚠ **This file, not the e2e, is what makes apple's mechanism load-bearing —
// and unusually, apple can pin it here in a way tui/linux/web could not.** The
// cross-app `test_agent_barrier.py` grades a barrier only insofar as no gap
// exists for the queue to drain unattributably; that is why the fused
// `barrier_probe` had to be invented (linux's M4 and web's M5 mutants survived
// the two-command test). In-process there is no round trip at all: the probe's
// blocks sit on `DispatchQueue.main` and nothing can run them while this
// main-actor test body holds the thread, so deleting the hops in `runBarrier`
// leaves `barrier_ack_probe` at `nil` and reds these cases outright.
//
// 🧪 **Mutation-graded by RUNNING the mutant** (2026-08-13): removing both hops
// from `BarrierTestCommand.runBarrier` (leaving the freeze, so the red is about
// *ordering* and not a missing observable) kills **3 of these 7 cases** —
// `fusedProbeOrdersItsOwnBatchBeforeAcking` and `barrierFreezesWorkQueuedBeforeIt`
// at `(ackProbe() → nil) == "tok-fused#2"` / `"tok-bare#2"` (the exact "acked
// without barriering at all" signature the e2e reports), plus
// `clearResetsBothSlots` at its `!= nil` precondition, which needs a real barrier
// to have frozen anything at all.
//
// The other two — `barrierMakesNoPromiseAboutWorkQueuedAfterIt` and
// `repeatedBarriersOnAnIdleAppAreSafe` — survive **by design, and that is worth
// stating rather than fixing**: both assert an ABSENCE, and a barrier that does
// nothing makes an absence trivially true. That is the same vacuity class
// `BARRIER_ACK_PROBE_KEY` documents, reached from the other side (and the reason
// `e2e-conventions.md` grades absence-guarding barriers on their *ordering*
// instead). They are kept as bound/no-hang cases, not counted as mechanism
// coverage.
//
// `.serialized` because every case mutates the SAME process-wide probe slots,
// mirroring `AutomationActuationGateTests`' actuation-log suite.
#if DEBUG

@MainActor
@Suite("Barrier test-agent command (convention 14's causal anchor)", .serialized)
struct BarrierTestCommandTests {
    /// The published `state.barrier_ack_probe` — the frozen ack-time observable,
    /// the ONLY key the cross-app self-test asserts. `NSNull` reads as `nil`.
    private func ackProbe() -> String? {
        BarrierTestCommand.stateFragment["barrier_ack_probe"] as? String
    }

    /// The published live `state.barrier_probe`.
    private func liveProbe() -> String? {
        BarrierTestCommand.stateFragment["barrier_probe"] as? String
    }

    private func probe(
        _ token: String, count: Int, fused: Bool = false
    ) async -> String? {
        var command: [String: Any] = ["token": token, "count": count]
        if fused { command["barrier"] = true }
        return await BarrierTestCommand.apply(
            action: BarrierTestCommand.probeAction, command: command)
    }

    // MARK: - The mechanism

    /// The grading case: enqueue and barrier inside ONE command, exactly as the
    /// fused `barrier_probe` does over the wire. The frozen value can be the last
    /// item only if the barrier genuinely ordered the whole batch.
    @Test func fusedProbeOrdersItsOwnBatchBeforeAcking() async {
        BarrierTestCommand.clear()

        let refusal = await probe("tok-fused", count: 3, fused: true)

        #expect(refusal == nil)
        #expect(ackProbe() == "tok-fused#2", """
            a fused probe must not ack until its own batch has run; nil means it \
            acked without barriering at all
            """)
    }

    /// The contract in its real usage shape: probe (early ack), then a separate
    /// `barrier`. In-process this discriminates the mechanism too — unlike the
    /// e2e twin, where the inter-command round trip drains the queue unaided.
    @Test func barrierFreezesWorkQueuedBeforeIt() async {
        BarrierTestCommand.clear()

        _ = await probe("tok-bare", count: 3)
        _ = await BarrierTestCommand.apply(
            action: BarrierTestCommand.barrierAction, command: [:])

        #expect(ackProbe() == "tok-bare#2",
                "after `barrier`, ALL work enqueued before it must have run")
    }

    /// The bound the contract states: a barrier covers work enqueued *before* it
    /// and does not wait for anything queued after. An implementation that waited
    /// for "a bit more" would be a settle-sleep wearing the barrier's name.
    @Test func barrierMakesNoPromiseAboutWorkQueuedAfterIt() async {
        BarrierTestCommand.clear()

        _ = await BarrierTestCommand.apply(
            action: BarrierTestCommand.barrierAction, command: [:])
        #expect(ackProbe() == nil, "nothing was queued before that barrier")

        _ = await probe("tok-late", count: 2)
        #expect(ackProbe() == nil, """
            the earlier barrier froze what IT saw; a later probe cannot \
            retroactively change that value
            """)
    }

    /// Two back-to-back barriers over an empty queue both return — the idle-app
    /// case the cross-app suite asserts as a non-hang.
    @Test func repeatedBarriersOnAnIdleAppAreSafe() async {
        BarrierTestCommand.clear()

        _ = await BarrierTestCommand.apply(
            action: BarrierTestCommand.barrierAction, command: [:])
        _ = await BarrierTestCommand.apply(
            action: BarrierTestCommand.barrierAction, command: [:])

        #expect(ackProbe() == nil)
    }

    // MARK: - Lifecycle + refusals

    /// `reset`/`logout` clear both slots, so a token cannot leak into the next
    /// test of a reused app process — the precondition the cross-app suite
    /// asserts before it probes.
    @Test func clearResetsBothSlots() async {
        _ = await probe("tok-leak", count: 2, fused: true)
        #expect(ackProbe() != nil)

        BarrierTestCommand.clear()

        #expect(ackProbe() == nil)
        #expect(liveProbe() == nil)
    }

    /// Convention 11: a token-less probe would ack green and prove nothing — the
    /// silent drop wearing a disguise. It must refuse LOUDLY instead.
    @Test func aTokenLessProbeRefusesInsteadOfPassingQuietly() async {
        BarrierTestCommand.clear()

        let missing = await BarrierTestCommand.apply(
            action: BarrierTestCommand.probeAction, command: [:])
        let empty = await probe("", count: 4)

        #expect(missing?.contains("token") == true)
        #expect(empty?.contains("token") == true)
        #expect(ackProbe() == nil)
    }

    /// The name set this app claims to implement is spelled once; a shell's
    /// `switch` and this handler cannot disagree about it.
    @Test func handlesExactlyTheTwoCrossAppNames() {
        #expect(BarrierTestCommand.handles("barrier"))
        #expect(BarrierTestCommand.handles("barrier_probe"))
        #expect(!BarrierTestCommand.handles("some_other_command"))
        // Mirrors `fauna_e2e_agent::BARRIER` / `::BARRIER_PROBE` — the wire
        // names, asserted so a rename here cannot silently drop the command.
        #expect(BarrierTestCommand.barrierAction == "barrier")
        #expect(BarrierTestCommand.probeAction == "barrier_probe")
    }
}

#endif
