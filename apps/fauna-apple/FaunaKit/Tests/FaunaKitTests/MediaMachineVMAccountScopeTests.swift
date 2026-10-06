import Foundation
import Testing
@testable import FaunaKit

// `MediaMachineVM` at an account switch — `docs/goal/architecture/apps/account-scoping.md`
// § The scoping taxonomy, the in-memory corollary: a live machine holding one
// identity's data is account-scoped state, dropped at the identity change and keyed
// on the identity, and a build already in flight when the drop runs must not land on
// the next account.
//
// Why this VM and not its twenty-odd page-owned siblings: iOS keeps `MainTabView` —
// and the More stack's `moreSelectedView` — mounted across an in-process switch, so
// a More → Media page outlives it, and `MediaView`'s `.task` only re-fired on a nav
// patch. `configure` then assigned the NEW api before its `machine == nil` guard, so
// the page kept rendering account A's machine (its items and decrypted thumbnails)
// paired with account B's api. Silent by construction — stale media renders as a
// plausible library, never as an error — which is why it is pinned here.
//
// The machines are real but offline: built over a torn-down `FfiNestClient`, so a
// request through one fails at once and nothing dials.

/// An `APIClient` whose Media machine build is scripted.
private final class ScriptedMediaAPI: APIClient {
    enum Build {
        case machine
        case fails(String)
    }

    var build: Build
    /// Every machine this api built, in order.
    private(set) var built: [MediaMachine] = []
    var parkBuild = false
    let parking = ScopeTestParking()

    init(_ build: Build) {
        self.build = build
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func mediaMachine(observer: MediaObserver) async throws -> MediaMachine {
        await parking.parkIfAsked(parkBuild)
        switch build {
        case .machine:
            let machine = buildMediaMachine(nest: try await tornDownNest(), observer: observer)
            built.append(machine)
            return machine
        case .fails(let account):
            throw ScopeTestFailure(account: account)
        }
    }
}

@MainActor
private func configure(_ vm: MediaMachineVM, _ api: ScriptedMediaAPI) async {
    await vm.configure(api: api, deviceId: "device", predecessors: MediaPredecessors())
}

// ── The failed switch ───────────────────────────────────────────────────────

@Suite("MediaMachineVM at an account switch")
@MainActor
struct MediaMachineVMAccountScopeTests {
    @Test("a failed machine build after an account switch leaves the Media page empty, never on the previous account's machine")
    @MainActor
    func aFailedBuildAfterASwitchLeavesTheMediaPageEmpty() async throws {
        let apiA = ScriptedMediaAPI(.machine)
        let vm = MediaMachineVM()
        await configure(vm, apiA)
        try #require(vm.machine === apiA.built.first, "precondition: account A's machine is live")

        await configure(vm, ScriptedMediaAPI(.fails("account B")))  // account B's first connection fails

        #expect(vm.machine == nil, "the failed switch kept account A's machine")
        #expect(vm.snapshot == nil, "the Media page still renders account A's snapshot")
        #expect(vm.errorMessage?.contains("account B") == true,
                "the page must report account B's failure, not sit on account A's library")
    }

    @Test("a same-account retry after a failed build recovers and drops the stale connect error")
    @MainActor
    func retryingAFailedBuildForTheSameAccountRecovers() async throws {
        let apiB = ScriptedMediaAPI(.fails("account B"))
        let vm = MediaMachineVM()
        await configure(vm, apiB)  // first connection fails
        try #require(vm.machine == nil)
        try #require(vm.errorMessage != nil, "precondition: the failure is on screen")

        apiB.build = .machine
        await configure(vm, apiB)  // the retry succeeds

        #expect(vm.machine === apiB.built.first, "the retry did not build a machine")
        #expect(vm.errorMessage?.contains("account B") != true,
                "the first attempt's connect error outlived the successful retry")
    }

    // ── The ordinary switch and the same-account no-op ──────────────────────────

    @Test("a switch to another account rebuilds the machine over the new api")
    @MainActor
    func aSwitchRebuildsTheMachineOverTheNewApi() async throws {
        let apiA = ScriptedMediaAPI(.machine)
        let apiB = ScriptedMediaAPI(.machine)
        let vm = MediaMachineVM()
        await configure(vm, apiA)
        await configure(vm, apiB)

        #expect(apiB.built.count == 1, "account B's api was never asked for a machine")
        #expect(vm.machine === apiB.built.first, "the page is not on account B's own machine")
        #expect(vm.machine !== apiA.built.first, "the page is still on account A's machine")
    }

    @Test("configuring again with the same api keeps the machine and builds nothing")
    @MainActor
    func reconfiguringTheSameApiKeepsTheMachine() async throws {
        let apiA = ScriptedMediaAPI(.machine)
        let vm = MediaMachineVM()
        await configure(vm, apiA)
        let machine = vm.machine

        apiA.build = .fails("account A")  // an (incorrect) rebuild here would throw and lose the machine
        await configure(vm, apiA)

        #expect(apiA.built.count == 1)
        #expect(vm.machine === machine)
    }

    // ── A result already in flight (`account-scoping.md`'s in-flight clause) ────

    @Test("a machine build already in flight for the outgoing account does not land after the switch")
    @MainActor
    func aBuildInFlightForTheOutgoingAccountDoesNotLandAfterASwitch() async throws {
        let apiA = ScriptedMediaAPI(.machine)
        apiA.parkBuild = true
        let apiB = ScriptedMediaAPI(.machine)
        let vm = MediaMachineVM()

        let configuringA = Task { await configure(vm, apiA) }
        await apiA.parking.waitUntilParked()  // account A's build is suspended mid-await
        await configure(vm, apiB)  // the switch completes meanwhile
        try #require(vm.machine === apiB.built.first, "precondition: the switch landed account B's machine")

        apiA.parking.release()
        await configuringA.value

        #expect(vm.machine === apiB.built.first, "account A's late build result landed on account B's page")
    }

    @Test("a build failure already in flight for the outgoing account does not paint on the next account's page")
    @MainActor
    func aBuildFailureInFlightForTheOutgoingAccountDoesNotPaintAfterASwitch() async throws {
        let apiA = ScriptedMediaAPI(.fails("account A"))
        apiA.parkBuild = true
        let apiB = ScriptedMediaAPI(.machine)
        let vm = MediaMachineVM()

        let configuringA = Task { await configure(vm, apiA) }
        await apiA.parking.waitUntilParked()
        await configure(vm, apiB)
        apiA.parking.release()  // account A's build now throws
        await configuringA.value

        #expect(vm.errorMessage?.contains("account A") != true,
                "account A's connect failure is on account B's page")
        #expect(vm.machine === apiB.built.first)
    }

    // ── The reset seam the page's nil-client phase calls ────────────────────────

    @Test("reset drops the machine and every error the page holds for the outgoing account")
    @MainActor
    func resetDropsEverythingTheVMHoldsForTheOutgoingAccount() async throws {
        let vm = MediaMachineVM()
        await configure(vm, ScriptedMediaAPI(.machine))
        await vm.uploadFromPath("/no/such/dir/photo.jpg")  // a client-glue failure: the file cannot be read
        try #require(vm.machine != nil, "precondition: account A's machine is live")
        try #require(vm.glueError != nil, "precondition: a glue error is on the page")

        vm.reset()

        #expect(vm.machine == nil)
        #expect(vm.snapshot == nil)
        #expect(vm.glueError == nil, "account A's upload error is still on the page")
        #expect(vm.errorMessage == nil)
        #expect(await vm.fetchThumbnail(hash: "00") == nil, "a reset VM still served a thumbnail")
    }

    @Test("a machine build already in flight when the page is reset does not land afterwards")
    @MainActor
    func aBuildInFlightAtResetDoesNotLand() async throws {
        let apiA = ScriptedMediaAPI(.machine)
        apiA.parkBuild = true
        let vm = MediaMachineVM()

        let configuringA = Task { await configure(vm, apiA) }
        await apiA.parking.waitUntilParked()
        vm.reset()  // the switch's nil-client phase
        apiA.parking.release()
        await configuringA.value

        #expect(vm.machine == nil, "a build that finished after the reset landed the outgoing account's machine")
    }

    @Test("after a reset the same api is asked for a fresh machine")
    @MainActor
    func aResetVMRebuildsForTheSameApi() async throws {
        let apiA = ScriptedMediaAPI(.machine)
        let vm = MediaMachineVM()
        await configure(vm, apiA)
        vm.reset()

        await configure(vm, apiA)

        #expect(apiA.built.count == 2, "the reset VM kept serving the machine it had dropped")
        #expect(vm.machine === apiA.built.last)
    }
}
