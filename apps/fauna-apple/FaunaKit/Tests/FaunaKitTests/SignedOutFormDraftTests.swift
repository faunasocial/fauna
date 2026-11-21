import Foundation
import Testing
@testable import FaunaKit

// The signed-out forms' submits paint from the shared draft answers
// (`docs/goal/ui/README.md` § Offer only what is possible, rule 4's form case,
// rule 6) and their actions re-ask them. These pin the apple wiring — that each
// answer the views read is the shared one, blank, malformed and complete —
// beside tui's `every_signed_out_form_paints_its_submit_from_its_draft`; the
// predicates' own grammar is pinned in `fauna_onboarding_machine::drafts` and
// `fauna_client_recovery::ceremony`.
//
// Mutation check: putting back a view-model `isEmpty` gate (a blank import
// answered possible, a lowercase confirm word arming the stolen button) reds
// the matching test.

/// The reason key a blocked answer gives; `"<possible>"` / `"<absent>"` else.
private func key(_ p: Possibility) -> String {
    switch p {
    case .possible: return "<possible>"
    case .absent: return "<absent>"
    case .blocked(let reason): return reason.key
    }
}

// Built at run time: a 64-hex literal in a shipped file reads as a key to the
// publish secret scan.
private let secret = String(repeating: "01", count: 32)

@Test @MainActor func theImportSubmitAnswersFromTheSharedParse() {
    let vm = OnboardingVM()
    #expect(key(vm.importPossibility("")) == "onboarding.identity_import.invalid_secret")
    #expect(key(vm.importPossibility("not a key")) == "onboarding.identity_import.invalid_secret")
    #expect(vm.importPossibility(secret).isPossible)
    // Surrounding whitespace is the import's own trim, not a malformed paste.
    #expect(vm.importPossibility("  \(secret)\n").isPossible)
}

@Test @MainActor func theImportSubmitRefusesABlankPasteOutLoud() {
    let vm = OnboardingVM()
    vm.importIdentity("   ")
    #expect(vm.errorMessage == vm.importPossibility("").refusal)
    #expect(vm.errorMessage?.isEmpty == false)
}

@Test @MainActor func theRecoveryEntrySubmitNamesTheFirstMissingField() {
    let vm = OnboardingVM()
    let p = { (phrase: String, account: String) in
        key(vm.recoveryEntryAnswer(phrase: phrase, account: account, submitting: false))
    }
    #expect(p("", "alice@example.com") == "onboarding.recovery_entry.invalid_kit")
    #expect(p("nonsense", "") == "onboarding.recovery_entry.invalid_kit")
    #expect(p(secret, "") == "onboarding.recovery_entry.account_needed")
    #expect(p(secret, "alice") == "onboarding.recovery_entry.account_malformed")
    #expect(p(secret, "alice@example.com") == "<possible>")
    // A complete draft still waits for the restore already on its way.
    #expect(key(vm.recoveryEntryAnswer(phrase: secret, account: "alice@example.com", submitting: true))
            == "common.in_flight")
}

@Test @MainActor func theSignedOutLockChecksSecretThenAccountThenTheWord() {
    let vm = OnboardingVM()
    let p = { (s: String, a: String, c: String) in
        key(vm.emergencyLockoutAnswer(secret: s, account: a, confirm: c, submitting: false))
    }
    #expect(p("", "", "") == "onboarding.emergency_lockout.invalid_secret")
    #expect(p("zz", "a@b.example", "LOCK") == "onboarding.emergency_lockout.invalid_secret")
    #expect(p(secret, "", "LOCK") == "onboarding.emergency_lockout.account_needed")
    #expect(p(secret, "alice", "LOCK") == "onboarding.emergency_lockout.account_malformed")
    #expect(p(secret, "alice@example.com", "lock") == "sessions.lockout_wrong_word")
    #expect(p(secret, "alice@example.com", "LOCK") == "<possible>")
    #expect(key(vm.emergencyLockoutAnswer(
        secret: secret, account: "alice@example.com", confirm: "LOCK", submitting: true))
            == "common.in_flight")
}

@Test @MainActor func theCodeAndHandleSubmitsBlockABlankDraft() {
    let vm = OnboardingVM()
    #expect(key(vm.inviteCodePossibility("")) == "onboarding.invite_request.oob_idle")
    #expect(key(vm.inviteCodePossibility(" \t")) == "onboarding.invite_request.oob_idle")
    #expect(vm.inviteCodePossibility("abc-123").isPossible)

    #expect(key(vm.claimCodePossibility("")) == "onboarding.claim_code.idle")
    #expect(vm.claimCodePossibility("claim-abc").isPossible)

    vm.machine.setCurrentHandle(h: "")
    #expect(key(vm.handleCheckAnswer) == "onboarding.handle_check.idle")
    vm.machine.setCurrentHandle(h: "alice@example.com")
    #expect(vm.handleCheckAnswer.isPossible)
}

@Test @MainActor func theCodeAndHandleActionsRefuseABlankDraftOutLoud() {
    let vm = OnboardingVM()
    vm.checkOobCode("")
    #expect(vm.errorMessage == vm.inviteCodePossibility("").refusal)

    let fresh = OnboardingVM()
    fresh.machine.setCurrentHandle(h: "  ")
    fresh.runHandleCheck()
    #expect(fresh.errorMessage == fresh.handleCheckAnswer.refusal)
    #expect(fresh.errorMessage?.isEmpty == false)
}

@Test @MainActor func theStolenButtonAnswersKitFirstThenTheWord() {
    let vm = RecoveryKitVM()
    #expect(key(vm.stolenAnswer) == "settings.recovery_kit.kit_phrase_required")
    vm.phraseInput = "fauna://recovery?x"
    vm.stolenConfirmInput = "succeed"
    #expect(key(vm.stolenAnswer) == "settings.recovery_kit.stolen_confirm_placeholder")
    vm.stolenConfirmInput = "SUCCEED"
    #expect(vm.stolenAnswer.isPossible)

    let locked = LockedLaunchModel(lockedUntilSecs: 0)
    #expect(key(locked.answer) == "settings.recovery_kit.kit_phrase_required")
    locked.phrase = "fauna://recovery?x"
    #expect(key(locked.answer) == "settings.recovery_kit.stolen_confirm_placeholder")
    locked.confirm = "SUCCEED"
    #expect(locked.answer.isPossible)
}
