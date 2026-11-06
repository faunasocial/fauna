import Foundation
import Testing
@testable import FaunaKit

// Pin the Apple apps' one door for the `fauna://` in-app routes
// (`ConsentHandoff`; apps/ios.md § App Entry → In-app routes): it parses ONLY
// through the shared `parseAppRoute`, stages each route for its page when the
// session is authenticated — the consent request for Connected apps, a share
// route's target for Media or Folders (`ShareRouteOpen`) — holds it while signed
// out, and clears the staged consent request only after the page's open has
// finished. Every test builds its own instance, so none shares the process-wide
// `ConsentHandoff.shared`.

private let handle = "urn:ietf:params:oauth:request_uri:abc123"
private let consentUrl = URL(string: "fauna://consent/\(handle)")!
private let shareLinkUrl = URL(string: "fauna://share-link?folder=7&path=Trips/a%20b.jpg")!
private let folderShareUrl = URL(string: "fauna://folder-share?folder=3")!

@Test func onlyTheConsentRouteYieldsARequestUri() {
    #expect(ConsentHandoff.consentRequestUri(from: consentUrl) == handle)
    #expect(ConsentHandoff.consentRequestUri(from: folderShareUrl) == nil)
    // A handle that is not what PAR mints, and a foreign scheme, never parse.
    #expect(ConsentHandoff.consentRequestUri(
        from: URL(string: "fauna://consent/not-a-par-handle")!) == nil)
    #expect(ConsentHandoff.consentRequestUri(
        from: URL(string: "https://example.com/consent/\(handle)")!) == nil)
}

@MainActor
@Test func anAuthenticatedRouteIsStagedForThePageAndClearedOnlyAfterTheOpen() {
    let door = ConsentHandoff()
    #expect(door.receive(consentUrl, authenticated: true) == .apply(.connectedApps))
    #expect(door.peekPending() == handle)
    #expect(door.held == nil)
    // Peeking never consumes: the page opens it, and only THEN is it cleared.
    #expect(door.peekPending() == handle)
    let visit = door.registerVisit()
    #expect(door.beginOpen(visit: visit) == handle)
    #expect(door.finishOpen(handle, visit: visit))
    #expect(door.peekPending() == nil)
}

@MainActor
@Test func aStagedRequestIsClaimedForOpeningOnceSoTwoVisitsNeverBothOpenIt() {
    let door = ConsentHandoff()
    #expect(door.beginOpen(visit: door.registerVisit()) == nil)
    door.receive(consentUrl, authenticated: true)
    let visit = door.registerVisit()
    #expect(door.beginOpen(visit: visit) == handle)
    // The same visit asking again while its open is in flight finds nothing to
    // open; the request stays staged until the open returns.
    #expect(door.beginOpen(visit: visit) == nil)
    #expect(door.peekPending() == handle)
    door.finishOpen(handle, visit: visit)
    #expect(door.peekPending() == nil)
    #expect(door.beginOpen(visit: visit) == nil)
}

@MainActor
@Test func onlyAVisitBegunAfterTheRouteWasStagedOpensIt() {
    // A page left mounted from an earlier session is still finishing a visit
    // over that session's client when the route lands: that visit never opens
    // the request — the visit the route's navigation starts does.
    let door = ConsentHandoff()
    let stale = door.registerVisit()
    door.receive(consentUrl, authenticated: true)
    #expect(door.beginOpen(visit: stale) == nil)
    let fresh = door.registerVisit()
    #expect(door.beginOpen(visit: stale) == nil)
    #expect(door.beginOpen(visit: fresh) == handle)
}

@MainActor
@Test func anOpenANewerVisitOvertookIsReReadByThatVisitBeforeTheRouteDrains() async {
    let door = ConsentHandoff()
    door.receive(consentUrl, authenticated: true)
    let first = door.registerVisit()
    #expect(door.beginOpen(visit: first) == handle)
    let newer = door.registerVisit()
    // The newer visit cannot open it again (the nest spends a request once)…
    #expect(door.beginOpen(visit: newer) == nil)
    // …and the overtaken open does not paint or drain the route on return.
    #expect(!door.finishOpen(handle, visit: first))
    #expect(door.peekPending() == handle)
    // The newest visit re-reads its tray for it; only then is it drained.
    #expect(await door.awaitSpent(visit: first) == nil)
    #expect(await door.awaitSpent(visit: newer) == handle)
    door.finishReread(handle)
    #expect(door.peekPending() == nil)
    #expect(await door.waitUntilDrained(timeout: 0.15))
}

@MainActor
@Test func aVisitWaitsOutAnOpenInFlightBeforeReReading() async {
    let door = ConsentHandoff()
    door.receive(consentUrl, authenticated: true)
    let first = door.registerVisit()
    #expect(door.beginOpen(visit: first) == handle)
    let newer = door.registerVisit()
    async let reread = door.awaitSpent(visit: newer)
    // Whether or not the waiter has parked on the open yet, it answers the
    // request once the open returns.
    try? await Task.sleep(nanoseconds: 50_000_000)
    door.finishOpen(handle, visit: first)
    #expect(await reread == handle)
}

@MainActor
@Test func aRouteArrivingSignedOutIsHeldAndAppliedAfterSignIn() {
    let door = ConsentHandoff()
    #expect(door.receive(consentUrl, authenticated: false) == .held)
    #expect(door.held == .consent(requestUri: handle))
    #expect(door.peekPending() == nil)

    #expect(door.takeHeld() == .connectedApps)
    #expect(door.held == nil)
    #expect(door.peekPending() == handle)
    // Nothing left to apply a second time.
    #expect(door.takeHeld() == nil)
}

@MainActor
@Test func aUrlThatIsNoSharedRouteFallsThrough() {
    let door = ConsentHandoff()
    // The File Provider context actions' own vocabulary (the version leaf) and
    // the onboarding URIs are not this door's: the caller falls through.
    #expect(door.receive(
        URL(string: "fauna://folder/docs?action=versions&path=a.txt")!, authenticated: true)
        == .notARoute)
    #expect(door.receive(URL(string: "fauna://identity?secret=00")!, authenticated: true)
        == .notARoute)
    #expect(door.peekPending() == nil)
    #expect(door.held == nil)
    #expect(!door.shares.isPending)
}

@MainActor
@Test func theShareLinkRouteStagesItsFileForMedia() {
    let door = ConsentHandoff()
    #expect(door.receive(shareLinkUrl, authenticated: true) == .apply(.media))
    let staged = door.shares.takeShareLink()
    #expect(staged?.folderId == 7)
    #expect(staged?.path == "Trips/a b.jpg")
    #expect(!door.shares.isPending)
    #expect(door.peekPending() == nil)
}

@MainActor
@Test func theFolderShareRouteOpensOnlyAnOwnedRowsFlow() {
    let door = ConsentHandoff()
    #expect(door.receive(folderShareUrl, authenticated: true) == .apply(.folders))
    let rows: [(id: Int64, name: String)] = [(id: 1, name: "Docs"), (id: 3, name: "Photos")]
    // The page expands the owned row; that row's section takes the route.
    #expect(door.shares.resolveFolderShare(ownerRows: rows) == "Photos")
    #expect(!door.shares.takeFolderShare(id: 1))
    #expect(door.shares.takeFolderShare(id: 3))
    #expect(!door.shares.isPending)

    // A set this account does not own consumes the route and opens nothing.
    #expect(door.receive(URL(string: "fauna://folder-share?folder=99")!, authenticated: true)
        == .apply(.folders))
    #expect(door.shares.resolveFolderShare(ownerRows: rows) == nil)
    #expect(!door.shares.isPending)
}

@MainActor
@Test func aShareRouteArrivingSignedOutIsHeldThenStaged() {
    let door = ConsentHandoff()
    #expect(door.receive(folderShareUrl, authenticated: false) == .held)
    #expect(!door.shares.isPending)
    #expect(door.takeHeld() == .folders)
    #expect(door.shares.folderShare == 3)
}

@MainActor
@Test func theLastShareRouteWins() {
    let door = ConsentHandoff()
    door.receive(shareLinkUrl, authenticated: true)
    door.receive(folderShareUrl, authenticated: true)
    #expect(door.shares.shareLink == nil)
    #expect(door.shares.folderShare == 3)
}

@MainActor
@Test func aNewerRouteStagedMeanwhileSurvivesTheOlderOpenFinishing() {
    let door = ConsentHandoff()
    let newer = "urn:ietf:params:oauth:request_uri:zzz999"
    door.receive(consentUrl, authenticated: true)
    door.receive(URL(string: "fauna://consent/\(newer)")!, authenticated: true)
    // The page finished opening the FIRST request; the newer one stays staged.
    door.finishOpen(handle, visit: door.registerVisit())
    #expect(door.peekPending() == newer)
}

@MainActor
@Test func waitingUntilDrainedReturnsOnceTheOpenFinishes() async {
    let door = ConsentHandoff()
    door.receive(consentUrl, authenticated: true)
    // Nothing finishes it: the deadline-polled wait gives up rather than hanging.
    #expect(await door.waitUntilDrained(timeout: 0.15) == false)
    door.finishOpen(handle, visit: door.registerVisit())
    #expect(await door.waitUntilDrained(timeout: 0.15) == true)

    // A share route drains once its page has taken it.
    door.receive(shareLinkUrl, authenticated: true)
    #expect(await door.waitUntilDrained(timeout: 0.15) == false)
    _ = door.shares.takeShareLink()
    #expect(await door.waitUntilDrained(timeout: 0.15) == true)
}
