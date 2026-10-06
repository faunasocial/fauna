import Foundation
import Testing
@testable import FaunaKit

// `SearchVM` at an account switch — `docs/goal/architecture/apps/account-scoping.md`
// § The scoping taxonomy, the in-memory corollary: a manager instance is
// account-scoped state, dropped at the identity change and keyed on the identity,
// and a build already in flight when the drop runs must not land on the next
// account. Silent by construction —
// stale search state renders as *plausible results*, never as an error — which is
// why it is pinned here rather than inspected.
//
// The manager is real. `FfiNestClient.new` "does not open the socket" and
// `search_manager()` is infallible, so an offline `FfiSearchManager` needs no dial;
// the api is an `APIClient` double whose `searchManager()` and
// `attachLocalSearchIndex(manager:)` are scripted, the same `RecordingAPI` shape
// `DevicesEnrollmentNoticeTests` uses. No test below runs a query through a real
// manager, so nothing here dials `nest.invalid`.
//
// The in-flight windows are made deterministic with two `AsyncStream`s — the double
// says when it has reached a call, the test says when that call may return — so
// there is no sleep and no wall-clock assertion (e2e-conventions.md convention 14).

private struct ScriptedBuildFailure: Error {}

/// A real `FfiSearchManager`, built without a connection.
private func offlineManager() throws -> FfiSearchManager {
    try FfiNestClient(nestUrl: "https://nest.invalid", secret: Data(repeating: 7, count: 32))
        .searchManager()
}

/// An `APIClient` whose manager build and local-index attach are scripted.
private final class ScriptedSearchAPI: APIClient {
    enum Build {
        case manager(FfiSearchManager)
        case fails
    }

    var build: Build
    /// What `attachLocalSearchIndex` reports — `false` is the ordinary "no mail
    /// yet" state, which leaves the attachment open for a later retry.
    var attachSucceeds = true
    /// Every manager `attachLocalSearchIndex` was asked to register, in order.
    private(set) var attached: [FfiSearchManager] = []

    /// Park the named call after it announces itself, until `release()`.
    var parkBuild = false
    var parkAttach = false

    private let entered: AsyncStream<Void>
    private let enteredSignal: AsyncStream<Void>.Continuation
    private let released: AsyncStream<Void>
    private let releaseSignal: AsyncStream<Void>.Continuation

    init(_ build: Build) {
        self.build = build
        let entering = AsyncStream.makeStream(of: Void.self)
        self.entered = entering.stream
        self.enteredSignal = entering.continuation
        let releasing = AsyncStream.makeStream(of: Void.self)
        self.released = releasing.stream
        self.releaseSignal = releasing.continuation
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    /// Suspends until a parked call has reached its suspension point.
    func waitUntilParked() async {
        for await _ in entered { return }
    }

    /// Lets the parked call return.
    func release() {
        releaseSignal.yield()
    }

    private func parkIfAsked(_ park: Bool) async {
        guard park else { return }
        enteredSignal.yield()
        for await _ in released { return }
    }

    override func searchManager() async throws -> FfiSearchManager {
        await parkIfAsked(parkBuild)
        switch build {
        case .manager(let manager): return manager
        case .fails: throw ScriptedBuildFailure()
        }
    }

    override func attachLocalSearchIndex(manager: FfiSearchManager) async -> Bool {
        attached.append(manager)
        await parkIfAsked(parkAttach)
        return attachSucceeds
    }
}

// ── The failed switch ───────────────────────────────────────────────────────

/// The headline. `configure` assigned the new account's api and then kept the
/// previous account's manager, observer, results and attachment whenever the new
/// account's first build threw. Mutation check — restoring the keep-the-old-manager
/// catch (deleting the drop that precedes the build) turns the first four
/// expectations red.
@Test("a failed manager build after an account switch leaves the Search page empty, never on the previous account's manager")
@MainActor
func aFailedBuildAfterASwitchLeavesTheSearchPageEmpty() async throws {
    let managerA = try offlineManager()
    let vm = SearchVM()
    await vm.configure(api: ScriptedSearchAPI(.manager(managerA)))
    try #require(vm.manager === managerA, "precondition: account A's manager is live")
    vm.query = "account A's private query"

    await vm.configure(api: ScriptedSearchAPI(.fails))  // account B's first connection fails

    #expect(vm.manager == nil, "the failed switch kept account A's manager")
    #expect(vm.snapshot == nil, "the Search page still renders account A's snapshot")
    #expect(vm.results.isEmpty)
    #expect(vm.query.isEmpty, "account A's typed query is still in account B's search field")

    // Only reached once the drop held — on the old behaviour this would fire a
    // real query through account A's manager and dial the network.
    try #require(vm.manager == nil)
    vm.query = "anything"
    await vm.search()
    #expect(managerA.snapshot().query.isEmpty,
            "a search after the failed switch ran through account A's manager")
}

/// The sibling the finding did not state: the failed switch left the NEW api
/// paired with the OLD manager, so the reconnect / search-submit attach retry
/// registered account B's local index on account A's manager.
@Test("after a failed switch build, the new account's api is never asked to register a local index on the previous account's manager")
@MainActor
func aFailedBuildAfterASwitchNeverPairsTheNewApiWithTheOldManager() async throws {
    let apiA = ScriptedSearchAPI(.manager(try offlineManager()))
    apiA.attachSucceeds = false  // account A has no mail yet, so its attachment is still open
    let vm = SearchVM()
    await vm.configure(api: apiA)

    let apiB = ScriptedSearchAPI(.fails)
    await vm.configure(api: apiB)
    await vm.attachLocalIndexIfNeeded()  // the `.onReconnect` / search-submit retry

    #expect(apiB.attached.isEmpty,
            "account B's api was handed account A's manager to attach its local index to")
}

/// Regression guard for the drop's scope: it fires on an identity CHANGE, never on
/// a retry for the same account, or the `.task(id:)` re-run after a failed first
/// build would wipe what the user has typed meanwhile.
@Test("retrying a failed build for the same account keeps what the user typed")
@MainActor
func retryingAFailedBuildForTheSameAccountKeepsTheTypedQuery() async throws {
    let managerB = try offlineManager()
    let apiB = ScriptedSearchAPI(.fails)
    let vm = SearchVM()
    await vm.configure(api: apiB)  // first connection fails
    vm.query = "still typing"

    apiB.build = .manager(managerB)
    await vm.configure(api: apiB)  // the retry succeeds

    #expect(vm.manager === managerB)
    #expect(vm.query == "still typing", "a same-account retry dropped the typed query")
}

// ── The ordinary switch and the same-account no-op ──────────────────────────

@Test("a switch to another account rebuilds over the new api and drops the previous account's typed query")
@MainActor
func aSwitchRebuildsOverTheNewApiAndDropsTheTypedQuery() async throws {
    let managerB = try offlineManager()
    let vm = SearchVM()
    await vm.configure(api: ScriptedSearchAPI(.manager(try offlineManager())))
    vm.query = "account A's private query"

    let apiB = ScriptedSearchAPI(.manager(managerB))
    await vm.configure(api: apiB)

    #expect(vm.manager === managerB)
    #expect(vm.query.isEmpty, "account A's typed query survived into account B's page")
    #expect(apiB.attached.count == 1 && apiB.attached.first === managerB,
            "account B's local index must be registered on account B's own manager")
}

@Test("configuring again with the same api keeps the manager and the typed query")
@MainActor
func reconfiguringTheSameApiKeepsTheManagerAndTheQuery() async throws {
    let managerA = try offlineManager()
    let apiA = ScriptedSearchAPI(.manager(managerA))
    let vm = SearchVM()
    await vm.configure(api: apiA)
    vm.query = "typing"

    apiA.build = .fails  // an (incorrect) rebuild here would throw and lose the manager
    await vm.configure(api: apiA)

    #expect(vm.manager === managerA)
    #expect(vm.query == "typing")
}

// ── A result already in flight (`account-scoping.md`'s in-flight clause) ────

@Test("a manager build already in flight for the outgoing account does not land after the switch")
@MainActor
func aBuildInFlightForTheOutgoingAccountDoesNotLandAfterASwitch() async throws {
    let managerA = try offlineManager()
    let managerB = try offlineManager()
    let apiA = ScriptedSearchAPI(.manager(managerA))
    apiA.parkBuild = true
    let vm = SearchVM()

    let configuringA = Task { await vm.configure(api: apiA) }
    await apiA.waitUntilParked()  // account A's build is suspended mid-await
    await vm.configure(api: ScriptedSearchAPI(.manager(managerB)))  // the switch completes meanwhile
    try #require(vm.manager === managerB, "precondition: the switch landed account B's manager")

    apiA.release()
    await configuringA.value

    #expect(vm.manager === managerB, "account A's late build result landed on account B's page")
}

@Test("a local-index attach already in flight for the outgoing account does not mark the next account's manager attached")
@MainActor
func anAttachInFlightForTheOutgoingAccountDoesNotMarkTheNextAccountAttached() async throws {
    let apiA = ScriptedSearchAPI(.manager(try offlineManager()))
    apiA.parkAttach = true
    let apiB = ScriptedSearchAPI(.manager(try offlineManager()))
    apiB.attachSucceeds = false  // account B has no mail yet — its attachment is still open
    let vm = SearchVM()

    let configuringA = Task { await vm.configure(api: apiA) }
    await apiA.waitUntilParked()  // account A's attach is suspended
    await vm.configure(api: apiB)
    apiA.release()  // account A's late attach now reports success
    await configuringA.value

    await vm.attachLocalIndexIfNeeded()  // account B's retry must still be allowed through
    #expect(apiB.attached.count == 2,
            "account A's late attach marked account B's manager attached, so B's retry was skipped")
}

// ── The reset seam the apps' nil-client phase calls ─────────────────────────

@Test("reset drops the manager and the typed query, and a later attach retry reaches nobody")
@MainActor
func resetDropsEverythingTheVMHoldsForTheOutgoingAccount() async throws {
    let apiA = ScriptedSearchAPI(.manager(try offlineManager()))
    let vm = SearchVM()
    await vm.configure(api: apiA)
    vm.query = "account A's private query"
    try #require(vm.manager != nil, "precondition: account A's manager is live")
    let attachesBefore = apiA.attached.count

    vm.reset()

    #expect(vm.manager == nil)
    #expect(vm.snapshot == nil)
    #expect(vm.results.isEmpty)
    #expect(vm.query.isEmpty)
    await vm.attachLocalIndexIfNeeded()
    #expect(apiA.attached.count == attachesBefore, "a reset VM still drove account A's api")
}

@Test("a manager build already in flight when the session is reset does not land afterwards")
@MainActor
func aBuildInFlightAtResetDoesNotLand() async throws {
    let apiA = ScriptedSearchAPI(.manager(try offlineManager()))
    apiA.parkBuild = true
    let vm = SearchVM()

    let configuringA = Task { await vm.configure(api: apiA) }
    await apiA.waitUntilParked()
    vm.reset()  // the switch's nil-client phase
    apiA.release()
    await configuringA.value

    #expect(vm.manager == nil, "a build that finished after the reset landed the outgoing account's manager")
}
