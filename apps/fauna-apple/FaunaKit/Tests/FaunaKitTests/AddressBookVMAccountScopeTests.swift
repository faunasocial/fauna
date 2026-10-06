import Foundation
import Testing
@testable import FaunaKit

// `AddressBookVM` at an account switch — `docs/goal/architecture/apps/account-scoping.md`
// § The scoping taxonomy, the in-memory corollary: the CardDAV client this VM holds,
// and the address books and vCards it loaded through it, are account-scoped state,
// dropped at the identity change and keyed on the identity, and a load already in
// flight when the drop runs must not land on the next account
// .
//
// The worst of the sweep: the VM's `configure` memoized its FIRST run in
// `configureTask` and returned it to every later caller whatever the api, so a
// second account never built a client at all. iOS mounts `AddressBookView` inside the
// Contacts TAB — a tab is never unmounted — so a switch with the Address Book
// segment open left account A's personal vCards rendered under account B, with no
// error anywhere. (A remount, e.g. toggling the segment, was always safe: it makes a
// fresh VM.)
//
// A real load dials the nest, which a unit test must not; so the api double scripts
// the CardDAV build itself, and ``AddressBookVM/seedForTest(addressbooks:cards:)``
// stands in for a completed load.

/// An `APIClient` whose CardDAV client build is scripted.
private final class ScriptedCarddavAPI: APIClient {
    enum Build {
        case client
        case fails(String)
    }

    var build: Build
    /// How many times this api was asked to build a CardDAV client.
    private(set) var buildCount = 0
    var parkBuild = false
    let parking = ScopeTestParking()

    init(_ build: Build) {
        self.build = build
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func carddavClient() async throws -> FfiCarddavClient {
        buildCount += 1
        await parking.parkIfAsked(parkBuild)
        switch build {
        case .client:
            return try await tornDownNest().carddav()
        case .fails(let account):
            throw ScopeTestFailure(account: account)
        }
    }
}

private let bookA = FfiAddressbookRow(id: "aa", name: "Account A's contacts", description: "", cardCount: 1)
private let cardA = FfiCardRow(
    id: "01", uid: "uid-a", formattedName: "Account A's private contact", emails: [], tels: [],
    addresses: [], urls: [], org: [], title: "", note: "", bday: "", hasFaunaExt: false)

// ── The switch ──────────────────────────────────────────────────────────────

@Suite("AddressBookVM at an account switch")
@MainActor
struct AddressBookVMAccountScopeTests {
    @Test("a switch to another account builds a CardDAV client over the new api instead of returning the first run")
    @MainActor
    func aSwitchBuildsAClientOverTheNewApi() async throws {
        let apiA = ScriptedCarddavAPI(.fails("account A"))
        let apiB = ScriptedCarddavAPI(.fails("account B"))
        let vm = AddressBookVM()
        await vm.configure(api: apiA)

        await vm.configure(api: apiB)

        #expect(apiB.buildCount == 1, "account B's api was never asked for a CardDAV client — the first account's run was returned")
        #expect(vm.errorMessage?.contains("account B") == true,
                "the page must report account B's failure, not sit on account A's")
    }

    @Test("a switch drops the previous account's address books, vCards, open card and error")
    @MainActor
    func aSwitchDropsTheLoadedContacts() async throws {
        let vm = AddressBookVM()
        await vm.configure(api: ScriptedCarddavAPI(.fails("account A")))
        vm.seedForTest(addressbooks: [bookA], cards: [cardA])
        vm.selectCard(cardA)
        try #require(!vm.addressbooks.isEmpty && !vm.cards.isEmpty && vm.selectedCard != nil,
                     "precondition: account A's contacts are on the page")

        await vm.configure(api: ScriptedCarddavAPI(.fails("account B")))  // account B's connection fails

        #expect(vm.addressbooks.isEmpty, "account A's address books are on account B's page")
        #expect(vm.cards.isEmpty, "account A's vCards are on account B's page")
        #expect(vm.selectedAddressbookId == nil)
        #expect(vm.selectedCard == nil, "account A's open contact is on account B's page")
        #expect(vm.errorMessage?.contains("account A") != true)
    }

    @Test("configuring again with the same api awaits the one run and builds nothing more")
    @MainActor
    func reconfiguringTheSameApiIsOneRun() async throws {
        let apiA = ScriptedCarddavAPI(.fails("account A"))
        let vm = AddressBookVM()
        await vm.configure(api: apiA)
        vm.seedForTest(addressbooks: [bookA], cards: [cardA])

        await vm.configure(api: apiA)  // the deep-link `.task` beside the plain one, or a remount's re-run

        #expect(apiA.buildCount == 1)
        #expect(!vm.addressbooks.isEmpty, "a same-account configure dropped the loaded address books")
    }

    // ── A result already in flight (`account-scoping.md`'s in-flight clause) ────

    @Test("a CardDAV build failure already in flight for the outgoing account does not paint after the switch")
    @MainActor
    func aBuildFailureInFlightDoesNotPaintAfterASwitch() async throws {
        let apiA = ScriptedCarddavAPI(.fails("account A"))
        apiA.parkBuild = true
        let apiB = ScriptedCarddavAPI(.fails("account B"))
        let vm = AddressBookVM()

        let configuringA = Task { await vm.configure(api: apiA) }
        await apiA.parking.waitUntilParked()  // account A's build is suspended mid-await
        await vm.configure(api: apiB)  // the switch completes meanwhile
        try #require(vm.errorMessage?.contains("account B") == true, "precondition: the switch landed")

        apiA.parking.release()  // account A's build now throws
        await configuringA.value

        #expect(vm.errorMessage?.contains("account B") == true,
                "account A's late failure overwrote account B's page")
    }

    // ── The reset seam the page's nil-client phase calls ────────────────────────

    @Test("reset drops the client, the loaded contacts and the error")
    @MainActor
    func resetDropsEverythingTheVMHoldsForTheOutgoingAccount() async throws {
        let apiA = ScriptedCarddavAPI(.client)
        let vm = AddressBookVM()
        // A BUILT client (over a torn-down nest, so nothing dials) — so the locate below
        // has a real client to reach if the reset failed to drop it.
        await vm.configure(api: apiA)
        vm.seedForTest(addressbooks: [bookA], cards: [cardA])
        vm.selectCard(cardA)
        vm.errorMessage = "account A's error"
        try #require(apiA.buildCount == 1, "precondition: a CardDAV client was built")

        vm.reset()

        #expect(vm.addressbooks.isEmpty)
        #expect(vm.cards.isEmpty)
        #expect(vm.selectedAddressbookId == nil)
        #expect(vm.selectedCard == nil)
        #expect(vm.errorMessage == nil)
        #expect(!vm.isLoading)
        // The client is gone too: a deep-link locate must reach nobody.
        #expect(await vm.locateCard(uidHash: "00") == false)
        #expect(vm.errorMessage == nil, "a reset VM still drove account A's CardDAV client")
    }

    @Test("after a reset the same api is asked for a fresh client")
    @MainActor
    func aResetVMRebuildsForTheSameApi() async throws {
        let apiA = ScriptedCarddavAPI(.fails("account A"))
        let vm = AddressBookVM()
        await vm.configure(api: apiA)
        vm.reset()

        await vm.configure(api: apiA)

        #expect(apiA.buildCount == 2, "the memoized first run outlived the reset")
    }

    @Test("a CardDAV build already in flight when the page is reset does not land afterwards")
    @MainActor
    func aBuildInFlightAtResetDoesNotLand() async throws {
        let apiA = ScriptedCarddavAPI(.fails("account A"))
        apiA.parkBuild = true
        let vm = AddressBookVM()

        let configuringA = Task { await vm.configure(api: apiA) }
        await apiA.parking.waitUntilParked()
        vm.reset()  // the switch's nil-client phase
        apiA.parking.release()
        await configuringA.value

        #expect(vm.errorMessage == nil, "a build that finished after the reset painted the outgoing account's failure")
    }
}
