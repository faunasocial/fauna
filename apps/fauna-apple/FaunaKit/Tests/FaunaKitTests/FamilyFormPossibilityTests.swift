#if !FAUNA_KIDS
import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for `family-safety.md` § Errors & edge cases (the form case of
// `ui/README.md` § Offer only what is possible, rule 4) on the apple apps: the
// guardian's contact-add, transfer and policy-save submits paint from
// `fauna_client_family::forms` over UniFFI, each verb asks the same predicate
// first and refuses with the same reason on `error-message`, and the Family
// surface gate is the shared `has_family_surface`.

private let validActorHex = String(repeating: "ab", count: 32)

@MainActor
private func reason(_ answer: Possibility) throws -> String {
    let text = try #require(answer.reasonText, "expected a blocked answer with a reason")
    #expect(!text.isEmpty)
    return text
}

@Suite("Family forms — painted from the shared predicates")
@MainActor
struct FamilyFormPossibilityTests {
    @Test("family-contact-add-button: blank and malformed are blocked, each with its own reason; a hex actor id is possible")
    func contactAdd() throws {
        let vm = FamilyVM()
        let blank = try reason(vm.contactAddPossibility)
        vm.contactAddInput = "not-hex"
        let malformed = try reason(vm.contactAddPossibility)
        #expect(blank != malformed)
        vm.contactAddInput = validActorHex
        #expect(vm.contactAddPossibility.isPossible)
    }

    @Test("a malformed contact refuses on error-message with the painted reason, and keeps the box")
    func contactAddRefuses() async throws {
        let vm = FamilyVM()
        vm.contactAddInput = "zz"
        let why = try reason(vm.contactAddPossibility)
        await vm.addContact()
        #expect(vm.errorMessage == why)
        #expect(vm.contactAddInput == "zz")
    }

    @Test("family-transfer-button: its own reasons, never the contact-add words")
    func transferOwnReasons() async throws {
        let vm = FamilyVM()
        let blank = try reason(vm.transferPossibility)
        #expect(blank != (try reason(vm.contactAddPossibility)))
        vm.transferInput = "nope"
        let malformed = try reason(vm.transferPossibility)
        await vm.submitTransfer()
        #expect(vm.errorMessage == malformed)
        vm.transferInput = validActorHex
        #expect(vm.transferPossibility.isPossible)
    }

    @Test("family-policy-save-button: unset cells are possible; an unparseable cell is blocked and refused with the rule's reason")
    func policySave() async throws {
        let vm = FamilyVM()
        #expect(vm.policySavePossibility.isPossible, "empty cells mean unset")
        vm.draftScreenWindowStart = "25:99"
        let why = try reason(vm.policySavePossibility)
        await vm.savePolicy()
        #expect(vm.errorMessage == why)
    }

    @Test("the Family surface gate is the shared rule")
    func hasSurface() {
        #expect(!familyHasSurface(supervised: false, wards: 0, incomingTransfers: 0))
        #expect(familyHasSurface(supervised: true, wards: 0, incomingTransfers: 0))
        #expect(familyHasSurface(supervised: false, wards: 1, incomingTransfers: 0))
        #expect(familyHasSurface(supervised: false, wards: 0, incomingTransfers: 1))
    }
}
#endif
