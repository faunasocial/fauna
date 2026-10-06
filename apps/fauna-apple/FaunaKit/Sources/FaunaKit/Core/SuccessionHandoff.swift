import Foundation

/// What an identity succession hands **across its own account switch** — the
/// ceremony's closing act, shared by macOS and iOS
/// (`identity-succession.md` § The RecoveryKey → *At succession*).
///
/// ── Why this is not in ``ActorScope/resetSharedState()`` ──────────────────────
///
/// Every other piece of actor-scoped state is dropped at a switch precisely so
/// one identity's state cannot paint under the next one's. These are the
/// **inverse**: they belong to the *outgoing* identity's ceremony, and the
/// teardown they have to survive **is that ceremony's own closing act**. The
/// sweep is its result (rendered after the switch); the owed kit is its last
/// step, which only the successor's session can perform (the mint authenticates
/// as an identity that does not exist as a session until the teardown
/// completes); the predecessor id is what that mint must seal; the nest's
/// stamp is the bound the aftermath's raises classify against; and the
/// aftermath context is the whole of what the successor's first session cannot
/// re-derive about the ceremony that made it one. tui states the
/// same rule at its own declaration sites (`apps/fauna-tui/src/app.rs`'s
/// `succession_{sweep,kit_owed,predecessor,succeeded_at}`), and its teardown
/// carries the matching "do not add these to the list" warning. **Do not "fix"
/// this by calling it from a switch teardown.**
///
/// ── What DOES clear it ────────────────────────────────────────────────────────
///
/// A factory reset, via ``clearOnFactoryReset()``. A reset destroys every
/// identity on the box, so there is no successor left to owe a kit to, no
/// predecessor row left for that kit to seal, and nothing left for the sweep to
/// describe. Same clear point as tui's, and the same reason.
///
/// Static rather than a field on the two `AppState`s deliberately: those are the
/// per-target halves that already drifted once, and this state's whole contract
/// is that it outlives the objects a switch rebuilds.
@MainActor
public enum SuccessionHandoff {
    /// The successor owes itself a fresh kit — set the instant a succession
    /// lands, **including on the adoption-failure path**: the account moved
    /// either way, so it is kitless and escrowless until this is discharged.
    /// Discharged by the successor's own post-auth pass
    /// (``RecoveryKitSection``'s hydrate), which mints unbidden and shows it.
    public private(set) static var kitOwed = false

    /// The identity the account just moved *away* from, 64-hex. Read before the
    /// switch: afterwards the session holds the successor and nothing else names
    /// that row (its registry row survives, but only this says which it is).
    public private(set) static var predecessorActorIdHex: String?

    /// The identity that owes itself the kit — i.e. who ``claimOwedKit(asSuccessor:)``
    /// will hand it to, and nobody else.
    ///
    /// ⚠ **Not bookkeeping — the guard that makes the obligation survivable.**
    /// The ceremony runs from a *mounted* Recovery Kit section, and that view
    /// outlives the ceremony by the width of the teardown: the switch nils the
    /// client, the still-live view re-hydrates, and an unbound claim would be
    /// taken by the OUTGOING session — which then mints against a nest that has
    /// just revoked its bearers, fails, and leaves the flag spent. Measured
    /// exactly so on the first `--app macos` journey run (2026-08-22): the
    /// successor connected 0.3 s after the ceremony and was never offered a kit.
    public private(set) static var successorActorIdHex: String?

    /// Unix seconds the nest applied the succession, when the submit reply
    /// carried it. `None` on the reconcile arm is real and honest, never a
    /// placeholder to be filled in later.
    public private(set) static var succeededAtUnix: Int64?

    /// The pre-switch group sweep's own account of what it managed, in the e2e
    /// state provider's vocabulary — `SweepStatus::state_json`, republished
    /// verbatim by each target's `/state` serializer as `data.succession_sweep`
    /// rather than re-encoded here (the shape is the cross-app contract, so it
    /// lives on the shared status; convention 11).
    ///
    /// ⚠ Not a painting surface: its vocabulary (`no_engine`) deliberately
    /// differs from `FfiSweepView.kind`'s (`no-engine`), which is the human one.
    public private(set) static var sweepStateJson: String?

    /// The pre-switch group sweep's own account of what it managed, as a
    /// surface paints it (`settings.md` § Recovery kit → *The sweep's own
    /// lines*) — same set point and same lifetime as `sweepStateJson` above
    /// (the human-facing twin of that machine one; carried and cleared
    /// together, deliberately never re-derived from each other). Read by
    /// `RecoveryKitVM` at hydrate and passed to `sweep_copy` at paint time,
    /// never matched on `kind` here.
    public private(set) static var sweep: FfiSweepView?

    /// The successor a **relaunch adoption** owes the group sweep to — `nil`
    /// when nothing is owed (`succession-propagation.md` § Propagation → *Own
    /// device fleet*, the relaunch-adoption clause). The ceremony runs its own
    /// sweep before its switch; an adoption cannot (a refused launch never opens
    /// the retired identity's engine), so it owes one, discharged by the
    /// successor's first authenticated session as an unbidden press of the
    /// retry — ``RecoveryKitVM/dischargeOwedSweep(sessionActorIdHex:)``.
    ///
    /// ⚠ **Its own binding, deliberately not ``successorActorIdHex``**:
    /// ``claimOwedKit(asSuccessor:)`` nils that one when the kit is discharged,
    /// and a sweep deferred past the kit's claim (a busy view model) would then
    /// be owed to nobody — the same two-obligations rule as the kit's
    /// own binding.
    public private(set) static var sweepOwedTo: String?

    /// Record a **relaunch adoption** — a launch refused as superseded whose
    /// chain-verified successor this device held, adopted by
    /// ``SupersededLaunchRoute`` — **before** the account switch that follows.
    ///
    /// The ceremony's closing obligations, minus what only the ceremony had:
    /// the kit is owed (the old one retired with the old identity) and so is
    /// the group sweep, which the ceremony would have run itself. No sweep
    /// report is carried — none ran — and no stamp: the reply that carried it
    /// was the one lost. tui's `App::adopt_held_successor` is the twin.
    public static func recordRelaunchAdoption(predecessorActorIdHex: String, successorActorIdHex: String) {
        kitOwed = true
        self.predecessorActorIdHex = predecessorActorIdHex
        self.successorActorIdHex = successorActorIdHex
        succeededAtUnix = nil
        sweepOwedTo = successorActorIdHex
    }

    /// Claim the owed sweep for the session that really is the successor —
    /// once. The actor bind and the single claim guard what
    /// ``claimOwedKit(asSuccessor:)``'s do, for the same reasons.
    public static func claimOwedSweep(asSuccessor actorIdHex: String) -> Bool {
        guard sweepOwedTo == actorIdHex else { return false }
        sweepOwedTo = nil
        return true
    }

    /// Put a claimed sweep back when its press could not run at all — bound
    /// to the successor it is given, like ``rearmUnshownKit(successor:)``.
    public static func rearmOwedSweep(successor actorIdHex: String) {
        sweepOwedTo = actorIdHex
    }

    /// Record a landed succession, **before** the account switch that follows it.
    ///
    /// Called on both arms of `persisted` for the reason the flag documents: the
    /// succession landed either way. The caller switches accounts immediately
    /// afterwards on the persisted arm — everything here is declared to survive
    /// that.
    public static func record(_ landed: FfiLandedSuccession, predecessorActorIdHex: String?) {
        kitOwed = true
        self.predecessorActorIdHex = predecessorActorIdHex
        successorActorIdHex = landed.newActorIdHex
        succeededAtUnix = landed.succeededAt
        sweepStateJson = landed.sweepStateJson
        sweep = landed.sweep
    }

    /// Claim the owed kit for the session that is actually the successor — once.
    ///
    /// Two guards, and each has its own failure it prevents. **The actor bind**
    /// keeps the departing session from taking an obligation it cannot perform
    /// (see ``successorActorIdHex``). **The single claim** keeps two hydrates of
    /// the same section from minting two kits, the second of which would
    /// register a kit nobody was shown — strictly worse than never-created
    /// (`identity-succession.md` § The RecoveryKey → *At succession*).
    public static func claimOwedKit(asSuccessor actorIdHex: String) -> Bool {
        guard kitOwed, successorActorIdHex == actorIdHex else { return false }
        kitOwed = false
        predecessorActorIdHex = nil
        successorActorIdHex = nil
        return true
    }

    /// Put a claimed-but-never-SHOWN obligation back, so the next live section
    /// mints again.
    ///
    /// ⚠ **This exists because "minted" and "shown" are different events, and
    /// only the second one discharges anything.** ``claimOwedKit(asSuccessor:)``
    /// is one-shot by design — it has to be, or the outgoing session would race
    /// the successor for it — but the view that wins the claim is not guaranteed
    /// to be the view the user is looking at. Measured on iOS, 2026-08-26
    /// : the account switch left **two**
    /// `RecoveryKitSection`s mounted for ~100 ms, the one being torn down won the
    /// claim, minted 147 ms after its own `onDisappear`, and took the only copy
    /// of the successor's RecoveryKey out of the view tree with it. The surviving
    /// section asked 0.3 ms later, got `owed=false`, and painted empty for good —
    /// a kit registered on the nest and held by nobody, which
    /// `identity-succession.md` § The RecoveryKey names as *strictly worse than
    /// never-created*.
    ///
    /// ⚠ **A second mint does NOT supersede a stranded one — it collides with
    /// it.** The discharge mints with no prior kit, and the shared ceremony
    /// refuses a no-prior mint over a registered chain head (`PriorKitRequired`,
    /// `fauna_client_recovery::kit::next_seq`): the stranded kit IS that head.
    /// Re-arming a bare obligation after a kit landed therefore bought a mint
    /// that can never succeed, re-armed again on every hydrate — measured
    /// 2026-09-24 as ~280 refused mints while an e2e poll re-opened the
    /// section. So a section that minted off-screen hands the kit ITSELF over
    /// (`stranded`), and the next live section shows that kit instead of
    /// minting: the obligation is discharged by the first *showing* of the one
    /// kit the nest holds. A re-arm with no kit (every mint attempt failed
    /// before one landed) stays a bare obligation — nothing is registered, so
    /// the next mint takes the no-prior arm cleanly.
    ///
    /// The stranded kit lives in process memory only, for the width of one
    /// view swap, and is never written anywhere — the same custody as the view
    /// model that minted it (`identity-succession.md` § The RecoveryKey →
    /// *Custody*: never STORED on any device). A factory reset drops it.
    ///
    /// Deliberately NOT a general "un-claim": it re-binds to the successor it is
    /// given, so a re-arm can never hand the obligation to a different identity
    /// than the one that owed it.
    public static func rearmUnshownKit(successor actorIdHex: String, stranded: StrandedKit? = nil) {
        kitOwed = true
        successorActorIdHex = actorIdHex
        strandedKit = stranded.map { (successor: actorIdHex, kit: $0) }
    }

    /// A minted successor kit that no screen showed — carried to the next live
    /// section by ``rearmUnshownKit(successor:stranded:)``.
    public struct StrandedKit: Equatable, Sendable {
        public let secretHex: String
        public let kitUri: String?
        public let escrowStored: Bool
        public let landsAt: Int64?

        public init(secretHex: String, kitUri: String?, escrowStored: Bool, landsAt: Int64?) {
            self.secretHex = secretHex
            self.kitUri = kitUri
            self.escrowStored = escrowStored
            self.landsAt = landsAt
        }
    }

    /// The kit a section minted and could not show, bound to its successor.
    private static var strandedKit: (successor: String, kit: StrandedKit)?

    /// Take the stranded kit — once, and only for the successor it was minted
    /// for. Called right after a successful ``claimOwedKit(asSuccessor:)``: a
    /// kit here is the one to SHOW, and minting instead would be refused.
    public static func takeStrandedKit(asSuccessor actorIdHex: String) -> StrandedKit? {
        guard let held = strandedKit, held.successor == actorIdHex else { return nil }
        strandedKit = nil
        return held.kit
    }

    /// Replace the carried sweep after `recovery-kit-sweep-retry-button`
    /// finishes the job — the retry's fresh report supersedes the one carried
    /// across the account switch (`settings.md` § Recovery kit → *Finishing an
    /// unfinished group sweep*: re-painting the old view would show the user
    /// the state their press just fixed). Both halves together, same as
    /// `record` sets them together, so the human-facing view and the
    /// `data.succession_sweep` state key can never disagree about which pass
    /// is current.
    public static func replaceSweep(_ fresh: FfiSweepView, stateJson: String) {
        sweep = fresh
        sweepStateJson = stateJson
    }

    /// The reset clear point — see the type's doc for why this is the *only* one.
    public static func clearOnFactoryReset() {
        kitOwed = false
        predecessorActorIdHex = nil
        successorActorIdHex = nil
        succeededAtUnix = nil
        sweepStateJson = nil
        sweep = nil
        sweepOwedTo = nil
        strandedKit = nil
    }

    /// The sweep report as the state serializer publishes it: the decoded
    /// object, or `NSNull` when no succession ran on this app run.
    ///
    /// **Absent stays null rather than an empty object**, for the reason the
    /// shared `state_json_or_null` documents: a journey must be able to tell "no
    /// succession ran" from "one ran and swept nothing".
    public static func sweepStateForSerialization() -> Any {
        guard let json = sweepStateJson,
              let data = json.data(using: .utf8),
              let decoded = try? JSONSerialization.jsonObject(with: data)
        else { return NSNull() }
        return decoded
    }
}
