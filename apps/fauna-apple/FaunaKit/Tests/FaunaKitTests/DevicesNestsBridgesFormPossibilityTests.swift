import Foundation
import Testing
@testable import FaunaKit

// The devices, nests and bridges forms on the apple apps (`docs/goal/ui/
// devices.md`, `docs/goal/ui/nests.md`, `docs/goal/behavior/bridges.md`
// § Errors & edge cases, the 2026-11-17 rulings; `ui/README.md` § Offer only
// what is possible, rules 4, 6 and 7): each submit paints the shared draft
// answer carried over UniFFI, its press asks the same answer and refuses out
// loud, and a form closes only on success. One suite for macos and ios: both
// read the one FaunaKit view model or view.

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

@Suite @MainActor struct DevicesNestsBridgesFormPossibilityTests {
    // MARK: devices — rename and the custody mint

    @Test func withNoMachineTheRenameIsAbsentAndTheSaveKeepsTheEditorOpen() async {
        let vm = DevicesMachineVM()
        #expect(vm.renamePossibility(deviceId: "dev", draft: "Laptop") == .absent)
        // `false` = the save did not go through: the editor stays open.
        #expect(await vm.renameDevice(deviceId: "dev", label: "Laptop") == false)
    }

    @Test func theCustodyMintEntryIsWaitingUntilReadThenNamesTheStep() {
        #expect(reasonKey(DevicesMachineVM().mintEntry) == "common.still_loading")
        #expect(reasonKey(custodyMintPossibility(candidates: nil)) == "common.still_loading")
        #expect(reasonKey(custodyMintPossibility(candidates: [])) == "devices.custody_mint_no_contacts")
        let one = CustodyMintCandidateView(host: Data(repeating: 1, count: 32), channelHex: "00", label: "alice")
        #expect(custodyMintPossibility(candidates: [one]) == .possible)
    }

    @Test func theCustodyMintConfirmIsBlockedUntilAHostIsPicked() {
        #expect(reasonKey(custodyMintConfirmPossibility(hostPicked: false)) == "devices.custody_mint_pick_host")
        #expect(custodyMintConfirmPossibility(hostPicked: true) == .possible)
    }

    @Test func aMintPressBeforeTheReadRefusesWithItsReason() async {
        let vm = DevicesMachineVM()
        #expect(await vm.openCustodyMint() == nil)
        #expect(vm.sharingError == DevicesMachineVM().mintEntry.refusal)
    }

    // MARK: nests — add and mint

    @Test func theAddSubmitIsBlockedOnABlankDraft() {
        #expect(reasonKey(linkInputPossibility(raw: "")) == "nests.add_input_blank")
        #expect(reasonKey(linkInputPossibility(raw: "   ")) == "nests.add_input_blank")
        #expect(linkInputPossibility(raw: "https://nest.example.test").isPossible)
    }

    @Test func theMintIsBlockedUntilAUseThenItsHolderIsPicked() {
        #expect(reasonKey(mintDraftPossibility(option: nil, pickedHolder: nil)) == "nests.mint_pick_scope")
        let shared = TrustMintOption(useCase: .mail, tier: nil, scope: [], holderCandidates: ["a", "b"])
        #expect(reasonKey(mintDraftPossibility(option: shared, pickedHolder: nil)) == "nests.mint_pick_holder")
        #expect(mintDraftPossibility(option: shared, pickedHolder: "b") == .possible)
        #expect(mintHolder(option: shared, picked: "b") == "b")
        let single = TrustMintOption(useCase: .mail, tier: nil, scope: [], holderCandidates: ["only"])
        #expect(mintDraftPossibility(option: single, pickedHolder: nil) == .possible)
        #expect(mintHolder(option: single, picked: nil) == "only")
    }

    // MARK: bridges — follow and plugin install

    @Test func theFollowIsBlockedOnABlankIdAndItsPressRefusesOutLoud() async {
        let vm = BridgeManagerVM()
        #expect(reasonKey(vm.followAnswer) == "bridges.follow_id_required")
        await vm.addFollow(bridgeId: "bluesky")
        #expect(vm.errorMessage == vm.followAnswer.refusal)
        #expect(vm.errorMessage?.isEmpty == false)
        vm.followId = "did:plc:someone"
        #expect(vm.followAnswer == .possible)
    }

    @Test func thePluginInstallIsBlockedOnABlankUrlAndItsPressRefusesOutLoud() async {
        let vm = PluginAdminVM()
        #expect(reasonKey(vm.installAnswer) == "bridges.plugin_install_url_required")
        await vm.install()
        #expect(vm.errorMessage == vm.installAnswer.refusal)
        #expect(vm.errorMessage?.isEmpty == false)
        vm.installUrl = "https://plugin.example.test/doc.json"
        #expect(vm.installAnswer == .possible)
    }
}
