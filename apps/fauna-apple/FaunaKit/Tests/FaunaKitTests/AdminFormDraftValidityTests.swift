#if !FAUNA_KIDS
import Foundation
import Testing
@testable import FaunaKit

// admin.md § Errors & edge cases, *A form's submit is live only for a draft the
// nest would accept* — the apple leg of the rule tui led (its
// `admin::action_possibility` draft fold). Each admin form's submit is painted
// from a shared draft answer, folded after the page's read gate, and its press
// is refused with the same reason on the page's error line, nothing reaching
// the nest. These cover the Swift seams with no nest: each VM's draft answer
// and the press refusal it drives.

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

private var stillLoading: String { "common.still_loading" }

// MARK: - Users

@Test @MainActor func theRegistrationSaveTakesABlankOrWholeCeilingAndRefusesAnythingElse() async {
    let vm = AdminVM()
    #expect(vm.registrationCeilingAnswer("") == .possible)
    #expect(vm.registrationCeilingAnswer(" 25 ") == .possible)
    let bad = vm.registrationCeilingAnswer("lots")
    #expect(reasonKey(bad) == "admin.users_page.max_free_users_invalid")
    await vm.saveRegistration(mode: .open, maxFreeUsersInput: "lots")
    #expect(vm.actionError == bad.refusal)
}

@Test @MainActor func theAdmitButtonAsksTheTierSelectFirstThenTheAccountKey() async {
    let vm = AdminVM()
    // No tiers read yet: the tier select's reason stands before the key's.
    let answer = vm.admitAnswer("not-a-key")
    #expect(answer == vm.tierPickAnswer)
    await vm.admitUser(actorHex: "not-a-key", handle: "", tier: "free")
    #expect(vm.actionError == answer.refusal)
    // The key's own answer: exactly 64 hex characters.
    #expect(reasonKey(adminAdmitActorPossibility(raw: "abc")) == "admin.users_page.admit_actor_hint")
    #expect(adminAdmitActorPossibility(raw: String(repeating: "a", count: 64)) == .possible)
}

// MARK: - Settings (tiers)

@Test @MainActor func aTierRowSaveRefusesACapThatDoesNotParseAndNeverSendsTheStoredOne() async {
    let vm = AdminTiersVM()
    #expect(AdminTiersVM.tierCapsAnswer(["1", "2", "3", "4", "5"]) == .possible)
    let bad = AdminTiersVM.tierCapsAnswer(["1", "", "3", "4", "5"])
    #expect(reasonKey(bad) == "admin.settings_page.save_tier_error_invalid_cap")
    await vm.saveTier(name: "free", caps: ["1", "", "3", "4", "5"])
    #expect(vm.errorMessage == bad.refusal)
}

@Test @MainActor func theAddATierFormAsksItsReadBeforeItsDrafts() {
    let vm = AdminTiersVM()
    // Before the tier list: still loading, whatever the drafts say.
    #expect(reasonKey(vm.addTierAnswer(name: "", caps: ["x", "x", "x", "x", "x"])) == stillLoading)
    // The drafts' own answer needs a name and five caps that parse.
    #expect(reasonKey(adminTierCreatePossibility(name: "", caps: ["1", "1", "1", "1", "1"])) != nil)
    #expect(adminTierCreatePossibility(name: "gold", caps: ["1", "1", "1", "1", "1"]) == .possible)
}

@Test @MainActor func aMembershipSaveWithNoAdmittedAtTierIsRefused() async {
    let vm = AdminTiersVM()
    let bad = AdminTiersVM.membershipSaveAnswer(adminTier: "")
    #expect(reasonKey(bad) == "admin.settings_page.save_membership_tier_error_no_tier")
    #expect(AdminTiersVM.membershipSaveAnswer(adminTier: "free") == .possible)
    await vm.saveMembershipTier(tierName: "patron", adminTier: "", lapseTier: "free")
    #expect(vm.errorMessage == bad.refusal)
}

// MARK: - Nest

@Test @MainActor func theServingPortSaveAsksTheReadFirstThenThePort() {
    let vm = AdminNestVM()
    #expect(reasonKey(vm.servingPortSaveAnswer("0")) == stillLoading)
    #expect(reasonKey(adminServingPortPossibility(raw: "0")) == "admin.nest_page.serving_port_invalid")
    #expect(reasonKey(adminServingPortPossibility(raw: "70000")) == "admin.nest_page.serving_port_invalid")
    #expect(adminServingPortPossibility(raw: "8443") == .possible)
}

@Test @MainActor func theRegionSaveRefusesACodeTheParserRejects() {
    let vm = AdminNestVM()
    #expect(reasonKey(vm.regionSaveAnswer("not a region")) != nil)
}

// MARK: - Calendar

@Test @MainActor func theCaldavPortSaveAsksTheReadFirstThenThePort() {
    let vm = AdminCalendarVM()
    #expect(reasonKey(vm.portSaveAnswer("0")) == stillLoading)
    #expect(reasonKey(caldavPortDraftPossibility(draft: "0")) != nil)
    #expect(caldavPortDraftPossibility(draft: "8008") == .possible)
}

// MARK: - Mail

@Test func aMailGroupDraftThatDoesNotParseIsBlockedBlankIncluded() {
    #expect(mailKnobsDraftPossibility(counts: ["5", "10"], countsU64: ["1024"], lists: ["60\n300"])
            == .possible)
    #expect(reasonKey(mailKnobsDraftPossibility(counts: ["5", ""], countsU64: [], lists: []))
            == "admin.mail_page.knob_invalid")
    #expect(reasonKey(mailKnobsDraftPossibility(counts: [], countsU64: [], lists: ["60\nsoon"]))
            == "admin.mail_page.knob_list_invalid")
}
#endif
