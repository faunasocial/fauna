import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for `folders.md` § Errors & edge cases (the form case of
// `ui/README.md` § Offer only what is possible, rule 4) on the apple apps:
// `folder-follow-confirm` and `folder-share-confirm` paint from
// `fauna_client_folders::forms` over UniFFI, each verb asks the same predicate
// first and refuses with its reason on `error-message`, and the share answers
// false when it cannot run, so the sheet stays open with what was typed.

@MainActor
private func reason(_ answer: Possibility) throws -> String {
    let text = try #require(answer.reasonText, "expected a blocked answer with a reason")
    #expect(!text.isEmpty)
    return text
}

@Suite("Folder forms — painted from the shared predicates")
@MainActor
struct FolderFormPossibilityTests {
    @Test("folder-follow-confirm: blank handle, blank name and not-signed-in are each blocked with their own reason")
    func follow() throws {
        let vm = DevicesMachineVM()  // no machine, no client: not ready
        let noHandle = try reason(vm.folderFollowPossibility(handle: " ", name: "photos"))
        let noName = try reason(vm.folderFollowPossibility(handle: "alice", name: ""))
        let notReady = try reason(vm.folderFollowPossibility(handle: "alice", name: "photos"))
        #expect(Set([noHandle, noName, notReady]).count == 3)
    }

    @Test("a follow that cannot run refuses on error-message with the painted reason")
    func followRefuses() async throws {
        let vm = DevicesMachineVM()
        let why = try reason(vm.folderFollowPossibility(handle: "", name: "photos"))
        let ok = await vm.followFolder(owner: "", folderName: "photos")
        #expect(ok == false, "the follow form stays open")
        #expect(vm.errorMessage == why)
    }

    @Test("folder-share-confirm: blank recipient and no session are each blocked with their own reason")
    func share() throws {
        let vm = DevicesMachineVM()
        let blank = try reason(vm.folderSharePossibility(recipient: "  "))
        let notReady = try reason(vm.folderSharePossibility(recipient: "bob"))
        #expect(blank != notReady)
    }

    @Test("the share form stays open when it cannot run: the share answers false with the reason on error-message")
    func shareStaysOpen() async throws {
        let vm = DevicesMachineVM()
        let why = try reason(vm.folderSharePossibility(recipient: "bob"))
        let issued = await vm.shareFolder(name: "photos", recipientInput: "bob")
        #expect(issued == false, "the sheet dismisses only once the share is issued")
        #expect(vm.errorMessage == why)
    }
}
