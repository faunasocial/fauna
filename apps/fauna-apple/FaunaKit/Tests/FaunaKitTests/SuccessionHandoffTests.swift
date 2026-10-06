import Foundation
import Testing
@testable import FaunaKit

// The apple twin of tui's four succession pins (`apps/fauna-tui/src/app.rs` and
// `session.rs`): the ceremony's four survivors have to cross the account switch
// they cause, and die at a reset. `identity-succession.md` § The RecoveryKey →
// *At succession* is the owner.
//
// What these can and cannot say. The mint itself — "a FRESH secret reaches the
// screen unbidden" — necessarily fakes the nest from here, so it is asserted by
// the journey (`test_identity_succession_ceremony.py`) instead; the same split
// tui's own bullet records. What is pinnable here is the part the journey cannot
// isolate: that the obligation SURVIVES the teardown at all, that a reset ends
// it, and that it is claimed exactly once.
//
// Every body is fully synchronous on the MainActor (bar the one `await` on a
// view model with no api), so these process-wide statics cannot interleave with
// another test's — the same reasoning `ActorScopeTests` writes out.

/// The two identities every fixture below names: the account moves from one to
/// the other, and which of them may claim the owed kit is the whole subject of
/// `theDepartingSessionCannotClaimTheSuccessorsKit`.
private let predecessorId = String(repeating: "ef", count: 32)
private let successorId = String(repeating: "cd", count: 32)

@MainActor
private func landedFixture(
    persisted: Bool = true,
    sweepStateJson: String = #"{"status":"ran","groups":2,"old_leaf_removed_everywhere":true}"#,
    succeededAt: Int64? = 1_760_000_000
) -> FfiLandedSuccession {
    FfiLandedSuccession(
        successorSecretHex: String(repeating: "ab", count: 32),
        newActorIdHex: successorId,
        persisted: persisted,
        sweep: FfiSweepView(
            kind: "ran", detail: nil, groups: 2, groupsOldLeafRemoved: 2,
            unattestedMembers: 0, owesWork: false),
        reviewRoster: [],
        sweepStateJson: sweepStateJson,
        succeededAt: succeededAt
    )
}

/// The headline, and the one that is *only* true because the hand-off is not part
/// of the canonical actor-scoped drop: a switch teardown must leave all four
/// standing, and a factory reset must take all four away.
///
/// Mutation check — moving `SuccessionHandoff.clearOnFactoryReset()` into
/// `ActorScope.resetSharedState()` (the tempting "tidy-up" the type's doc warns
/// against) turns the first half of this red.
@MainActor
@Test func theOwedSuccessorKitOutlivesTheSwitchButNotAReset() {
    SuccessionHandoff.clearOnFactoryReset()
    SuccessionHandoff.record(landedFixture(), predecessorActorIdHex: predecessorId)

    // The switch's own teardown — everything actor-scoped goes, these four stay.
    ActorScope.resetSharedState()

    #expect(SuccessionHandoff.kitOwed,
            """
            the successor's owed kit did not survive the switch its own ceremony caused — only \
            the successor's session can mint it, so an obligation dropped here is lost
            """)
    #expect(SuccessionHandoff.predecessorActorIdHex == predecessorId,
            "the predecessor row is unnameable after the switch, so losing it here loses it")
    #expect(SuccessionHandoff.succeededAtUnix == 1_760_000_000)
    #expect(SuccessionHandoff.sweepStateJson != nil,
            "the sweep is rendered AFTER the switch, so it has to ride across it")
    #expect(SuccessionHandoff.sweep != nil,
            """
            the human-facing view is the section's only source for `sweep_copy` at paint \
            time — losing it here paints nothing, exactly as losing `sweepStateJson` above \
            would leave `data.succession_sweep` null after a real succession
            """)

    // A reset destroys every identity on the box: no successor to owe a kit to,
    // no predecessor row for it to seal, nothing left for the sweep to describe.
    SuccessionHandoff.clearOnFactoryReset()

    #expect(!SuccessionHandoff.kitOwed)
    #expect(SuccessionHandoff.predecessorActorIdHex == nil)
    #expect(SuccessionHandoff.succeededAtUnix == nil)
    #expect(SuccessionHandoff.sweepStateJson == nil)
    #expect(SuccessionHandoff.sweep == nil,
            "the view must clear with its state-json twin, or a reset install's next section reads a stale ceremony")
}

/// The succession landed either way, so the obligation is owed either way —
/// including on the arm where the device could not save the successor seed and
/// the user's route back is importing the secret off the error.
@MainActor
@Test func aLandedSuccessionOwesTheSuccessorAKitEvenIfTheAdoptionFails() {
    SuccessionHandoff.clearOnFactoryReset()

    SuccessionHandoff.record(landedFixture(persisted: false), predecessorActorIdHex: predecessorId)

    #expect(SuccessionHandoff.kitOwed,
            """
            a succession whose seed did not persist still moved the account, so the successor \
            is still kitless and escrowless — the obligation stands
            """)

    SuccessionHandoff.clearOnFactoryReset()
}

/// Two hydrates of the section (the `.task` and the client-arrival re-hydrate)
/// must not mint two kits: the second would register a kit the user was never
/// shown, which is the sharp failure the closing act exists to avoid.
@MainActor
@Test func theOwedKitIsClaimedExactlyOnce() {
    SuccessionHandoff.clearOnFactoryReset()
    SuccessionHandoff.record(landedFixture(), predecessorActorIdHex: predecessorId)

    #expect(SuccessionHandoff.claimOwedKit(asSuccessor: successorId))
    #expect(!SuccessionHandoff.claimOwedKit(asSuccessor: successorId),
            "a second claim would mint a second kit, registering one nobody holds")
    #expect(!SuccessionHandoff.kitOwed)

    // The sweep is NOT claimed with the kit: it is read by the state serializer
    // after the switch, and the successor may open the section long after.
    #expect(SuccessionHandoff.sweepStateJson != nil)

    SuccessionHandoff.clearOnFactoryReset()
}

/// A successor whose first launch cannot reach its nest keeps the obligation, so
/// the next launch offers the kit instead of the step vanishing silently.
@MainActor
@Test func anOwedKitWithNoNestYetStaysOwed() async {
    SuccessionHandoff.clearOnFactoryReset()
    SuccessionHandoff.record(landedFixture(), predecessorActorIdHex: predecessorId)

    // A view model that never got an `api` — the "no nest yet" launch.
    await RecoveryKitVM().dischargeOwedSuccessionKit(sessionActorIdHex: successorId)

    #expect(SuccessionHandoff.kitOwed,
            "the discharge consumed the obligation without being able to mint anything")

    SuccessionHandoff.clearOnFactoryReset()
}

/// `data.succession_sweep` is null — never `{}` — when no succession ran, because
/// a journey has to tell "none ran" apart from "one ran and swept nothing".
@MainActor
@Test func theSweepSerializesAsNullUntilOneRuns() {
    SuccessionHandoff.clearOnFactoryReset()

    #expect(SuccessionHandoff.sweepStateForSerialization() is NSNull)

    SuccessionHandoff.record(landedFixture(), predecessorActorIdHex: predecessorId)
    let decoded = SuccessionHandoff.sweepStateForSerialization() as? [String: Any]

    #expect(decoded?["status"] as? String == "ran",
            """
            the state key must republish the shared `SweepStatus::state_json` object verbatim \
            — re-encoding it here is how the cross-app shape drifts
            """)
    #expect(decoded?["groups"] as? Int == 2)

    SuccessionHandoff.clearOnFactoryReset()
}

/// `data.succession_witness`'s twin obligation: `nil`
/// — no conversations session yet — serializes as `NSNull`, never `{}`, because
/// only an empty-but-PRESENT report indicts the inbound poll; a report that
/// exists republishes verbatim, no re-derivation.
@MainActor
@Test func theWitnessSerializesAsNullBeforeASessionExists() {
    #expect(AppStateObservables.successionWitnessForSerialization(json: nil) is NSNull)

    let json = #"{"harvest":{},"anchor_store":{},"statements":[],"verified_statements":[],"peers":[]}"#
    let decoded = AppStateObservables.successionWitnessForSerialization(json: json) as? [String: Any]

    #expect(decoded?["peers"] != nil,
            """
            the state key must republish the shared `witness::state_json` object verbatim \
            — re-encoding it here is how the cross-app shape drifts
            """)
}

/// The defect the first `--app macos` journey run measured (2026-08-22), and the
/// reason the claim is bound to an actor at all.
///
/// The Recovery Kit section is **mounted while the ceremony runs** and stays
/// mounted through the teardown that follows it, so it re-hydrates as the
/// DEPARTING identity — whose bearers the succession has just revoked. An
/// unbound claim is taken there, mints against a nest that refuses it, and the
/// obligation is spent before the successor's session ever mounts the section.
/// The run that found it: the successor connected 0.3 s after the ceremony and
/// was never offered a kit, with nothing on any error surface to say why.
///
/// Mutation check — dropping the `successorActorIdHex ==` half of
/// `claimOwedKit(asSuccessor:)` turns the first expectation below red.
@MainActor
@Test func theDepartingSessionCannotClaimTheSuccessorsKit() {
    SuccessionHandoff.clearOnFactoryReset()
    SuccessionHandoff.record(landedFixture(), predecessorActorIdHex: predecessorId)

    #expect(!SuccessionHandoff.claimOwedKit(asSuccessor: predecessorId),
            "the outgoing session took the successor's obligation and spent it")
    #expect(SuccessionHandoff.kitOwed,
            "a refused claim must leave the obligation standing for the successor")

    #expect(SuccessionHandoff.claimOwedKit(asSuccessor: successorId),
            "the successor's own session is exactly who this is for")

    SuccessionHandoff.clearOnFactoryReset()
}

/// The **seat** half of the same rule, and the third defect of this class
/// : knowing *who* is claiming is not enough if the
/// client the mint would ride belongs to somebody else.
///
/// Measured on iOS 2026-08-26, twice in four journey runs. The section hydrated
/// with `sessionActorIdHex` already flipped to the successor while the
/// environment still held the PREDECESSOR's `FaunaClient`, so `configure(api:)`
/// installed a seat whose bearers the ceremony had revoked inside the nest's own
/// transaction. `claimOwedKit` passed — it was the successor asking — and all
/// five mint attempts then failed `fauna.protocol.disconnected` over 43 s
/// against a socket that could never come back.
///
/// Pinned here rather than left to the journey because the journey caught it
/// only half the time: whether the successor's client reaches the environment
/// before or after that hydrate is a scheduling race, and a 50 % signal is not
/// coverage. The refusal is the whole mechanism — the mint that follows it
/// needs a nest, so that half stays the journey's (the same split this file's
/// header draws).
///
/// ⚠ **`kitOwed` alone cannot say this** — and that is worth spelling out,
/// because it is the obvious assertion and it is vacuous. With the guard
/// deleted the discharge claims, burns all five mint attempts against an
/// unreachable nest, and then `rearmUnshownKit` puts the obligation *back* — so
/// the flag reads `true` either way and only the wall clock (13 s versus
/// instant) differs, which convention 14 rules out as an assertion. The witness
/// that separates them is `errorText`: a refusal that happens BEFORE the claim
/// surfaces nothing, while five failed mints leave the last transport error on
/// the section. Measured — this exact hole passed the mutation check below
/// until the `errorText` expectation was added.
///
/// Mutation check — deleting the `seat == sessionActorIdHex` guard from
/// `dischargeOwedSuccessionKit` turns the `errorText` expectation below red.
@MainActor
@Test func aKitIsNeverMintedOverAnotherActorsSeat() async {
    SuccessionHandoff.clearOnFactoryReset()
    SuccessionHandoff.record(landedFixture(), predecessorActorIdHex: predecessorId)

    // A seat primed with SOME identity's secret — whichever actor that derives
    // to, it is not `successorId`, which is the only thing this asserts.
    let strangersSeat = APIClient(nodeUrl: URL(string: "https://nest.invalid")!)
    strangersSeat.primeSecret(String(repeating: "77", count: 32))
    #expect(strangersSeat.boundActorIdHex != nil,
            "a primed seat must name its actor, or the guard under test is vacuous")
    #expect(strangersSeat.boundActorIdHex != successorId,
            "fixture error: this seat is supposed to belong to someone else")

    let vm = RecoveryKitVM()
    vm.configure(api: strangersSeat)
    await vm.dischargeOwedSuccessionKit(sessionActorIdHex: successorId)

    #expect(SuccessionHandoff.kitOwed,
            """
            the obligation was spent on a seat belonging to another actor — the mint \
            rides that client, so this is the silent loss `dischargeOwedSuccessionKit` \
            exists to prevent, not a retry
            """)
    #expect(vm.errorText == nil,
            """
            the seat was refused only AFTER the mint was attempted — an error surface \
            means the one-shot claim was taken and spent against a nest this identity \
            cannot authenticate to, which the re-arm then hides behind a `kitOwed` that \
            reads correct
            """)

    SuccessionHandoff.clearOnFactoryReset()
}

/// The witness the guard above is built on, pinned on its own: a seat names the
/// actor it signs as, and two seats primed with different secrets never agree.
///
/// Cheap and worth having because everything else in this area now keys off it —
/// `RecoveryKitSection.hydrateKey` re-fires on it, so an accessor that silently
/// returned a constant (or `nil`) would restore the original defect while every
/// test above stayed green.
@MainActor
@Test func aSeatNamesTheActorItSignsAs() {
    let url = URL(string: "https://nest.invalid")!

    let one = APIClient(nodeUrl: url)
    one.primeSecret(String(repeating: "11", count: 32))
    let other = APIClient(nodeUrl: url)
    other.primeSecret(String(repeating: "22", count: 32))

    #expect(APIClient(nodeUrl: url).boundActorIdHex == nil,
            "an unprimed client has no seat, and that is a third state — not an actor")
    #expect(one.boundActorIdHex?.count == 64)
    #expect(one.boundActorIdHex != other.boundActorIdHex,
            "two secrets derived to the same actor id — the seat cannot tell clients apart")
    #expect(one.boundActorIdHex == one.boundActorIdHex,
            "the memoized read disagreed with itself")
}

// ── The relaunch adoption (`succession-propagation.md` § Propagation → *Own
// device fleet*, the relaunch-adoption clause) — tui's `App::adopt_held_successor`
// twin. A lost reply's relaunch takes the account back without the ceremony's
// fold, so it owes both of the fold's closing acts: the kit AND the sweep.

/// The adoption owes the successor its kit and its sweep, both surviving the
/// switch the adoption triggers, and a reset ends both.
///
/// Mutation check — dropping `sweepOwedTo = nil` from `clearOnFactoryReset()`
/// turns the last expectation red.
@MainActor
@Test func aRelaunchAdoptionOwesTheKitAndTheSweepAcrossTheSwitchButNotAReset() {
    SuccessionHandoff.clearOnFactoryReset()

    SuccessionHandoff.recordRelaunchAdoption(
        predecessorActorIdHex: predecessorId, successorActorIdHex: successorId)
    ActorScope.resetSharedState()

    #expect(SuccessionHandoff.kitOwed,
            "the ceremony never minted the successor's kit; the adoption still owes it")
    #expect(SuccessionHandoff.successorActorIdHex == successorId)
    #expect(SuccessionHandoff.predecessorActorIdHex == predecessorId,
            "the owed kit's mint seals the predecessor — it must cross the switch")
    #expect(SuccessionHandoff.sweepOwedTo == successorId,
            """
            the relaunch never ran the group sweep, so the retired leaf keeps its seat in \
            every group — the successor's first session owes the sweep
            """)
    #expect(SuccessionHandoff.sweep == nil,
            "no sweep ran, so no report may be carried — a parked report is the discharge's")

    SuccessionHandoff.clearOnFactoryReset()
    #expect(!SuccessionHandoff.kitOwed)
    #expect(SuccessionHandoff.sweepOwedTo == nil,
            "a reset leaves no successor to sweep for")
}

/// The owed sweep is claimed only by the successor's own session, and once —
/// independently of the kit, whose claim must not spend it (the sweep is
/// discharged first, but a deferred sweep can land after the kit's claim).
@MainActor
@Test func theOwedSweepIsClaimedOnceAndOnlyByTheSuccessor() {
    SuccessionHandoff.clearOnFactoryReset()
    SuccessionHandoff.recordRelaunchAdoption(
        predecessorActorIdHex: predecessorId, successorActorIdHex: successorId)

    #expect(!SuccessionHandoff.claimOwedSweep(asSuccessor: predecessorId),
            "the retired identity's session must not take the successor's sweep")
    #expect(SuccessionHandoff.claimOwedKit(asSuccessor: successorId))
    #expect(SuccessionHandoff.sweepOwedTo == successorId,
            "claiming the kit must not spend the sweep")
    #expect(SuccessionHandoff.claimOwedSweep(asSuccessor: successorId))
    #expect(!SuccessionHandoff.claimOwedSweep(asSuccessor: successorId),
            "a second hydrate must not sweep twice")

    SuccessionHandoff.rearmOwedSweep(successor: successorId)
    #expect(SuccessionHandoff.sweepOwedTo == successorId,
            "a press that could not run puts the obligation back, bound to the same successor")

    SuccessionHandoff.clearOnFactoryReset()
}

/// A ceremony that ran its own sweep owes none: only the adoption parks the
/// flag, or every post-ceremony hydrate would re-sweep over a finished report.
@MainActor
@Test func aCeremonyThatSweptOwesNoSweep() {
    SuccessionHandoff.clearOnFactoryReset()
    SuccessionHandoff.record(landedFixture(), predecessorActorIdHex: predecessorId)

    #expect(SuccessionHandoff.sweepOwedTo == nil)

    SuccessionHandoff.clearOnFactoryReset()
}

// ── The no-second-mint rule (`SuccessionHandoff.rearmUnshownKit`'s ⚠) ──────────
//
// A kit minted into a section that left the screen is the registered chain head,
// and the discharge's no-prior mint is refused over a registered head
// (`PriorKitRequired`). The old re-arm handed back a BARE obligation, so every
// later hydrate claimed it, burned five refused mints and re-armed — ~280 refused
// mints on the 2026-09-24 `--app macos` journey, the successor never shown a kit.

private let strandedFixture = SuccessionHandoff.StrandedKit(
    secretHex: String(repeating: "5a", count: 32),
    kitUri: "fauna://recovery?secret=\(String(repeating: "5a", count: 32))",
    escrowStored: true, landsAt: nil)

/// The next live section SHOWS the stranded kit — no second mint. Runs over a
/// seat that really is the successor's, pointed at an unreachable nest, so any
/// mint attempt fails and leaves nothing on screen.
///
/// Mutation check — dropping the `takeStrandedKit` hand-over from
/// `dischargeOwedSuccessionKit` sends the discharge to mint against
/// `nest.invalid`, which shows nothing and turns the first expectation red.
@MainActor
@Test func aKitMintedOffScreenIsShownByTheNextSectionNotMintedAgain() async {
    SuccessionHandoff.clearOnFactoryReset()
    let seat = APIClient(nodeUrl: URL(string: "https://nest.invalid")!)
    seat.primeSecret(String(repeating: "33", count: 32))
    guard let successor = seat.boundActorIdHex else {
        Issue.record("a primed seat must name its actor")
        return
    }
    SuccessionHandoff.recordRelaunchAdoption(
        predecessorActorIdHex: predecessorId, successorActorIdHex: successor)
    #expect(SuccessionHandoff.claimOwedKit(asSuccessor: successor))
    // The section that minted left the screen: obligation AND kit go back.
    SuccessionHandoff.rearmUnshownKit(successor: successor, stranded: strandedFixture)

    let vm = RecoveryKitVM()
    vm.configure(api: seat)
    await vm.dischargeOwedSuccessionKit(sessionActorIdHex: successor)

    #expect(vm.mintedSecretHex == strandedFixture.secretHex,
            """
            the live section did not show the kit the off-screen one minted — a second \
            no-prior mint is refused over that registered head, so the successor is never \
            shown a kit at all
            """)
    #expect(vm.mintedKitUri == strandedFixture.kitUri)
    #expect(vm.errorText == nil, "showing a held kit makes no nest call that could fail")
    #expect(!SuccessionHandoff.kitOwed, "the first showing discharges the obligation")
    #expect(SuccessionHandoff.takeStrandedKit(asSuccessor: successor) == nil,
            "the kit is taken once — a second section must not paint it again")

    SuccessionHandoff.clearOnFactoryReset()
}

/// The stranded kit is bound to its successor and dies at a reset, exactly like
/// the obligation it rides with.
@MainActor
@Test func aStrandedKitIsTakenOnlyByItsSuccessorAndDiesAtAReset() {
    SuccessionHandoff.clearOnFactoryReset()
    SuccessionHandoff.rearmUnshownKit(successor: successorId, stranded: strandedFixture)

    #expect(SuccessionHandoff.takeStrandedKit(asSuccessor: predecessorId) == nil,
            "another identity must never be handed the successor's recovery secret")

    SuccessionHandoff.clearOnFactoryReset()
    #expect(SuccessionHandoff.takeStrandedKit(asSuccessor: successorId) == nil,
            "a reset leaves no successor to show a kit to")

    // A bare re-arm (every attempt failed before a kit landed) carries no kit.
    SuccessionHandoff.rearmUnshownKit(successor: successorId, stranded: strandedFixture)
    SuccessionHandoff.rearmUnshownKit(successor: successorId)
    #expect(SuccessionHandoff.takeStrandedKit(asSuccessor: successorId) == nil)

    SuccessionHandoff.clearOnFactoryReset()
}

/// The retry stops — without re-arming — once the chain says a no-prior create is
/// refused: a head this device does not hold can never be minted over. An unread
/// chain is NOT that answer (the read raced the reconnect the retry exists for).
@MainActor
@Test func theDischargeRetryStopsOnlyOnAPositiveCreateRefusal() {
    func status(allowsCreate: Bool) -> FfiRecoveryKitStatus {
        FfiRecoveryKitStatus(
            kind: allowsCreate ? "never-created" : "registered",
            allowsCreate: allowsCreate, allowsReplace: !allowsCreate, allowsLost: !allowsCreate,
            allowsStolen: true, allowsEscrowReseal: false,
            pendingNewPubkeyHex: nil, pendingLandsAt: nil)
    }
    #expect(RecoveryKitVM.mintCanNeverLand(status(allowsCreate: false)))
    #expect(!RecoveryKitVM.mintCanNeverLand(status(allowsCreate: true)))
    #expect(!RecoveryKitVM.mintCanNeverLand(nil))
}
