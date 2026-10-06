import Foundation
import SwiftData
import Testing
@testable import FaunaKit

// The apple twin of linux's `actor_scope.rs` tests: pin that FaunaKit's half of
// the in-memory actor-scoped drop actually drops, so the isolation contract in
// `account-scoping.md` § The scoping taxonomy holds across an identity change.
//
// `resetSharedStateDropsEveryFaunaKitOwnedSurface` etc. below cover the SHARED
// half. `dropAppOwnedStateDropsEveryAppOwnedSurface` covers the app-owned half
// too (`criticalAlertsHost`, the conversations manager, `modelContainer`, the
// `AppState`/`MacAppState` caches) — previously untestable here because each
// target's own `dropActorScopedState()` lived on a SwiftUI `App` struct, not
// constructible in a unit test; `ActorScope.dropAppOwnedState(...)` extracted
// the same code as a plain FaunaKit function , so it takes ordinary FaunaKit objects instead.
//
// Every test body below is **fully synchronous on the MainActor**, which buys two
// things at once. First, neither cadence's loop body ever gets a chance to run:
// `start` spawns a MainActor-isolated `Task`, which cannot be scheduled until the
// test yields, so no tick ever reaches the network and the URL below is never
// dialled. Second, these are the only tests in the target that touch these process
// singletons, and a synchronous MainActor body cannot interleave with another — so
// the shared state they mutate needs no serialization opt-in.

@MainActor
private func throwawayApi() -> APIClient {
    APIClient(nodeUrl: URL(string: "https://nest.invalid")!)
}

/// The headline: one call disarms both cadences and empties the reveal set.
/// Mutation check — deleting any one of those three lines from
/// `ActorScope.resetSharedState` turns exactly one expectation below red.
///
/// `FeedVM.lastLoadedPosts` is asserted but deliberately **not** pre-populated: a
/// `PostSummary` fixture would have to carry a `RenderDocument` +
/// `VerificationStatus` and would break on every unrelated field the shared
/// snapshot grows — the same call `CueViewportObserverTests` already makes for that
/// type. Its clearing rides the tier_3 actor-switch journey instead (the test agent
/// serializes it as `data.feed.posts[]`).
@MainActor
@Test func resetSharedStateDropsEveryFaunaKitOwnedSurface() {
    DnsAutoRenewCadence.shared.start(api: throwawayApi())
    SubscriptionsAuthorCadence.shared.start(api: throwawayApi())
    GuardianNotifyCadence.shared.start(api: throwawayApi())
    FeedVM.revealedMutedPostIds = ["post-1"]

    #expect(DnsAutoRenewCadence.shared.isRunning)
    #expect(SubscriptionsAuthorCadence.shared.isRunning)
    #expect(GuardianNotifyCadence.shared.isRunning)

    ActorScope.resetSharedState()

    #expect(!DnsAutoRenewCadence.shared.isRunning,
            "the DNS auto-renew cadence kept running against the outgoing api")
    #expect(!SubscriptionsAuthorCadence.shared.isRunning,
            "the subscriptions author cadence kept polling as the outgoing identity")
    #expect(!GuardianNotifyCadence.shared.isRunning,
            "the Guardian Notify cadence kept running against the outgoing api")
    #expect(FeedVM.revealedMutedPostIds.isEmpty,
            "the outgoing account's muted-post reveals survived into the incoming session")
    #expect(FeedVM.lastLoadedPosts.isEmpty)
}

/// The Guardian Notify twin of `theLatchedDnsCadenceRebindsOnlyAfterTheDrop`
/// below, PLUS the sharp edge that cadence doesn't have: the dedup set. A
/// teardown that skipped the drop would not just leave the loop bound to the
/// outgoing `APIClient` — it would make the INCOMING ward's first
/// enforcement on a reused item id silently uncounted, because the outgoing
/// actor's `seen` set is still in force. `reset()` (called from `stop()`)
/// clears both the accumulator's dedup set AND its `enabled` flag, so the
/// witness here is a full round-trip: record → reset → re-enable → record
/// the SAME item id again → it must count.
@MainActor
@Test func stopClearsTheDedupSetSoAReusedItemIdCountsAgainForTheIncomingActor() {
    ActorScope.resetSharedState()
    let baseline = GuardianNotifyCadence.shared.armCount

    GuardianNotifyCadence.shared.start(api: throwawayApi())
    #expect(GuardianNotifyCadence.shared.armCount == baseline + 1,
            "the outgoing identity's login must arm the cadence")

    // A second `start` without an intervening `stop` must be swallowed by the
    // latch — the same defect class `DnsAutoRenewCadence` guards.
    GuardianNotifyCadence.shared.start(api: throwawayApi())
    #expect(GuardianNotifyCadence.shared.armCount == baseline + 1,
            "the latch must swallow a re-start")

    ActorScope.resetSharedState()
    #expect(!GuardianNotifyCadence.shared.isRunning)
    GuardianNotifyCadence.shared.start(api: throwawayApi())
    #expect(GuardianNotifyCadence.shared.armCount == baseline + 2,
            "after the drop the incoming identity must be able to arm its own loop")

    ActorScope.resetSharedState()
}

/// Why the `stop()` inside `resetSharedState` is load-bearing rather than tidy.
///
/// `DnsAutoRenewCadence.start` is latched (`guard loop == nil`), so a teardown that
/// skips the drop leaves the cadence **permanently** bound to the outgoing
/// `APIClient`: the next login's own `start` is silently swallowed. This is the
/// apple twin of linux's never-reset `AtomicBool` finding, and it is what made
/// `factoryResetReonboard`'s missing stop a production defect rather than a
/// cosmetic one — a factory-reset re-onboard kept issuing certs against the
/// pre-reset nest for the rest of the process.
///
/// The witness is `armCount`, not `isRunning`: `isRunning` reads `true` whether the
/// second `start` rebound or was swallowed, so asserting on it would be vacuous.
/// `armCount` increments only when the latch actually lets a `start` through.
@MainActor
@Test func theLatchedDnsCadenceRebindsOnlyAfterTheDrop() {
    // Land on a known baseline — sibling tests share this process singleton.
    ActorScope.resetSharedState()
    let baseline = DnsAutoRenewCadence.shared.armCount

    DnsAutoRenewCadence.shared.start(api: throwawayApi())
    #expect(DnsAutoRenewCadence.shared.armCount == baseline + 1,
            "the outgoing identity's login must arm the cadence")

    // The incoming identity's login, with the drop skipped — what a drifted
    // teardown site does. The latch swallows it: no new loop, so the one still
    // running is the OUTGOING actor's, holding the outgoing api.
    DnsAutoRenewCadence.shared.start(api: throwawayApi())
    #expect(DnsAutoRenewCadence.shared.armCount == baseline + 1,
            "the latch must swallow a re-start — this is the defect the drop exists to prevent")

    // ...and the drop is what breaks the latch, so the next login arms afresh
    // against its own api.
    ActorScope.resetSharedState()
    #expect(!DnsAutoRenewCadence.shared.isRunning)
    DnsAutoRenewCadence.shared.start(api: throwawayApi())
    #expect(DnsAutoRenewCadence.shared.armCount == baseline + 2,
            "after the drop the incoming identity must be able to arm its own tick")

    ActorScope.resetSharedState()
}

/// The drop must be safe to call when nothing is armed — several of the four
/// teardown sites per target can run from a partially-built session.
@MainActor
@Test func resetSharedStateIsIdempotentAndSafeWhenNothingIsArmed() {
    ActorScope.resetSharedState()
    ActorScope.resetSharedState()

    #expect(!DnsAutoRenewCadence.shared.isRunning)
    #expect(!SubscriptionsAuthorCadence.shared.isRunning)
    #expect(!GuardianNotifyCadence.shared.isRunning)
    #expect(FeedVM.lastLoadedPosts.isEmpty)
    #expect(FeedVM.revealedMutedPostIds.isEmpty)
}

/// A minimal `ActorScopedAppCaches` conformer — stands in for `AppState`/
/// `MacAppState`'s four e2e-serialization caches without pulling in either
/// platform's full app-entry type.
@MainActor
private final class FakeAppCaches: ActorScopedAppCaches {
    var lastContacts: [Contact] = []
    var lastKnocks: [Knock] = []
    var lastEvents: [EventSummary] = []
    var notificationsUnreadCount: Int = 0
    var liveModelContainer: ModelContainer?
    let webPublish = WebPublishStore()
}

@MainActor
private func inMemoryModelContainer() -> ModelContainer {
    let config = ModelConfiguration(schema: Schema([PhotoBackupRecord.self]), isStoredInMemoryOnly: true)
    return try! ModelContainer(for: Schema([PhotoBackupRecord.self]), configurations: [config])
}

/// The app-owned half's headline test — the twin of
/// `resetSharedStateDropsEveryFaunaKitOwnedSurface` above, now possible
/// because `dropAppOwnedState` takes plain FaunaKit objects instead of a
/// SwiftUI `App`'s own state. Mutation check: deleting any one line from
/// `ActorScope.dropAppOwnedState` turns exactly one expectation below red.
@MainActor
@Test func dropAppOwnedStateDropsEveryAppOwnedSurface() {
    let criticalAlertsHost = CriticalAlertsHost()
    let conversationsVM = ConversationsVM()
    // App-scene-level `@State` injected through `.environment`, exactly like
    // `conversationsVM` — which is why it is on this list and not behind a page
    // seam .
    let feedVM = FeedVM()
    feedVM.searchText = "the outgoing actor's feed search"
    feedVM.showCreateForm = true
    feedVM.pendingPostOpen = "outgoing-actor-post-id"
    feedVM.setClientErrorMessage("the outgoing actor's error")
    // The Events page's view model — app-scene-level like `feedVM`, so a
    // half-written event outlives leaving the page; its drop is this list's.
    let eventsVM = EventsVM()
    eventsVM.scheduleDraftsSave(FfiEventDrafts(summary: "the outgoing actor's event",
                                                dtstart: "", dtend: "", description: "", location: ""))
    eventsVM.showNewEvent = true
    // The session's ONE Devices/Folders view model — app-scene-level like
    // `feedVM`, so the machine (and the followed-folders memory inside it)
    // outlives a page visit; its drop is this list's.
    let devicesVM = DevicesMachineVM()
    devicesVM.offlineSharePeerCodeInput = "the outgoing actor's peer code"
    let screenTime = ScreenTimeStore()
    screenTime.start(api: throwawayApi())
    #expect(screenTime.isRunning)
    var modelContainer = inMemoryModelContainer()
    let originalContainer = modelContainer
    let appState = FakeAppCaches()
    appState.liveModelContainer = originalContainer
    appState.lastContacts = [Contact(peerId: "peer-1", status: "known", updatedAt: nil, handle: nil, domain: nil)]
    appState.lastKnocks = []
    appState.notificationsUnreadCount = 3
    // The outgoing actor's web-publish surface — `landHydrate` is the pure
    // guarded-landing step `hydrate` itself calls, driven directly here
    // since a real hydrate needs a live `FfiWebClient`
    // (`WebPublishStoreTests.swift` covers the generation guard itself).
    appState.webPublish.landHydrate(
        generation: appState.webPublish.currentGeneration,
        client: nil,
        servingDomain: "outgoing.example.com",
        view: SubdomainView(enabled: true, url: "https://outgoing.example.com/", disabledReason: nil),
        domains: [WebDomainRow(domain: "outgoing.example.com", status: "active")],
        posts: [])
    #expect(appState.webPublish.isHydrated, "sanity: the outgoing actor's store is actually hydrated")

    ActorScope.dropAppOwnedState(
        criticalAlertsHost: criticalAlertsHost,
        conversationsVM: conversationsVM,
        feedVM: feedVM,
        eventsVM: eventsVM,
        devicesVM: devicesVM,
        screenTime: screenTime,
        modelContainer: &modelContainer,
        appState: appState,
        newModelContainer: inMemoryModelContainer
    )

    #expect(criticalAlertsHost.active.isEmpty)
    #expect(conversationsVM.session == nil,
            "deactivate() must release the outgoing identity's conversations session")
    #expect(!screenTime.isRunning,
            "the outgoing ward's screen-time lock/accrual must not survive into the incoming session")
    #expect(modelContainer !== originalContainer,
            "the outgoing actor's photo-backup container must be replaced, not reused")
    #expect(appState.liveModelContainer == nil,
            """
            the LIVE container slot must be cleared too — it, not the `@State` half, \
            is what every callback-reached read resolves to, so a survivor here keeps \
            the incoming actor reading the outgoing one's photo-backup store
            """)
    #expect(appState.lastContacts.isEmpty)
    #expect(appState.lastKnocks.isEmpty)
    #expect(appState.lastEvents.isEmpty)
    #expect(appState.notificationsUnreadCount == 0,
            "a survivor here would report the departed account's unread count as the current one's")
    #expect(feedVM.searchText.isEmpty,
            "the outgoing actor's feed search term survived into the incoming session")
    #expect(!feedVM.showCreateForm)
    #expect(feedVM.pendingPostOpen == nil,
            "a deep-link post staged by the outgoing actor would open on the incoming actor's feed")
    #expect(feedVM.clientErrorMessage == nil)
    #expect(eventsVM.resumableDraft == nil,
            "the outgoing actor's half-written event would resume on the incoming actor's New Event")
    #expect(!eventsVM.showNewEvent)
    #expect(devicesVM.offlineSharePeerCodeInput.isEmpty,
            "the session-held Devices view model must not carry the outgoing actor into the incoming session")
    #expect(!appState.webPublish.isHydrated,
            "the outgoing account's web-publish origin/client must not survive an account switch")
    #expect(appState.webPublish.domains.isEmpty)
    #expect(appState.webPublish.posts.isEmpty)
    #expect(appState.webPublish.servingDomain.isEmpty)
}

/// Safe to call from a partially-built session — several of the four
/// per-target teardown sites can run before everything is armed.
@MainActor
@Test func dropAppOwnedStateIsSafeWhenNothingIsArmed() {
    let criticalAlertsHost = CriticalAlertsHost()
    let conversationsVM = ConversationsVM()
    let feedVM = FeedVM()
    let eventsVM = EventsVM()
    let devicesVM = DevicesMachineVM()
    let screenTime = ScreenTimeStore()
    var modelContainer = inMemoryModelContainer()
    let appState = FakeAppCaches()

    ActorScope.dropAppOwnedState(
        criticalAlertsHost: criticalAlertsHost,
        conversationsVM: conversationsVM,
        feedVM: feedVM,
        eventsVM: eventsVM,
        devicesVM: devicesVM,
        screenTime: screenTime,
        modelContainer: &modelContainer,
        appState: appState,
        newModelContainer: inMemoryModelContainer
    )

    #expect(!screenTime.isRunning)
    #expect(appState.notificationsUnreadCount == 0)
}
