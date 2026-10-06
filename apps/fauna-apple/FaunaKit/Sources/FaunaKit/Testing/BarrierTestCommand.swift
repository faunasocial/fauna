import Foundation

// Compiled out of release artifacts (`e2e-conventions.md` convention 15), like
// the app shells' whole `handleTestCommand` surface that calls into it and like
// its `Testing/` siblings — the runtime `FAUNA_E2E_*` gates are the inner switch
// WITHIN a test-capable build, never the boundary.
#if DEBUG

/// Apple's leg of the cross-app `barrier` / `barrier_probe` TestAgent commands —
/// convention 14's causal anchor for negative asserts
/// (`docs/goal/architecture/e2e-conventions.md` § convention 14; the contract and
/// the per-app rationale live in ONE home, `fauna_e2e_agent::BARRIER`).
///
/// **Contract, identical on every app:** the agent acks `barrier` only after all
/// UI-thread work *enqueued before the command* has run. A test proving "X did
/// not happen" reads an observable, triggers, positively awaits the trigger
/// handler's own completion, issues `barrier`, and only then asserts the
/// observable unchanged — the absence is anchored to causal order, so a green run
/// pays nothing and a late X can no longer false-pass. An agent that acks early
/// is worse than no barrier at all: every negative assert built on it silently
/// reverts to the race it was written to remove, and nothing downstream can tell.
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (and so `swift-test`
/// covers it, unlike the app targets), mirroring `DelegationClockTestCommand` /
/// `BackupAuditTestCommand`. Both shells route `barrier` and `barrier_probe` here
/// from `handleTestCommand`.
@MainActor
public enum BarrierTestCommand {
    // MARK: - The cross-app names, mirrored (not imported)
    //
    // apple cannot link `fauna-e2e-agent` (it is a Rust crate the two Rust apps
    // use directly), so — exactly as web's `$lib/barrier-e2e.ts` does — the four
    // constants are re-spelled here against their one documented home. Any drift
    // is caught by `test_agent_barrier.py`, which spells the same values a third
    // time and asserts across the wire.

    /// Mirrors `fauna_e2e_agent::BARRIER`.
    public static let barrierAction = "barrier"
    /// Mirrors `fauna_e2e_agent::BARRIER_PROBE`.
    public static let probeAction = "barrier_probe"
    /// Mirrors `fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD` — the fused-probe flag.
    private static let fuseField = "barrier"
    /// Mirrors `fauna_e2e_agent::BARRIER_PROBE_DEFAULT_COUNT`.
    private static let defaultCount = 64
    /// Mirrors `fauna_e2e_agent::barrier_probe_value`.
    private static func probeValue(_ token: String, _ i: Int) -> String { "\(token)#\(i)" }

    /// Whether this action is one of ours — so a shell's `switch` and this file
    /// cannot disagree about which names apple claims to implement.
    public static func handles(_ action: String) -> Bool {
        action == barrierAction || action == probeAction
    }

    // MARK: - Observables

    /// The probe's last applied value (`state.barrier_probe`), `nil` until one
    /// runs. Written from the queued work itself, i.e. from the queue position
    /// the barrier's own hop must land *after*.
    private static var probeToken: String?

    /// What `probeToken` held when the last barrier acked, frozen
    /// (`state.barrier_ack_probe`) — the ONLY key the self-test asserts.
    ///
    /// ⚠ **The live token above cannot carry that proof, and this is measured
    /// rather than reasoned** (`fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`): the
    /// driver reads state over a round trip *after* the ack, and every app keeps
    /// republishing state meanwhile, so the queue has drained on its own by the
    /// time the read lands and a `barrier` that did nothing at all passes. The
    /// first version of this slice asserted the live key, went green on tui, and
    /// then survived the mutant deleting tui's drain entirely.
    private static var ackProbe: String?

    /// The two top-level state keys, merged into each shell's `serializeState()`.
    /// TOP-level, not under `data` — the cross-app test reads
    /// `get_state("barrier_ack_probe")`, and tui/linux/web publish at that depth.
    public static var stateFragment: [String: Any] {
        [
            "barrier_probe": jsonValue(probeToken),
            "barrier_ack_probe": jsonValue(ackProbe),
        ]
    }

    /// Clear both slots — the `reset`/`logout` clear point, so a token cannot
    /// leak into the next test of a reused app process (both apple drivers reuse
    /// one app across a module). The self-test's own precondition asserts the
    /// frozen key reads `None` before it probes, so a leak surfaces there rather
    /// than silently making a later assertion vacuous.
    public static func clear() {
        probeToken = nil
        ackProbe = nil
    }

    // MARK: - The mechanism

    /// The barrier's whole mechanism — the ordering hops plus the ack-time freeze
    /// — in ONE place, so the bare `barrier` command and the fused
    /// `barrier_probe` cannot drift apart. That sharing is what makes a mutant
    /// meaningful: a mutant applied here is applied to both callers.
    ///
    /// **Apple's shape, and why it is these two hops.** Both apple agents
    /// marshal every op to the main run loop (`apple-e2e-automation.md`), and
    /// work reaches it two ways, so the barrier orders against both:
    ///
    /// - `DispatchQueue.main.async` — the queue the probe's batch rides, and the
    ///   one `InProcessAutomationServer.dispatchCommand` itself hops. Dispatch
    ///   runs a serial queue's blocks **FIFO**, so a block enqueued *here* runs
    ///   strictly after every block enqueued before this command. That is the
    ///   load-bearing hop: it makes the ordering true **by construction**, not by
    ///   an inter-command gap no test pins.
    /// - `Task.yield()` — re-enqueues this job on the MainActor's executor, so we
    ///   also land after main-actor jobs (`Task { @MainActor in … }`) enqueued
    ///   earlier. Cheap, and stated separately because it is a *different* queue
    ///   discipline rather than a second helping of the same one.
    ///
    /// ⚠ Deliberately NOT a settle-sleep, and deliberately not `renderSettleMs`'s
    /// cousin: the contract bounds a barrier to work enqueued *before* it. An
    /// implementation that waited for "a bit more" would be a sleep wearing the
    /// barrier's name — and would show up as a `barrier()` timeout in
    /// `test_agent_barrier.py::test_barrier_is_idempotent_and_safe_on_an_idle_app`
    /// rather than as an honest failure.
    private static func runBarrier() async {
        await Task.yield()
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            DispatchQueue.main.async { continuation.resume() }
        }
        // Freeze what the barrier saw, synchronously in the job the hop resumed —
        // before the driver's next round trip can let anything further run.
        ackProbe = probeToken
    }

    /// Apply `barrier` / `barrier_probe`. Returns `nil` on success, or a
    /// human-readable reason the caller must surface as a **loud** TestAgent
    /// failure — never a silent no-op (convention 11: honour the command or
    /// refuse audibly; a dropped command reads downstream as a real product bug).
    public static func apply(action: String, command: [String: Any]) async -> String? {
        if action == barrierAction {
            await runBarrier()
            return nil
        }
        guard action == probeAction else {
            return "BarrierTestCommand: not my command: \(action)"
        }

        // The probe queues UI work on the same queue real work rides and returns
        // WITHOUT waiting for it. The early ack is the whole self-test: only a
        // correct barrier can make the token observable afterwards.
        guard let token = command["token"] as? String, !token.isEmpty else {
            // A token-less probe would ack green and prove nothing — the silent
            // drop wearing a disguise.
            return "\(probeAction): payload needs a non-empty `token`"
        }
        // JSON numbers arrive as `NSNumber`; take the widest read, matching
        // `DelegationClockTestCommand`'s parser.
        let count = (command["count"] as? NSNumber)?.intValue
            ?? (command["count"] as? Int)
            ?? defaultCount
        // A BATCH, not one item — see `fauna_e2e_agent::BARRIER_PROBE` for why the
        // one-item probe is nearly vacuous on an app whose queue drains by itself.
        for i in 0..<count {
            let value = probeValue(token, i)
            DispatchQueue.main.async {
                MainActor.assumeIsolated { probeToken = value }
            }
        }

        // Fused form (`fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD`): barrier before
        // acking, inside this same command. Deleting the inter-command gap is what
        // makes the mechanism gradeable at all — with the two-command shape the
        // driver's round trip between `barrier_probe` and `barrier` is itself long
        // enough for a main run loop to drain the batch unaided, so a do-nothing
        // barrier is indistinguishable from a correct one (measured on linux and
        // web, 2026-08-13: mutants M4/M5 both survived the two-command test).
        if command[fuseField] as? Bool == true {
            await runBarrier()
        }
        return nil
    }

    /// `nil` → `NSNull`, so the key is always PRESENT in the published state and
    /// reads as JSON `null` rather than vanishing. The driver's `get_state(key)`
    /// maps both to `None`, but a present-and-null key is what lets a failure say
    /// "the barrier acked without draining" instead of "apple has no such key".
    private static func jsonValue(_ value: String?) -> Any { value ?? NSNull() }
}

#endif
