import Foundation
import Testing
@testable import FaunaKit

// `BackupsMachineVM` at an account switch — `docs/goal/architecture/apps/account-scoping.md`
// § The scoping taxonomy, the in-memory corollary: a live machine, and the four
// non-machine reads that sit beside it (device chips, repo stats, the snapshot diff,
// the sharing error) plus the immediate-delete friction bar's typed inputs, are
// account-scoped state, dropped at the identity change and keyed on the identity, and
// a read already in flight when the drop runs must not land on the next account
// .
//
// Reachable on iOS on two routes, neither unmounted by a switch: More → Backups (the
// More stack's `moreSelectedView` survives `tearDownSessionForSwitch`) and
// Settings → Backups, a closure-destination `NavigationLink` that the
// `selectedSettingsPage` reset does not pop. `configure` assigned the NEW api before
// its `machine == nil` guard, so the page kept account A's snapshots and folder names
// paired with account B's api — plausible data, never an error.
//
// The machines are real but offline (a torn-down `FfiNestClient`), so nothing dials.

/// An `APIClient` whose Backups machine build and non-machine reads are scripted.
private final class ScriptedBackupsAPI: APIClient {
    enum Build {
        case machine
        case fails(String)
    }

    var build: Build
    private(set) var built: [BackupsMachine] = []
    var parkBuild = false
    let buildParking = ScopeTestParking()

    /// What `listFolderDevices` returns, and whether it parks first.
    var devices: [DeviceInfo] = []
    var parkDevices = false
    let devicesParking = ScopeTestParking()

    /// What `repoStats` does, and whether it parks first.
    var stats: Result<RepoStatsResponse, ScopeTestFailure> = .failure(ScopeTestFailure(account: "unscripted"))
    var parkStats = false
    let statsParking = ScopeTestParking()

    init(_ build: Build) {
        self.build = build
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func backupsMachine(observer: BackupsObserver) async throws -> BackupsMachine {
        await buildParking.parkIfAsked(parkBuild)
        switch build {
        case .machine:
            let machine = buildBackupsMachine(nest: try await tornDownNest(), observer: observer)
            built.append(machine)
            return machine
        case .fails(let account):
            throw ScopeTestFailure(account: account)
        }
    }

    override func listFolderDevices(name: String) async throws -> [DeviceInfo] {
        await devicesParking.parkIfAsked(parkDevices)
        return devices
    }

    override func repoStats(folder: String) async throws -> RepoStatsResponse {
        await statsParking.parkIfAsked(parkStats)
        return try stats.get()
    }
}

private func repoStats(folder: String) -> RepoStatsResponse {
    RepoStatsResponse(
        folder: folder, snapshotCount: 3, totalFiles: 9, rawSizeBytes: 100,
        storedSizeBytes: 50, dedupRatio: 2.0, storageBackend: "local")
}

private func device(_ id: String) -> DeviceInfo {
    DeviceInfo(deviceId: id, label: id, lastChangeAt: nil, changeCount: 1)
}

@MainActor
private func configure(_ vm: BackupsMachineVM, _ api: ScriptedBackupsAPI) async {
    await vm.configure(api: api, deviceIdHex: nil)
}

// ── The failed switch ───────────────────────────────────────────────────────

@Suite("BackupsMachineVM at an account switch")
@MainActor
struct BackupsMachineVMAccountScopeTests {
    @Test("a failed machine build after an account switch leaves the Backups page empty, never on the previous account's machine")
    @MainActor
    func aFailedBuildAfterASwitchLeavesTheBackupsPageEmpty() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        let vm = BackupsMachineVM()
        await configure(vm, apiA)
        try #require(vm.machine === apiA.built.first, "precondition: account A's machine is live")

        await configure(vm, ScriptedBackupsAPI(.fails("account B")))  // account B's first connection fails

        #expect(vm.machine == nil, "the failed switch kept account A's machine")
        #expect(vm.snapshot == nil, "the Backups page still renders account A's snapshot")
        #expect(vm.errorMessage?.contains("account B") == true,
                "the page must report account B's failure, not sit on account A's snapshots")
    }

    // ── The switch drops everything account A owned, machine or not ─────────────

    @Test("a switch drops the previous account's device chips, repo stats, sharing error and typed delete inputs")
    @MainActor
    func aSwitchDropsTheNonMachineStateToo() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        apiA.devices = [device("device-of-account-a")]
        apiA.stats = .success(repoStats(folder: "account A's folder"))
        let vm = BackupsMachineVM()
        await configure(vm, apiA)
        await vm.loadDevices(folder: "account A's folder")
        await vm.loadStats(folder: "account A's folder")
        apiA.stats = .failure(ScopeTestFailure(account: "account A"))
        await vm.loadStats(folder: "account A's folder")  // a failure now: the sharing error is on the page
        vm.selectedDeviceId = "device-of-account-a"
        vm.openImmediateDelete(snapshotId: 7)
        vm.immediateDeleteConfirmId = "7"
        vm.immediateDeleteAcknowledge = "account A's typed acknowledgement"
        try #require(!vm.devices.isEmpty && vm.stats != nil && vm.sharingError != nil,
                     "precondition: account A's non-machine state is populated")

        await configure(vm, ScriptedBackupsAPI(.machine))  // the switch

        #expect(vm.devices.isEmpty, "account A's device chips are on account B's page")
        #expect(vm.selectedDeviceId == nil, "account A's device filter is hiding account B's rows")
        #expect(vm.stats == nil, "account A's repo stats are on account B's page")
        #expect(vm.diffResult == nil)
        #expect(vm.sharingError == nil, "account A's sharing error is on account B's page")
        #expect(vm.immediateDeleteSnapshotId == nil, "account A's delete modal is open on account B's page")
        #expect(vm.immediateDeleteConfirmId.isEmpty)
        #expect(vm.immediateDeleteAcknowledge.isEmpty)
        #expect(vm.immediateDeleteAck.isEmpty)
    }

    // ── The ordinary switch and the same-account no-op ──────────────────────────

    @Test("a switch to another account rebuilds the machine over the new api")
    @MainActor
    func aSwitchRebuildsTheMachineOverTheNewApi() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        let apiB = ScriptedBackupsAPI(.machine)
        let vm = BackupsMachineVM()
        await configure(vm, apiA)
        await configure(vm, apiB)

        #expect(apiB.built.count == 1, "account B's api was never asked for a machine")
        #expect(vm.machine === apiB.built.first)
        #expect(vm.machine !== apiA.built.first, "the page is still on account A's machine")
    }

    @Test("configuring again with the same api keeps the machine and what the user typed")
    @MainActor
    func reconfiguringTheSameApiKeepsTheMachineAndTheTypedInputs() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        let vm = BackupsMachineVM()
        await configure(vm, apiA)
        let machine = vm.machine
        vm.openImmediateDelete(snapshotId: 7)
        vm.immediateDeleteConfirmId = "7"

        apiA.build = .fails("account A")  // an (incorrect) rebuild here would throw and lose the machine
        await configure(vm, apiA)  // a Backups-page remount

        #expect(apiA.built.count == 1)
        #expect(vm.machine === machine)
        #expect(vm.immediateDeleteSnapshotId == 7, "a same-account remount closed the delete modal")
        #expect(vm.immediateDeleteConfirmId == "7", "a same-account remount dropped what the user typed")
    }

    // ── A result already in flight (`account-scoping.md`'s in-flight clause) ────

    @Test("a machine build already in flight for the outgoing account does not land after the switch")
    @MainActor
    func aBuildInFlightForTheOutgoingAccountDoesNotLandAfterASwitch() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        apiA.parkBuild = true
        let apiB = ScriptedBackupsAPI(.machine)
        let vm = BackupsMachineVM()

        let configuringA = Task { await configure(vm, apiA) }
        await apiA.buildParking.waitUntilParked()
        await configure(vm, apiB)
        try #require(vm.machine === apiB.built.first, "precondition: the switch landed account B's machine")

        apiA.buildParking.release()
        await configuringA.value

        #expect(vm.machine === apiB.built.first, "account A's late build result landed on account B's page")
    }

    @Test("a device read already in flight for the outgoing account does not land after the switch")
    @MainActor
    func aDeviceReadInFlightDoesNotLandAfterASwitch() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        apiA.devices = [device("device-of-account-a")]
        apiA.parkDevices = true
        let vm = BackupsMachineVM()
        await configure(vm, apiA)

        let loading = Task { await vm.loadDevices(folder: "account A's folder") }
        await apiA.devicesParking.waitUntilParked()
        await configure(vm, ScriptedBackupsAPI(.machine))  // the switch completes meanwhile
        apiA.devicesParking.release()
        await loading.value

        #expect(vm.devices.isEmpty, "account A's late device read landed on account B's page")
    }

    @Test("a stats failure already in flight for the outgoing account does not paint on the next account's page")
    @MainActor
    func aStatsFailureInFlightDoesNotPaintAfterASwitch() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        apiA.stats = .failure(ScopeTestFailure(account: "account A"))
        apiA.parkStats = true
        let vm = BackupsMachineVM()
        await configure(vm, apiA)

        let loading = Task { await vm.loadStats(folder: "account A's folder") }
        await apiA.statsParking.waitUntilParked()
        await configure(vm, ScriptedBackupsAPI(.machine))
        apiA.statsParking.release()  // account A's read now throws
        await loading.value

        #expect(vm.sharingError == nil, "account A's failure is on account B's page")
        #expect(vm.errorMessage?.contains("account A") != true)
    }

    // ── The reset seam the page's nil-client phase calls ────────────────────────

    @Test("reset drops the machine and everything else the page holds for the outgoing account")
    @MainActor
    func resetDropsEverythingTheVMHoldsForTheOutgoingAccount() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        apiA.devices = [device("device-of-account-a")]
        apiA.stats = .success(repoStats(folder: "account A's folder"))
        let vm = BackupsMachineVM()
        await configure(vm, apiA)
        await vm.loadDevices(folder: "account A's folder")
        await vm.loadStats(folder: "account A's folder")
        vm.openImmediateDelete(snapshotId: 7)
        try #require(vm.machine != nil && !vm.devices.isEmpty && vm.stats != nil)

        vm.reset()

        #expect(vm.machine == nil)
        #expect(vm.snapshot == nil)
        #expect(vm.devices.isEmpty)
        #expect(vm.stats == nil)
        #expect(vm.immediateDeleteSnapshotId == nil)
        #expect(vm.errorMessage == nil)
        // The api is gone too: a later read must reach nobody.
        await vm.loadStats(folder: "account A's folder")
        #expect(vm.stats == nil, "a reset VM still drove account A's api")
    }

    @Test("a machine build already in flight when the page is reset does not land afterwards")
    @MainActor
    func aBuildInFlightAtResetDoesNotLand() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        apiA.parkBuild = true
        let vm = BackupsMachineVM()

        let configuringA = Task { await configure(vm, apiA) }
        await apiA.buildParking.waitUntilParked()
        vm.reset()  // the switch's nil-client phase
        apiA.buildParking.release()
        await configuringA.value

        #expect(vm.machine == nil, "a build that finished after the reset landed the outgoing account's machine")
    }

    @Test("after a reset the same api is asked for a fresh machine")
    @MainActor
    func aResetVMRebuildsForTheSameApi() async throws {
        let apiA = ScriptedBackupsAPI(.machine)
        let vm = BackupsMachineVM()
        await configure(vm, apiA)
        vm.reset()

        await configure(vm, apiA)

        #expect(apiA.built.count == 2, "the reset VM kept serving the machine it had dropped")
        #expect(vm.machine === apiA.built.last)
    }
}
