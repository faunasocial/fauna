import Foundation
import Testing
@testable import FaunaKit

// The view models on the pages iOS keeps mounted through an account switch, at that
// switch — `docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy,
// the in-memory corollary: state keyed to one identity is dropped at the identity
// change itself, keyed on the identity, and a read already in flight when the drop
// runs must not land on the next account
// .
//
// ONE file for the surviving set on purpose. This is one finding, not nine: every
// surviving page loaded from a bare one-shot `.task {}` (or a `.task(id:)` keyed on
// something other than the client), so none of them re-loaded at a switch at all, and
// `configure`'s unconditional `self.api = api` then paired the incoming account's api
// with the outgoing account's data. Nine near-identical files would hide that shared
// shape, and would be the "hand-list rots" form the corollary's first rule warns
// about — a view model missing from THIS file is visible at a glance.
// `FaunaKit/Core/ActorScope.swift` § *State a view owns* is the roster; this is its pin.
//
// ⚠ ONE member of the set is NOT page-owned, and its suite says so where it sits:
// `FeedVM` is App-scene-level `@State` injected through `.environment`, so its drop
// rides the canonical `ActorScope.dropAppOwnedState` list (pinned in
// `ActorScopeTests`) rather than a page seam . It is kept in this file because it is the same finding.
//
// A real load dials the nest, which a unit test must not, so each double either
// scripts the failure (naming the account, so a test can tell WHOSE failure a page is
// rendering) or the page's fields are seeded directly.

// MARK: - Doubles

/// Records the acting actor id every write was issued with, so a test can prove which
/// account a tap acted AS — the write direction of the isolation contract.
private final class ScriptedContactsAPI: APIClient {
    let account: String
    private(set) var knockActorIds: [String] = []
    /// Suspends the knock read so a switch can land while it is genuinely in flight.
    var parkKnocks = false
    /// When set, the reads SUCCEED with these rows instead of throwing — what pins the
    /// re-check on the landing (success) path, as opposed to the `catch` one.
    var rows: (knocks: [Knock], contacts: [Contact])?
    let parking = ScopeTestParking()

    init(_ account: String) {
        self.account = account
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func fetchKnocks(actorId: String) async throws -> [Knock] {
        await parking.parkIfAsked(parkKnocks)
        if let rows { return rows.knocks }
        throw ScopeTestFailure(account: account)
    }

    override func fetchContacts(actorId: String) async throws -> [Contact] {
        if let rows { return rows.contacts }
        throw ScopeTestFailure(account: account)
    }

    override func sendKnock(actorId: String, peerId: String, recipientNestUrl: String?) async throws {
        knockActorIds.append(actorId)
    }
}

/// An `APIClient` whose moderation-client build is scripted and counted.
///
/// ⚠ The `.client` case matters for the permanence pin and a `.fails`-only double
/// cannot replace it: when the build always throws, `moderation` never becomes
/// non-nil, so `configure`'s `moderation == nil` guard never bites and a test over
/// it would pass with or without the drop. `.client` returns a REAL client over a
/// torn-down nest (`AddressBookVMAccountScopeTests`' trick — nothing dials
/// `nest.invalid`), which is what makes the guard live.
private final class ScriptedModerationAPI: APIClient {
    enum Build {
        case client
        case fails(String)
    }

    var build: Build
    private(set) var buildCount = 0
    var parkBuild = false
    let parking = ScopeTestParking()

    init(_ build: Build) {
        self.build = build
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func moderationClient() async throws -> FfiModerationClient {
        buildCount += 1
        await parking.parkIfAsked(parkBuild)
        switch build {
        case .client:
            return try await tornDownNest().moderation()
        case .fails(let account):
            throw ScopeTestFailure(account: account)
        }
    }
}

private func plainAPI() -> APIClient {
    APIClient(nodeUrl: URL(string: "https://nest.invalid/")!)
}

// MARK: - Fixtures

private let knockA = Knock(id: 1, sender: "account-a-peer", senderNode: nil,
                           summary: "account A's pending knock", createdAt: 0)
private let contactA = Contact(peerId: "account-a-contact", status: "confirmed",
                               updatedAt: 0, handle: "a-peer", domain: "a.invalid")
private let notificationA = NotificationItem(id: "n-a", body: "account A's notification",
                                             createdAt: 0, read: false, notifType: "like")
private let bridgeA = BridgeInfo(id: "account-a-bridge", name: "A", available: true,
                                 linked: true, identity: nil, mode: nil, settings: [],
                                 supportsFollows: false, linkModes: nil, error: nil)
private let rowA = QueueRow(contentId: "account-a-content", contentType: "post",
                            category: "spam", confidencePerMille: 900, action: 1,
                            timestamp: 0, source: .server)
private let quotaA: QuotaResponse = {
    let json = """
        {"tier":"account-a-tier",
         "inbox":{"used_bytes":1,"max_bytes":2},
         "storage":{"used_bytes":3,"max_bytes":4},
         "devices":{"used":1,"max":2},
         "features":{"versioned_backup":true,"bridges":true,"max_feeds":3}}
        """
    return try! JSONDecoder().decode(QuotaResponse.self, from: Data(json.utf8))
}()

// MARK: - ContactsVM (the Contacts tab)

@Suite("ContactsVM at an account switch")
@MainActor
struct ContactsVMAccountScopeTests {
    @Test("a switch drops the previous account's roster, knocks and find state")
    func aSwitchDropsTheRoster() async throws {
        let vm = ContactsVM()
        vm.configure(api: ScriptedContactsAPI("account A"), actorId: "actor-a")
        vm.knocks = [knockA]
        vm.contacts = [contactA]
        vm.findQuery = "account A's search"
        vm.findResult = (actorId: "found-by-a", handle: nil, domain: nil)
        vm.errorMessage = "account A's error"
        try #require(!vm.contacts.isEmpty, "precondition: account A's roster is on the page")

        vm.configure(api: ScriptedContactsAPI("account B"), actorId: "actor-b")

        #expect(vm.contacts.isEmpty, "account A's contacts are on account B's page")
        #expect(vm.knocks.isEmpty, "account A's pending knocks are on account B's page")
        #expect(vm.findQuery.isEmpty, "account A's typed search survived the switch")
        #expect(vm.findResult == nil, "account A's resolved contact survived the switch")
        #expect(vm.errorMessage == nil, "account A's error is on account B's page")
    }

    // The write direction: `ContactsVM` holds the acting `actorId`, so a surviving one
    // makes the INCOMING account's tap act as the outgoing account.
    @Test("after a switch a knock is sent as the incoming account, not the outgoing one")
    func aSwitchRepointsTheActingActor() async throws {
        let apiA = ScriptedContactsAPI("account A")
        let apiB = ScriptedContactsAPI("account B")
        let vm = ContactsVM()
        vm.configure(api: apiA, actorId: "actor-a")

        vm.configure(api: apiB, actorId: "actor-b")
        await vm.addContact(actorId: "some-peer")

        #expect(apiA.knockActorIds.isEmpty, "the knock went to account A's api after the switch")
        #expect(apiB.knockActorIds == ["actor-b"],
                "the knock was sent as \(apiB.knockActorIds) — the outgoing account's actor id survived")
    }

    @Test("a same-account reconfigure keeps the loaded roster")
    func aSameAccountReconfigureKeepsTheRoster() async throws {
        let apiA = ScriptedContactsAPI("account A")
        let vm = ContactsVM()
        vm.configure(api: apiA, actorId: "actor-a")
        vm.contacts = [contactA]

        vm.configure(api: apiA, actorId: "actor-a")   // a remount's re-run

        #expect(!vm.contacts.isEmpty, "a same-account configure dropped the loaded roster")
    }

    @Test("reset drops the roster, the find state and the acting actor")
    func resetDropsEverything() async throws {
        let apiA = ScriptedContactsAPI("account A")
        let vm = ContactsVM()
        vm.configure(api: apiA, actorId: "actor-a")
        vm.contacts = [contactA]
        vm.knocks = [knockA]

        vm.reset()

        #expect(vm.contacts.isEmpty)
        #expect(vm.knocks.isEmpty)
        #expect(!vm.isLoading)
        // The acting actor is gone too: a write must reach nobody.
        await vm.addContact(actorId: "some-peer")
        #expect(apiA.knockActorIds.isEmpty, "a reset VM still sent a knock as account A")
    }

    // The in-flight clause: a read suspended for the outgoing account is not cancelled
    // mid-call, so it returns after the drop and would still assign.
    //
    // ⚠ The read has to be PARKED for this to test anything. Without the parking the
    // `Task` below does not start until the test yields — which is after the
    // synchronous `configure`, so the read is issued as account B and assigning B's
    // own failure is CORRECT. That version of this test failed for exactly that
    // reason, which is worth keeping written down: the window this guard protects is
    // a read suspended mid-call, not a task merely created before the switch.
    @Test("a read already in flight for the outgoing account does not paint after a switch")
    func anInFlightReadDoesNotPaintAfterASwitch() async throws {
        let apiA = ScriptedContactsAPI("account A")
        apiA.parkKnocks = true
        let vm = ContactsVM()
        vm.configure(api: apiA, actorId: "actor-a")

        let refreshingA = Task { await vm.refresh() }
        await apiA.parking.waitUntilParked()   // account A's read is suspended mid-call
        vm.configure(api: ScriptedContactsAPI("account B"), actorId: "actor-b")
        apiA.parking.release()                 // account A's read now throws
        await refreshingA.value

        #expect(vm.errorMessage == nil,
                "account A's late read painted its failure onto account B's page")
    }

    // The same clause on the LANDING path, which the failure case above does not
    // reach: a read that SUCCEEDS late must not put the outgoing account's rows on
    // the page. Two guards, two cases — removing either one reds only its own.
    @Test("rows already in flight for the outgoing account do not land after a switch")
    func inFlightRowsDoNotLandAfterASwitch() async throws {
        let apiA = ScriptedContactsAPI("account A")
        apiA.parkKnocks = true
        apiA.rows = (knocks: [knockA], contacts: [contactA])
        let vm = ContactsVM()
        vm.configure(api: apiA, actorId: "actor-a")

        let refreshingA = Task { await vm.refresh() }
        await apiA.parking.waitUntilParked()
        vm.configure(api: ScriptedContactsAPI("account B"), actorId: "actor-b")
        apiA.parking.release()                 // account A's read now SUCCEEDS
        await refreshingA.value

        #expect(vm.contacts.isEmpty, "account A's late rows landed on account B's page")
        #expect(vm.knocks.isEmpty, "account A's late knocks landed on account B's page")
    }
}

// MARK: - NotificationsVM (More → Notifications)

@Suite("NotificationsVM at an account switch")
@MainActor
struct NotificationsVMAccountScopeTests {
    @Test("a switch drops the previous account's notifications and unread count")
    func aSwitchDropsTheNotifications() async throws {
        let vm = NotificationsVM()
        vm.configure(api: plainAPI(), actorId: "actor-a")
        vm.seedForTest(notifications: [notificationA], unreadCount: 7)
        vm.errorMessage = "account A's error"
        try #require(!vm.notifications.isEmpty, "precondition: account A's rows are on the page")

        vm.configure(api: plainAPI(), actorId: "actor-b")

        #expect(vm.notifications.isEmpty, "account A's notifications are on account B's page")
        #expect(vm.unreadCount == 0, "account A's unread count is on account B's page")
        #expect(vm.errorMessage == nil)
    }

    @Test("reset drops the notifications and the unread count")
    func resetDropsTheNotifications() async throws {
        let vm = NotificationsVM()
        vm.configure(api: plainAPI(), actorId: "actor-a")
        vm.seedForTest(notifications: [notificationA], unreadCount: 7)

        vm.reset()

        #expect(vm.notifications.isEmpty)
        #expect(vm.unreadCount == 0)
    }
}

// MARK: - StatusVM (the Settings root)

@Suite("StatusVM at an account switch")
@MainActor
struct StatusVMAccountScopeTests {
    @Test("a switch drops the previous account's quota and feature limits")
    func aSwitchDropsTheQuota() async throws {
        let vm = StatusVM()
        vm.configure(api: plainAPI())
        vm.quota = quotaA
        vm.errorMessage = "account A's error"
        try #require(vm.quota != nil, "precondition: account A's quota is on the page")

        vm.configure(api: plainAPI())

        #expect(vm.quota == nil, "account A's storage quota is on account B's page")
        #expect(vm.featureRows == nil, "account A's feature limits are on account B's page")
        #expect(vm.errorMessage == nil)
    }

    @Test("a same-account reconfigure keeps the quota")
    func aSameAccountReconfigureKeepsTheQuota() async throws {
        let apiA = plainAPI()
        let vm = StatusVM()
        vm.configure(api: apiA)
        vm.quota = quotaA

        vm.configure(api: apiA)

        #expect(vm.quota != nil, "a same-account configure dropped the loaded quota")
    }

    @Test("reset drops the quota, the feature limits and the error")
    func resetDropsTheQuota() async throws {
        let vm = StatusVM()
        vm.configure(api: plainAPI())
        vm.quota = quotaA
        vm.errorMessage = "account A's error"

        vm.reset()

        #expect(vm.quota == nil)
        #expect(vm.featureRows == nil)
        #expect(vm.errorMessage == nil)
        #expect(!vm.isLoading)
    }
}

// MARK: - BridgeManagerVM (More → Bridges)

@Suite("BridgeManagerVM at an account switch")
@MainActor
struct BridgeManagerVMAccountScopeTests {
    // `linkFields` is the sharp one: for several bridges those values ARE the user's
    // credentials for that provider, and `link(bridge:)` would submit them under the
    // incoming account's api.
    @Test("a switch drops the previous account's bridges, follows and half-typed link credentials")
    func aSwitchDropsTheBridgesAndTypedCredentials() async throws {
        let vm = BridgeManagerVM()
        vm.configure(api: plainAPI())
        vm.bridges = [bridgeA]
        vm.follows = ["account-a-bridge": []]
        vm.linkFields = ["bluesky": ["app_password": "account-A-secret"]]
        vm.selectedMode = ["bluesky": "app_password"]
        vm.followId = "account-a-follow"
        vm.followPetname = "account A's petname"
        vm.errorMessage = "account A's error"
        try #require(!vm.linkFields.isEmpty, "precondition: account A's typed credentials are staged")

        vm.configure(api: plainAPI())

        #expect(vm.linkFields.isEmpty,
                "account A's typed bridge credentials are staged under account B — `link` would submit them")
        #expect(vm.bridges.isEmpty, "account A's bridges are on account B's page")
        #expect(vm.follows.isEmpty, "account A's bridge follows are on account B's page")
        #expect(vm.selectedMode.isEmpty)
        #expect(vm.followId.isEmpty)
        #expect(vm.followPetname.isEmpty)
        #expect(vm.errorMessage == nil)
    }

    // The host view's scoping choice is NOT account-scoped: dropping it would silently
    // widen the AT Protocol page's Linked-account panel to the whole bridge list.
    @Test("a switch keeps the host view's single-bridge scoping choice")
    func aSwitchKeepsTheSingleBridgeScope() async throws {
        let vm = BridgeManagerVM()
        vm.singleBridgeId = "bluesky"
        vm.configure(api: plainAPI())

        vm.configure(api: plainAPI())

        #expect(vm.singleBridgeId == "bluesky",
                "the host's single-bridge scope was dropped as if it were account data")
    }

    @Test("reset drops the bridges, the follows and the link form")
    func resetDropsTheBridges() async throws {
        let vm = BridgeManagerVM()
        vm.configure(api: plainAPI())
        vm.bridges = [bridgeA]
        vm.linkFields = ["bluesky": ["app_password": "account-A-secret"]]

        vm.reset()

        #expect(vm.bridges.isEmpty)
        #expect(vm.linkFields.isEmpty)
        #expect(!vm.isLoading)
    }
}

// MARK: - ModerationQueueVM (More → Moderation)

@Suite("ModerationQueueVM at an account switch")
@MainActor
struct ModerationQueueVMAccountScopeTests {
    // The permanence bug: `configure` re-pointed `api` unconditionally while three
    // `== nil` guards kept the OUTGOING account's moderation client, conversations
    // session and mail-settings machine — so the pairing survived for the whole
    // process, not just one render.
    @Test("a switch builds a moderation client over the new api instead of keeping the previous account's")
    func aSwitchRebuildsOverTheNewApi() async throws {
        // Account A's build SUCCEEDS, so `moderation` is non-nil going into the
        // switch — the state that made the `== nil` guard permanent.
        let apiA = ScriptedModerationAPI(.client)
        let apiB = ScriptedModerationAPI(.fails("account B"))
        let vm = ModerationQueueVM()
        await vm.configure(api: apiA)
        try #require(apiA.buildCount == 1, "precondition: account A's client was built")

        await vm.configure(api: apiB)

        #expect(apiB.buildCount == 1,
                "account B's api was never asked for a moderation client — the `== nil` guard kept account A's")
        #expect(vm.errorMessage?.contains("account B") == true,
                "the page must report account B's failure, not sit on account A's")
    }

    @Test("a switch drops the previous account's queue rows")
    func aSwitchDropsTheQueue() async throws {
        let vm = ModerationQueueVM()
        await vm.configure(api: ScriptedModerationAPI(.fails("account A")))
        vm.seedForTest(rows: [rowA])
        try #require(!vm.rows.isEmpty, "precondition: account A's enforcement history is on the page")

        await vm.configure(api: ScriptedModerationAPI(.fails("account B")))

        #expect(vm.rows.isEmpty, "account A's enforcement history is on account B's page")
    }

    @Test("after a reset the same api is asked for a fresh moderation client")
    func aResetVMRebuildsForTheSameApi() async throws {
        let apiA = ScriptedModerationAPI(.client)
        let vm = ModerationQueueVM()
        await vm.configure(api: apiA)
        try #require(apiA.buildCount == 1, "precondition: a client was built")
        vm.reset()

        await vm.configure(api: apiA)

        #expect(apiA.buildCount == 2, "the outgoing account's moderation client outlived the reset")
    }

    @Test("reset drops the queue and the error")
    func resetDropsTheQueue() async throws {
        let vm = ModerationQueueVM()
        await vm.configure(api: ScriptedModerationAPI(.fails("account A")))
        vm.seedForTest(rows: [rowA])

        vm.reset()

        #expect(vm.rows.isEmpty)
        #expect(vm.errorMessage == nil)
        #expect(!vm.isLoading)
    }

    // The in-flight clause, on the sharpest page: a build suspended for the outgoing
    // account must not paint over the incoming account's own verdict.
    //
    // ⚠ UNPINNED, stated so it is not assumed covered: this reaches the `catch`-path
    // re-check only. `configure`'s three re-checks on the LANDING path (the ones
    // before `moderation`, `conversationsSession` and `mailSettings` are assigned)
    // have no unit pin, because reaching them needs a build that SUCCEEDS late, after
    // which `configure` runs straight on into `api.sharedConversationsSession()` on
    // the outgoing account's real `APIClient` — which dials. A unit test must not, so
    // that case belongs to the account-switch e2e journey, not here. `ContactsVM`'s
    // `inFlightRowsDoNotLandAfterASwitch` above pins the landing path for the same
    // clause on a page whose reads are fully scripted.
    @Test("a moderation-client build already in flight for the outgoing account does not paint after the switch")
    func anInFlightBuildDoesNotPaintAfterASwitch() async throws {
        let apiA = ScriptedModerationAPI(.fails("account A"))
        apiA.parkBuild = true
        let apiB = ScriptedModerationAPI(.fails("account B"))
        let vm = ModerationQueueVM()

        let configuringA = Task { await vm.configure(api: apiA) }
        await apiA.parking.waitUntilParked()   // account A's build is suspended mid-await
        await vm.configure(api: apiB)          // the switch completes meanwhile
        try #require(vm.errorMessage?.contains("account B") == true, "precondition: the switch landed")

        apiA.parking.release()                 // account A's build now throws
        await configuringA.value

        #expect(vm.errorMessage?.contains("account B") == true,
                "account A's late failure overwrote account B's page")
    }
}

// MARK: - Doubles for the pages whose `configure` also loads
//
// These four view models load inside `configure`, so a plain `APIClient` would DIAL
// on every test. Each double scripts the reads its page issues.

/// An `APIClient` whose family-client build is scripted and counted. `.client`
/// returns a real client over a torn-down nest, which is what makes `configure`'s
/// `family == nil` guard live (the same reason `ScriptedModerationAPI` needs it).
private final class ScriptedFamilyAPI: APIClient {
    enum Build {
        case client
        case fails(String)
    }

    var build: Build
    private(set) var buildCount = 0

    init(_ build: Build) {
        self.build = build
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func familyClient() async throws -> FfiFamilyClient {
        buildCount += 1
        switch build {
        case .client:
            return try await tornDownNest().family()
        case .fails(let account):
            throw ScopeTestFailure(account: account)
        }
    }
}

/// An `APIClient` whose subscription reads are scripted — the author's own hydrate
/// and a viewer's offers/status read alike.
private final class ScriptedSubscriptionsAPI: APIClient {
    let account: String
    /// When `status` is set the offers/status reads SUCCEED with these, so a test can
    /// get the viewer's own tier onto the page before the switch.
    var offers: [FfiTierItem] = []
    var status: FfiSubscriptionStatus?

    init(_ account: String) {
        self.account = account
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func listSubscriptionTiers() async throws -> [FfiTierItem] {
        throw ScopeTestFailure(account: account)
    }

    override func subscriptionOffersList(authorIdHex: String) async throws -> [FfiTierItem] {
        guard status != nil else { throw ScopeTestFailure(account: account) }
        return offers
    }

    override func subscriptionStatusGet(authorIdHex: String) async throws -> FfiSubscriptionStatus {
        guard let status else { throw ScopeTestFailure(account: account) }
        return status
    }
}

/// A minimal `EventsAPI` — the seam that protocol exists for. Every read throws, so
/// the page's own loads never dial; the tests here seed the model directly and assert
/// the drop, which is what the identity guard owns.
private final class ScriptedCalendarAPI: EventsAPI {
    let account: String
    init(_ account: String) { self.account = account }
    private func refuse() -> ScopeTestFailure { ScopeTestFailure(account: account) }

    func listCalendars() async throws -> [FaunaCalendar] { throw refuse() }
    func createCalendar(name: String) async throws { throw refuse() }
    func queryEvents(calendarId: String) async throws -> [EventSummary] { throw refuse() }
    func queryEventsSeeded(calendarId: String) async throws -> [EventSummary]? { throw refuse() }
    func queryMyEvents(filter: String) async throws -> [EventSummary] { throw refuse() }
    func createEvent(_ request: CreateEventRequest) async throws { throw refuse() }
    func getEvent(id: String) async throws -> EventDetail { throw refuse() }
    func deleteEvent(id: String) async throws { throw refuse() }
    func inviteToEvent(eventId: String, email: String) async throws { throw refuse() }
    func rsvpEvent(eventId: String, response: RsvpResponse) async throws { throw refuse() }
    func setReminder(eventId: String, offset: String) async throws { throw refuse() }
    func removeReminder(eventId: String) async throws { throw refuse() }
    func importCalendar(calendarId: String, icsText: String) async throws -> CalendarImportResult { throw refuse() }
    func exportCalendar(calendarId: String) async throws -> String { throw refuse() }
}

private let calendarA = FaunaCalendar(id: "account-a-calendar", name: "Account A's calendar")
private let tierA = FfiTierItem(
    name: "account-a-tier", rank: 1, description: nil, priceHint: nil, askingPriceSats: nil,
    paymentUrl: nil, autoApprove: false, createdAt: 0, unlocksPost: nil, hidden: false)

// MARK: - EventsVM (More → Events)

@Suite("EventsVM at an account switch")
@MainActor
struct EventsVMAccountScopeTests {
    @Test("a switch drops the previous account's calendars and per-calendar visibility")
    func aSwitchDropsTheCalendars() async throws {
        let vm = EventsVM()
        vm.configure(api: ScriptedCalendarAPI("account A"))
        vm.calendars = [calendarA]
        vm.selectedCalendar = calendarA
        vm.visibleCalendarIds = ["account-a-calendar"]
        vm.errorMessage = "account A's error"
        vm.creatingEvent = true
        try #require(!vm.calendars.isEmpty, "precondition: account A's calendars are on the page")

        vm.configure(api: ScriptedCalendarAPI("account B"))

        #expect(vm.calendars.isEmpty, "account A's calendars are on account B's page")
        #expect(vm.selectedCalendar == nil, "account A's open calendar is on account B's page")
        #expect(vm.visibleCalendarIds.isEmpty,
                "account A's per-calendar visibility is on account B's page")
        #expect(vm.errorMessage == nil)
        #expect(!vm.creatingEvent)
    }

    @Test("a same-account reconfigure keeps the loaded calendars")
    func aSameAccountReconfigureKeepsTheCalendars() async throws {
        let apiA = ScriptedCalendarAPI("account A")
        let vm = EventsVM()
        vm.configure(api: apiA)
        vm.calendars = [calendarA]

        vm.configure(api: apiA)

        #expect(!vm.calendars.isEmpty, "a same-account configure dropped the loaded calendars")
    }

    // Which day the user is looking at, and in which grid, is device UI state that no
    // account owns — so the drop must NOT take it (`EventsVM.reset()` says so).
    @Test("a switch keeps the viewed date and grid mode")
    func aSwitchKeepsTheViewedDateAndMode() async throws {
        let vm = EventsVM()
        vm.configure(api: ScriptedCalendarAPI("account A"))
        vm.viewMode = .month
        let viewed = vm.currentDate

        vm.configure(api: ScriptedCalendarAPI("account B"))

        #expect(vm.viewMode == .month, "the grid mode was dropped as if it were account data")
        #expect(vm.currentDate == viewed, "the viewed date was dropped as if it were account data")
    }

    @Test("reset drops the calendars and the in-flight form flags")
    func resetDropsTheCalendars() async throws {
        let vm = EventsVM()
        vm.configure(api: ScriptedCalendarAPI("account A"))
        vm.calendars = [calendarA]
        vm.showNewEvent = true

        vm.reset()

        #expect(vm.calendars.isEmpty)
        #expect(!vm.showNewEvent)
        #expect(!vm.isLoading)
    }
}

// MARK: - FamilyVM (More → Family)

@Suite("FamilyVM at an account switch")
@MainActor
struct FamilyVMAccountScopeTests {
    // The `draft*` fields are the SELECTED WARD's policy staged for save, so a
    // survivor would offer to write the outgoing account's ward policy through the
    // incoming account's session.
    @Test("a switch drops the previous account's staged ward policy")
    func aSwitchDropsTheStagedWardPolicy() async throws {
        let vm = FamilyVM()
        await vm.configure(api: ScriptedFamilyAPI(.fails("account A")))
        vm.draftContactApproval = true
        vm.draftUnknownSender = "block"
        vm.draftScreenDailyMinutes = "45"
        vm.contactAddInput = "account-a-peer"
        vm.transferInput = "account-a-guardian"
        vm.graduateConfirmVisible = true
        try #require(vm.draftContactApproval, "precondition: account A's policy edit is staged")

        await vm.configure(api: ScriptedFamilyAPI(.fails("account B")))

        #expect(!vm.draftContactApproval, "account A's staged ward policy is on account B's page")
        #expect(vm.draftUnknownSender == "allow")
        #expect(vm.draftScreenDailyMinutes.isEmpty)
        #expect(vm.contactAddInput.isEmpty)
        #expect(vm.transferInput.isEmpty, "account A's proposed new guardian survived the switch")
        #expect(!vm.graduateConfirmVisible)
    }

    // ⚠ There is deliberately NO build-count test here, unlike `ModerationQueueVM`'s
    // and `AddressBookVM`'s, and the reason is worth recording so nobody adds an
    // unsound one: `APIClient.familyStatus()` calls `familyClient()` ITSELF on every
    // read (it is the shared choke point that also persists the supervision
    // snapshot). So `load()` re-vends a client through whichever api is current, and
    // a build lands on the incoming api whether or not the drop ran — the count
    // cannot distinguish the two. Dropping the `family` handle still matters, because
    // every OTHER call on this page goes through the stored one
    // (`family.approvalsList()`, the policy writes); it is simply not separately
    // observable from a unit test, so the staged-policy test above is what pins this
    // VM's drop.
    @Test("reset drops the staged policy and the loaded roster")
    func resetDropsTheStagedPolicy() async throws {
        let vm = FamilyVM()
        await vm.configure(api: ScriptedFamilyAPI(.fails("account A")))
        vm.transferInput = "account-a-guardian"
        vm.draftScreenDailyMinutes = "45"

        vm.reset()

        #expect(vm.transferInput.isEmpty)
        #expect(vm.draftScreenDailyMinutes.isEmpty)
        #expect(vm.wards.isEmpty)
        #expect(vm.errorMessage == nil)
        #expect(!vm.isLoading)
    }
}

// MARK: - SubscriptionsVM + ProfileOffersVM (More → Profile)

@Suite("SubscriptionsVM at an account switch")
@MainActor
struct SubscriptionsVMAccountScopeTests {
    // `formProviderSecret` is the sharp one — the §4 payment-provider form's API
    // secret, typed by the author, which `setPaymentProvider` would submit under the
    // incoming account's api.
    @Test("a switch drops the previous account's typed provider secret and tier forms")
    func aSwitchDropsTheProviderSecret() async throws {
        let vm = SubscriptionsVM()
        await vm.configure(api: ScriptedSubscriptionsAPI("account A"), authorIdHex: "actor-a")
        vm.formName = "account A's tier"
        vm.formAskingPrice = "5000"
        vm.selectedTier = "account-a-tier"
        #if !FAUNA_EXCISE_PAYMENTS
        vm.formProviderSecret = "account-A-webhook-secret"
        vm.formProviderKind = "stripe"
        try #require(!vm.formProviderSecret.isEmpty, "precondition: account A's secret is typed")
        #endif

        await vm.configure(api: ScriptedSubscriptionsAPI("account B"), authorIdHex: "actor-b")

        #if !FAUNA_EXCISE_PAYMENTS
        #expect(vm.formProviderSecret.isEmpty,
                "account A's typed provider secret is staged under account B — `setPaymentProvider` would submit it")
        #expect(vm.formProviderKind.isEmpty)
        #endif
        #expect(vm.formName.isEmpty, "account A's half-written tier is on account B's form")
        #expect(vm.formAskingPrice.isEmpty)
        #expect(vm.selectedTier.isEmpty)
        #expect(vm.tiers.isEmpty)
        #expect(vm.subscribers.isEmpty, "account A's subscriber roster is on account B's page")
    }

    @Test("reset drops the tier list, the roster and the forms")
    func resetDropsTheForms() async throws {
        let vm = SubscriptionsVM()
        await vm.configure(api: ScriptedSubscriptionsAPI("account A"), authorIdHex: "actor-a")
        vm.formName = "account A's tier"

        vm.reset()

        #expect(vm.formName.isEmpty)
        #expect(vm.tiers.isEmpty)
        #expect(vm.errorMessage == nil)
    }
}

@Suite("ProfileOffersVM at an account switch")
@MainActor
struct ProfileOffersVMAccountScopeTests {
    // `statusTier` is the VIEWER's own held tier, not the creator's public offer — so
    // a survivor tells the incoming account it holds a paid tier on the strength of
    // the outgoing account's subscription.
    @Test("a switch drops the viewer's own held tier and the creator's offers")
    func aSwitchDropsTheViewersTier() async throws {
        let apiA = ScriptedSubscriptionsAPI("account A")
        apiA.offers = [tierA]
        apiA.status = FfiSubscriptionStatus(tier: "account-a-tier", expiresAt: nil, autoApprove: false)
        let vm = ProfileOffersVM()
        await vm.configure(api: apiA, authorIdHex: "some-creator")
        try #require(vm.statusTier == "account-a-tier",
                     "precondition: the viewer's own tier is on the page")
        try #require(!vm.offers.isEmpty, "precondition: the creator's offers are loaded")

        // The SAME creator is being viewed — only the viewer changed, which is
        // exactly what a `viewedActorId`-keyed task could not see.
        await vm.configure(api: ScriptedSubscriptionsAPI("account B"), authorIdHex: "some-creator")

        #expect(vm.statusTier == nil,
                "account A's held tier is attributed to account B")
        #expect(vm.offers.isEmpty, "account A's loaded offers are on account B's page")
        #expect(!vm.isFollowing, "account A's follow state is on account B's page")
    }

    @Test("reset drops the viewer's tier, the offers and the follow state")
    func resetDropsTheViewerState() async throws {
        let apiA = ScriptedSubscriptionsAPI("account A")
        apiA.offers = [tierA]
        apiA.status = FfiSubscriptionStatus(tier: "account-a-tier", expiresAt: nil, autoApprove: false)
        let vm = ProfileOffersVM()
        await vm.configure(api: apiA, authorIdHex: "some-creator")

        vm.reset()

        #expect(vm.statusTier == nil)
        #expect(vm.offers.isEmpty)
        #expect(!vm.isFollowing)
        #expect(vm.errorMessage == nil)
    }
}

// MARK: - FeedVM (the Feed tab — but NOT page-owned)

/// An `APIClient` whose feed-manager build is counted. The manager is REAL, over a
/// torn-down nest (nothing dials), because `FeedVM.configure`'s early return keys on
/// `configuredSecret`, which is only set once a build succeeds — a throwing double
/// could never reach the behaviour these tests pin.
private final class ScriptedFeedAPI: APIClient {
    private(set) var buildCount = 0

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func feedManager(secret secretHex: String) async throws -> FfiFeedManager {
        buildCount += 1
        // Deliberately NOT calling `adoptActor` as the real one does: this double
        // exists to count builds, not to prime a connection.
        return try await tornDownNest().feedManager(secret: Data(repeating: 7, count: 32))
    }
}

// `FeedVM` is in this file because it is part of the same finding, but it is the one
// member that is NOT page-owned: `feedVM` is App-scene-level `@State` injected
// through `.environment` (`FaunaApp.swift` / `FaunaMacApp.swift`), which
// `FeedListView` reads with `@Environment(FeedVM.self)`. So its drop rides the
// canonical `ActorScope.dropAppOwnedState` list — pinned there by
// `ActorScopeTests.dropAppOwnedStateDropsEveryAppOwnedSurface` — and what this suite
// pins is `reset()` itself, above all the actor-key half that no page seam could have
// reached .
@Suite("FeedVM at an account switch")
@MainActor
struct FeedVMAccountScopeTests {
    // THE headline: `configure` returns early when `manager != nil, configuredSecret
    // == secretHex`, so before this drop a factory-reset that re-claimed the SAME
    // actor left the VM holding a manager built on the discarded `APIClient` for the
    // rest of the process — and that path unmounts the Feed page, so no page seam
    // could ever have fired on it.
    //
    // ⚠ What this pins is the MANAGER drop, which is the half that breaks the early
    // return (the condition needs both). Mutation-testing found that removing
    // `reset()`'s `configuredSecret = nil` alone reds nothing, because the manager is
    // already gone — so that half is bookkeeping, and `FeedVM.reset()`'s own comment
    // says so rather than letting a reader infer a pin that does not exist.
    @Test("after a reset the SAME actor's secret builds a fresh manager instead of returning the stale one")
    func aResetVMRebuildsForTheSameSecret() async throws {
        let api = ScriptedFeedAPI()
        let vm = FeedVM()
        await vm.configure(api: api, secretHex: "aa")
        try #require(api.buildCount == 1, "precondition: a manager was built")
        try #require(vm.manager != nil, "precondition: the manager is installed")

        vm.reset()
        await vm.configure(api: api, secretHex: "aa")   // the SAME actor, re-claimed

        #expect(api.buildCount == 2,
                "the outgoing session's manager outlived the reset — `configuredSecret` was not dropped")
    }

    // The guard the drop must not break: a plain re-entry of the page with the same
    // actor still costs nothing.
    @Test("a same-secret reconfigure without a reset builds nothing more")
    func aSameSecretReconfigureIsOneBuild() async throws {
        let api = ScriptedFeedAPI()
        let vm = FeedVM()
        await vm.configure(api: api, secretHex: "aa")

        await vm.configure(api: api, secretHex: "aa")

        #expect(api.buildCount == 1, "a same-actor reconfigure rebuilt the manager")
    }

    @Test("reset drops the manager, the api and the page's view glue")
    func resetDropsTheManagerAndGlue() async throws {
        let api = ScriptedFeedAPI()
        let vm = FeedVM()
        await vm.configure(api: api, secretHex: "aa")
        vm.searchText = "the outgoing actor's search"
        vm.showCreateForm = true
        vm.pendingPostOpen = "outgoing-post"
        vm.setClientErrorMessage("the outgoing actor's error")
        try #require(vm.manager != nil, "precondition: the manager is installed")

        vm.reset()

        #expect(vm.manager == nil, "the outgoing actor's feed manager survived the drop")
        #expect(vm.api == nil)
        #expect(vm.posts.isEmpty, "the outgoing actor's posts are still rendered")
        #expect(vm.feeds.isEmpty)
        #expect(!vm.composeReady, "the compose surface must not be armed without a manager")
        #expect(vm.searchText.isEmpty)
        #expect(!vm.showCreateForm)
        #expect(vm.pendingPostOpen == nil)
        #expect(vm.clientErrorMessage == nil)
        #expect(!vm.isReconfiguring)
    }

    // The page watches `managerGeneration` to dismiss a pushed post detail, whose
    // fire-once gated-unlock `.task` would otherwise unseal a post straight into the
    // incoming actor's feed. A drop is a manager change, so it must bump too.
    @Test("reset bumps the manager generation so a pushed post detail is dismissed")
    func resetBumpsTheManagerGeneration() async throws {
        let api = ScriptedFeedAPI()
        let vm = FeedVM()
        await vm.configure(api: api, secretHex: "aa")
        let before = vm.managerGeneration

        vm.reset()

        #expect(vm.managerGeneration != before,
                "a pushed post detail from the outgoing actor would stay on screen")
    }

    // The statics are `resetSharedState()`'s, not `reset()`'s (the corollary's first
    // rule: one owner per field) — but `reset()` must not leave the mirrored list
    // holding the outgoing actor's posts either, since the test agent serializes it.
    @Test("reset leaves the mirrored post list empty")
    func resetEmptiesTheMirroredPostList() async throws {
        let api = ScriptedFeedAPI()
        let vm = FeedVM()
        await vm.configure(api: api, secretHex: "aa")
        FeedVM.lastLoadedPosts = []   // whatever the observer last mirrored

        vm.reset()

        #expect(FeedVM.lastLoadedPosts.isEmpty,
                "the state the test agent serializes still reports the departed actor's posts")
    }
}
