#if !FAUNA_KIDS
import Foundation
import Testing
@testable import FaunaKit

// admin.md § Errors & edge cases, *Controls wait for their read* — the apple
// leg of the rule tui led (`admin::action_possibility`). Every admin control
// that writes what a snapshot read decides is painted from the shared answer
// and its press is refused with the same reason on `error-message`, nothing
// reaching the nest. These cover the Swift seams with no nest: the page's own
// read (`PageRead`), the machine-absent answer, each page family's press
// refusal before its read, and — over a real shared machine — a failed NAT
// read never enabling save.

private var stillLoading: String? { readPossibility(read: .pending).refusal }
private var loadFailed: String? { readPossibility(read: .failed).refusal }

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

// MARK: - The shared Swift seams

@Test func aPageReadIsStillLoadingThenFailedThenKeepsAResolvedAnswer() {
    var read = PageRead()
    #expect(reasonKey(read.possibility) == "common.still_loading")
    read.failed()
    #expect(reasonKey(read.possibility) == "common.load_failed")
    read.resolved()
    #expect(read.possibility == .possible)
    // A re-read failing after one succeeded keeps the earlier answer.
    read.failed()
    #expect(read.possibility == .possible)
}

@Test func aMachineNotBuiltYetIsStillLoadingAndOneThatCannotBeIsFailed() {
    let none: BridgeApprovalMachine? = nil
    #expect(reasonKey(machinePossibility(none) { $0.confirmPossibility() }) == "common.still_loading")
    #expect(reasonKey(machinePossibility(none, buildFailed: true) { $0.confirmPossibility() })
            == "common.load_failed")
}

@Test func aWriteWaitsForItsReadThenForItsOwnChange() {
    #expect(reasonKey(writeAfterReadPossibility(read: .pending, idle: true)) == "common.still_loading")
    #expect(reasonKey(writeAfterReadPossibility(read: .failed, idle: true)) == "common.load_failed")
    #expect(reasonKey(writeAfterReadPossibility(read: .resolved, idle: false)) == "common.in_flight")
    #expect(writeAfterReadPossibility(read: .resolved, idle: true) == .possible)
}

// MARK: - Nest

@Test @MainActor func nestControlsRefuseBeforeTheReflectiveRead() async {
    let vm = AdminNestVM()
    #expect(reasonKey(vm.pairingPossibility) == "common.still_loading")
    #expect(reasonKey(vm.servingPortPossibility) == "common.still_loading")
    #expect(reasonKey(vm.natSaveAnswer) == "common.still_loading")
    await vm.setPairing(true)
    #expect(vm.errorMessage == stillLoading)
    vm.errorMessage = nil
    await vm.saveNatMode()
    #expect(vm.errorMessage == stillLoading)
    vm.errorMessage = nil
    vm.selectNatMode(.private)
    #expect(vm.errorMessage == stillLoading)
}

/// The shared machine over a nest that cannot answer: its `fauna.setup.status`
/// read fails, and neither the radios nor save go live on the pre-selected
/// default `public` — saving it would be a guess.
@Test(.timeLimit(.minutes(1)))
func aFailedNatReadNeverEnablesSave() async {
    let machine = AdminNatModeMachine(
        nestUrl: "https://nest.invalid", secretHex: String(repeating: "11", count: 32))
    #expect(reasonKey(machine.savePossibility()) == "common.still_loading")
    await machine.hydrate()
    #expect(reasonKey(machine.savePossibility()) == "common.load_failed")
    #expect(reasonKey(machine.selectPossibility()) == "common.load_failed")
}

// MARK: - Mail

@Test @MainActor func mailWritesRefuseBeforeThePolicyRead() async {
    let vm = AdminMailVM()
    #expect(reasonKey(vm.writeAnswer) == "common.still_loading")
    await vm.setMailEnabled(true)
    #expect(vm.errorMessage == stillLoading)
    vm.errorMessage = nil
    await vm.press(.publishSpamBaseline)
    #expect(vm.errorMessage == stillLoading)
}

// MARK: - Calendar / Contacts / Files

@Test @MainActor func davTogglesRefuseBeforeTheirRead() async {
    let calendar = AdminCalendarVM()
    #expect(reasonKey(calendar.toggleAnswer) == "common.still_loading")
    await calendar.setCaldavEnabled(true)
    #expect(calendar.errorMessage == stillLoading)
    calendar.errorMessage = nil
    await calendar.setCaldavPort(8443)
    #expect(calendar.errorMessage == stillLoading)

    let contacts = AdminContactsVM()
    await contacts.setCarddavEnabled(true)
    #expect(contacts.errorMessage == stillLoading)

    let files = AdminFilesVM()
    await files.setWebdavEnabled(true)
    #expect(files.errorMessage == stillLoading)

    // A machine that could not be built never reads: failed, not loading.
    files.machineBuildFailed = true
    #expect(reasonKey(files.toggleAnswer) == "common.load_failed")
    files.errorMessage = nil
    await files.setWebdavEnabled(true)
    #expect(files.errorMessage == loadFailed)
}

// MARK: - Aliases

@Test @MainActor func theForwarderFormRefusesBeforeItsRead() async {
    let vm = ForwardersVM()
    #expect(reasonKey(vm.addAnswer) == "common.still_loading")
    #expect(vm.listRead == .pending)
    await vm.create(localDomain: "example.test", pattern: "sales", forwardTarget: "out@example.net")
    #expect(vm.errorMessage == stillLoading)
}

// MARK: - Settings (tiers)

@Test @MainActor func theTierAddFormRefusesBeforeTheTierList() async {
    let vm = AdminTiersVM()
    #expect(reasonKey(vm.addTierPossibility) == "common.still_loading")
    let created = await vm.addTier(name: "gold", inbox: "1", storage: "1", devices: "1",
                                   blobSize: "1", feeds: "1")
    #expect(created == false)
    #expect(vm.errorMessage == stillLoading)
}

// MARK: - Bridges

@Test @MainActor func bridgeSectionsAreUnreadAndTheConfirmIsNotLiveBeforeTheRead() async {
    let vm = BridgeApprovalVM()
    #expect(vm.listRead == .pending)
    #expect(reasonKey(vm.confirmAnswer) == "common.still_loading")
    await vm.confirm()
    #expect(vm.errorMessage == stillLoading)
}
#endif
