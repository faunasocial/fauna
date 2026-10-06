import Foundation
import Testing
@testable import FaunaKit

/// `RecoveryKitVM.stolenVisible` / `.phraseFieldVisible` — the apple twin of
/// windows' `RecoveryKitViewModelTests.TheStolenTriggerRendersEvenWhenTheChainReadFails`
/// / `OnlyAPositiveRefusalHidesTheStolenTrigger`.
/// `docs/goal/ui/settings.md` § Recovery kit — *The four actions* ("stolen
/// (any)"), read against `libs/fauna-ffi/src/recovery.rs`'s
/// `FfiRecoveryKitStatus::allows_stolen` (unconditionally true).

private func status(
    kind: String = "registered",
    allowsCreate: Bool = false,
    allowsReplace: Bool = false,
    allowsLost: Bool = false,
    allowsStolen: Bool = true,
    allowsEscrowReseal: Bool = false,
    pendingLandsAt: Int64? = nil
) -> FfiRecoveryKitStatus {
    FfiRecoveryKitStatus(
        kind: kind,
        allowsCreate: allowsCreate,
        allowsReplace: allowsReplace,
        allowsLost: allowsLost,
        allowsStolen: allowsStolen,
        allowsEscrowReseal: allowsEscrowReseal,
        pendingNewPubkeyHex: nil,
        pendingLandsAt: pendingLandsAt
    )
}

/// The ceremony's authorization is the KIT, never the status — an owner a
/// thief has locked out is exactly the one whose chain read fails, so the
/// trigger and the phrase field it needs must not wait on a read that may
/// never resolve. Covers both the not-yet-read and the failed-read case: a
/// failed read leaves `status` exactly as unset as no read at all, so one
/// fixture (`RecoveryKitVM()`'s default `nil`) stands for both.
///
/// Mutation check: reverting `stolenVisible`/`phraseFieldVisible` to their
/// earlier shape (`guard let s = status else { return false }`) reds
/// this.
@MainActor
@Test func theStolenTriggerRendersWithTheStatusUnread() {
    let vm = RecoveryKitVM()
    #expect(vm.status == nil, "fixture: no read has landed yet (or one failed outright)")

    #expect(vm.stolenVisible)
    #expect(vm.phraseFieldVisible)
}

/// Only a status that POSITIVELY says stolen is disallowed hides it — the
/// shared predicate never does today (`allows_stolen` is unconditionally
/// true), but the render honours it if it ever starts.
@MainActor
@Test func onlyAPositiveRefusalHidesTheStolenTrigger() {
    let vm = RecoveryKitVM()
    vm.status = status(allowsStolen: false)

    #expect(!vm.stolenVisible)
}

/// The phrase field's own widened set of reachable ceremonies, resolved
/// state — unaffected by this change except for the unread case above.
@MainActor
@Test func thePhraseFieldStaysGatedOnReachableCeremoniesOnceResolved() {
    let vm = RecoveryKitVM()
    vm.status = status(allowsReplace: false, allowsStolen: false, allowsEscrowReseal: false)

    #expect(!vm.phraseFieldVisible)

    vm.status = status(allowsStolen: false, allowsEscrowReseal: true)
    #expect(vm.phraseFieldVisible)
}

/// The apple twin of linux's `render_account_error_label` guard
/// (`apps/fauna-linux/src/settings/mod.rs`):
/// `docs/goal/ui/settings.md` § Recovery kit → *The persist-failure message
/// survives the page* (ratified 2026-09-14, all apps) requires a stolen-
/// identity ceremony's persist-failure message — the ONLY surviving copy of
/// the successor's new key — to win over every other write to the shared
/// `errorText` slot until the user acknowledges it. `setErrorText` and
/// `stolenFailedMessagePending` are seeded directly (`internal`, not
/// `private`) because every OTHER writer that reaches the real guard needs a
/// live `APIClient`, which these tests don't stand up.
///
/// Mutation check: dropping the `guard !stolenFailedMessagePending` in
/// `setErrorText` reds this.
@MainActor
@Test func aPendingPersistFailureMessageWinsOverAnyOtherWriteToErrorText() {
    let vm = RecoveryKitVM()
    vm.errorText = "the persist-failure message, with the successor's seed"
    vm.stolenFailedMessagePending = true

    // Stands in for any of the 14 ordinary writers — a validation guard, a
    // caught error, a landed status poll.
    vm.setErrorText("an unrelated write that must not land")

    #expect(vm.errorText == "the persist-failure message, with the successor's seed")
}

/// Once acknowledged, the guard steps aside and ordinary writes land again.
///
/// Mutation check: `acknowledgeStolenFailedMessage` not actually clearing
/// `stolenFailedMessagePending` reds this.
@MainActor
@Test func acknowledgingTheMessageLetsOrdinaryWritesThroughAgain() {
    let vm = RecoveryKitVM()
    vm.errorText = "the persist-failure message"
    vm.stolenFailedMessagePending = true

    vm.acknowledgeStolenFailedMessage()
    vm.setErrorText("an ordinary write")

    #expect(vm.errorText == "an ordinary write")
    #expect(!vm.stolenFailedMessagePending)
}

/// The regression review flagged: `resetForIdentityChange`
/// (fired on an account switch, `RecoveryKitSection.hydrate()`) must not
/// refuse its OWN `errorText = nil` write by tripping over the very guard it
/// is supposed to discharge — a previous identity's parked persist-failure
/// message (its seed) must not stay on screen for the identity that just
/// signed in. `clearHeldSecrets()` (which `resetForIdentityChange` calls
/// first) is what discharges the flag before the write, so ordering here is
/// load-bearing.
///
/// Mutation check: calling `acknowledgeStolenFailedMessage`/discharging AFTER
/// (or never) rather than before the `errorText` write reds this.
@MainActor
@Test func anIdentityChangeDischargesRatherThanRefusingItsOwnErrorClear() {
    let vm = RecoveryKitVM()
    vm.errorText = "the PREVIOUS identity's persist-failure message"
    vm.stolenFailedMessagePending = true

    vm.resetForIdentityChange()

    #expect(vm.errorText == nil, "the new identity must not inherit the old one's parked seed")
    #expect(!vm.stolenFailedMessagePending)
}

/// `clearHeldSecrets()` alone — the `.onDisappear` / off-screen-`hydrate()`
/// nav-away edge, distinct from an identity change — discharges too, so a
/// later ordinary write on the SAME identity's next visit isn't silently
/// dropped forever.
@MainActor
@Test func leavingTheAccountPageDischargesThePendingMessage() {
    let vm = RecoveryKitVM()
    vm.errorText = "the persist-failure message"
    vm.stolenFailedMessagePending = true

    vm.clearHeldSecrets()

    #expect(!vm.stolenFailedMessagePending)
}

/// The account-naming `fauna://recovery` URI embeds the minted secret, so it
/// must leave with it — on the nav-away edge (`clearHeldSecrets`) and on an
/// identity change (`resetForIdentityChange`) alike. A URI that outlived its
/// secret would keep the only copy of the kit in process memory after the
/// screen that showed it is gone (`RecoveryKitVM`, *The kit is held only while
/// the screen shows it*).
@MainActor
@Test func theMintedKitUriIsDroppedWithTheSecretItEmbeds() {
    let secret = String(repeating: "ab", count: 32)
    let vm = RecoveryKitVM()

    vm.mintedSecretHex = secret
    vm.mintedKitUri = "fauna://recovery?secret=\(secret)"
    vm.clearHeldSecrets()
    #expect(vm.mintedSecretHex == nil)
    #expect(vm.mintedKitUri == nil, "leaving the page must not keep the kit's URI")

    vm.mintedSecretHex = secret
    vm.mintedKitUri = "fauna://recovery?secret=\(secret)"
    vm.resetForIdentityChange()
    #expect(vm.mintedKitUri == nil, "a different identity must not inherit the previous kit's URI")
}

// MARK: - The let-go of a dead generation

/// A dead read as `recovery_dead_generations` hands it over: one generation,
/// and the shared projection's line beside it.
private func oneDeadGeneration() -> FfiDeadGenerations {
    FfiDeadGenerations(
        generationIds: [Data(repeating: 7, count: 32)],
        unreadableStatus: LocalizedText(
            key: "settings.recovery_kit.unreadable_status_undated", args: ["rows": "3"]))
}

/// `docs/goal/ui/settings.md` § Recovery kit, the fifth act: the let-go trio
/// renders ONLY while the runtime's dead read answers non-empty, and the
/// button arms on the confirm word alone. The apple twin of tui's
/// `the_let_go_renders_only_while_a_generation_is_dead_and_arms_on_the_word`.
///
/// Mutation check: a `letGoStatus` that answers a line with nothing dead, or
/// a `letGoArmed` that ignores the word, reds this.
@MainActor
@Test func theLetGoRendersOnlyWhileAGenerationIsDeadAndArmsOnTheWord() {
    let vm = RecoveryKitVM()
    #expect(vm.letGoStatus == nil, "nothing is dead: the trio is absent")

    vm.deadGenerations = oneDeadGeneration()
    #expect(vm.letGoStatus != nil, "a dead generation renders the line, field and button")
    #expect(!vm.letGoArmed, "an empty confirm field leaves the button unarmed")

    vm.letGoConfirmInput = "let go"
    #expect(!vm.letGoArmed, "the gate is the literal word, not a case-folded match")

    vm.letGoConfirmInput = recoveryLetGoConfirmWord()
    #expect(vm.letGoArmed)

    vm.busy = true
    #expect(!vm.letGoArmed, "a ceremony in flight disarms every button")
    vm.busy = false

    vm.deadGenerations = FfiDeadGenerations(generationIds: [], unreadableStatus: nil)
    #expect(vm.letGoStatus == nil, "once nothing is dead the trio leaves the screen")
}

/// The confirm word is re-checked when the act FIRES, not only in the render:
/// an agent driving `recovery-kit-let-go-button` directly reaches the handler,
/// and it must refuse out loud rather than retire anything.
///
/// Mutation check: dropping the word guard in `letGo()` reds this (the
/// refusal line never lands).
@MainActor
@Test func theLetGoRefusesOutLoudWithoutTheConfirmWord() async {
    let vm = RecoveryKitVM()
    vm.deadGenerations = oneDeadGeneration()
    vm.letGoConfirmInput = "nearly"

    await vm.letGo()

    #expect(vm.errorText == L.settings.recoveryKit.letGoConfirmPlaceholder)
    #expect(vm.deadGenerations == oneDeadGeneration(), "nothing was let go")
}

/// An armed gate must not survive a page leave, and another identity's dead
/// read must not offer its generations to the one that just signed in.
@MainActor
@Test func theLetGoGateAndListDoNotOutliveTheirPageOrIdentity() {
    let vm = RecoveryKitVM()
    vm.deadGenerations = oneDeadGeneration()
    vm.letGoConfirmInput = recoveryLetGoConfirmWord()

    vm.clearHeldSecrets()
    #expect(vm.letGoConfirmInput.isEmpty, "leaving the page disarms the gate")
    #expect(vm.letGoStatus != nil, "the same identity's read stays")

    vm.resetForIdentityChange()
    #expect(vm.letGoStatus == nil, "a different identity starts with nothing listed")
}

// MARK: - The stolen ceremony's typed outcome

/// `docs/goal/ui/settings.md` § Recovery kit → *The ceremony's outcome is
/// headlined by its arm*: every arm but landed paints the shared
/// `StolenOutcome::message()` verbatim on `error-message`, and the one that
/// carries the only copy of the successor seed is parked exactly as the
/// persist-failure message is. `applyStolenOutcome` is driven directly
/// (`internal`, not `private`) because the ceremony itself needs a live
/// `APIClient`, which these tests don't stand up.
private func unlanded(
    kind: String, key: String, args: [String: String], carriesTheOnlySeed: Bool = false
) -> FfiStolenOutcome {
    FfiStolenOutcome(
        kind: kind,
        message: LocalizedText(key: key, args: args),
        landed: nil,
        carriesTheOnlySeed: carriesTheOnlySeed)
}

/// The undecided arm whose persist was not verified: the sentence holds the
/// seed's only copy, so it is parked — a later ordinary write must not take it
/// off screen — and the session is not switched.
///
/// Mutation check: painting this arm through `setErrorText` without raising
/// `stolenFailedMessagePending` reds this.
@MainActor
@Test func anOutcomeCarryingTheOnlySeedIsParkedAndTheSessionStaysUp() {
    let secret = String(repeating: "cd", count: 32)
    let vm = RecoveryKitVM()
    var switched = false

    let adopted = vm.applyStolenOutcome(
        unlanded(
            kind: "undecided",
            key: "settings.recovery_kit.stolen_outcome_unknown_unsaved",
            args: ["cause": "lookup refused", "reported": "reply lost", "secret": secret],
            carriesTheOnlySeed: true),
        predecessorActorIdHex: nil,
        onSucceeded: { _ in switched = true })

    #expect(!adopted)
    #expect(!switched, "tearing the session down would take the only copy of the seed with it")
    #expect(vm.stolenFailedMessagePending)
    #expect(
        vm.errorText
            == L.settings.recoveryKit.stolenOutcomeUnknownUnsaved(
                cause: "lookup refused", secret: secret, reported: "reply lost"))

    vm.setErrorText("an unrelated write that must not land")
    #expect(vm.errorText?.contains(secret) == true)
}

/// The parking decision is the record's flag, never its kind: the undecided
/// arm whose seed WAS saved carries no secret and is an ordinary message.
///
/// Mutation check: parking on `kind == "undecided"` reds this.
@MainActor
@Test func anUndecidedOutcomeWhoseSeedWasSavedIsPaintedVerbatimAndNotParked() {
    let vm = RecoveryKitVM()

    let adopted = vm.applyStolenOutcome(
        unlanded(
            kind: "undecided",
            key: "settings.recovery_kit.stolen_outcome_unknown_saved",
            args: ["cause": "lookup refused", "reported": "reply lost"]),
        predecessorActorIdHex: nil,
        onSucceeded: { _ in Issue.record("an undecided ceremony must not switch the session") })

    #expect(!adopted)
    #expect(!vm.stolenFailedMessagePending)
    #expect(
        vm.errorText
            == L.settings.recoveryKit.stolenOutcomeUnknownSaved(
                cause: "lookup refused", reported: "reply lost"),
        "the shared sentence, whole — nothing wrapped around it")
}

/// Nothing moved, and landed for another: the shared sentence verbatim, no
/// per-app headline in front of it, nothing parked.
@MainActor
@Test func theOtherUnlandedArmsPaintTheSharedSentenceVerbatim() {
    let vm = RecoveryKitVM()

    _ = vm.applyStolenOutcome(
        unlanded(
            kind: "not-landed",
            key: "settings.recovery_kit.stolen_ceremony_failed",
            args: ["message": "the kit does not match"]),
        predecessorActorIdHex: nil, onSucceeded: { _ in })
    #expect(
        vm.errorText == L.settings.recoveryKit.stolenCeremonyFailed(message: "the kit does not match"))
    #expect(!vm.stolenFailedMessagePending)

    _ = vm.applyStolenOutcome(
        unlanded(
            kind: "landed-for-another",
            key: "settings.recovery_kit.stolen_landed_for_another",
            args: ["actor": "ab12"]),
        predecessorActorIdHex: nil, onSucceeded: { _ in })
    #expect(vm.errorText == L.settings.recoveryKit.stolenLandedForAnother(actor: "ab12"))
    #expect(!vm.stolenFailedMessagePending)
}
