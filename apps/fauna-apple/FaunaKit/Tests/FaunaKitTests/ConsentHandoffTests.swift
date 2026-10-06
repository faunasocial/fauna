import Foundation
import Testing
@testable import FaunaKit

// Pin the Apple apps' one door for the `fauna://consent/<request_uri>` route
// (`ConsentHandoff`; apps/ios.md § App Entry → In-app routes): it parses ONLY
// through the shared `parseAppRoute`, stages the request for the Connected apps
// page when the session is authenticated, holds it while signed out, and clears
// the staged request only after the page's open has finished. Every test builds
// its own instance, so none shares the process-wide `ConsentHandoff.shared`.

private let handle = "urn:ietf:params:oauth:request_uri:abc123"
private let consentUrl = URL(string: "fauna://consent/\(handle)")!

@Test func onlyTheConsentRouteIsThisDoorsToTake() {
    #expect(ConsentHandoff.consentRequestUri(from: consentUrl) == handle)
    // The File Provider context actions' vocabulary and the share routes are not
    // this door's: they fall through to their own parsers.
    #expect(ConsentHandoff.consentRequestUri(
        from: URL(string: "fauna://folder/Photos?action=share")!) == nil)
    #expect(ConsentHandoff.consentRequestUri(
        from: URL(string: "fauna://folder-share?folder=3")!) == nil)
    // A handle that is not what PAR mints, and a foreign scheme, never parse.
    #expect(ConsentHandoff.consentRequestUri(
        from: URL(string: "fauna://consent/not-a-par-handle")!) == nil)
    #expect(ConsentHandoff.consentRequestUri(
        from: URL(string: "https://example.com/consent/\(handle)")!) == nil)
}

@MainActor
@Test func anAuthenticatedRouteIsStagedForThePageAndClearedOnlyAfterTheOpen() {
    let door = ConsentHandoff()
    #expect(door.receive(consentUrl, authenticated: true))
    #expect(door.peekPending() == handle)
    #expect(door.held == nil)
    // Peeking never consumes: the page opens it, and only THEN is it cleared.
    #expect(door.peekPending() == handle)
    door.finishOpen(handle)
    #expect(door.peekPending() == nil)
}

@MainActor
@Test func aRouteArrivingSignedOutIsHeldAndAppliedAfterSignIn() {
    let door = ConsentHandoff()
    #expect(door.receive(consentUrl, authenticated: false))
    #expect(door.held == handle)
    #expect(door.peekPending() == nil)

    #expect(door.takeHeld())
    #expect(door.held == nil)
    #expect(door.peekPending() == handle)
    // Nothing left to apply a second time.
    #expect(!door.takeHeld())
}

@MainActor
@Test func aRouteThatIsNotTheConsentRouteIsNotConsumed() {
    let door = ConsentHandoff()
    // The caller must fall through to its other deep-link parsers.
    #expect(!door.receive(URL(string: "fauna://folder-share?folder=3")!, authenticated: true))
    #expect(door.peekPending() == nil)
    #expect(door.held == nil)
}

@MainActor
@Test func aNewerRouteStagedMeanwhileSurvivesTheOlderOpenFinishing() {
    let door = ConsentHandoff()
    let newer = "urn:ietf:params:oauth:request_uri:zzz999"
    #expect(door.receive(consentUrl, authenticated: true))
    #expect(door.receive(URL(string: "fauna://consent/\(newer)")!, authenticated: true))
    // The page finished opening the FIRST request; the newer one stays staged.
    door.finishOpen(handle)
    #expect(door.peekPending() == newer)
}

@MainActor
@Test func waitingUntilDrainedReturnsOnceTheOpenFinishes() async {
    let door = ConsentHandoff()
    #expect(door.receive(consentUrl, authenticated: true))
    // Nothing finishes it: the deadline-polled wait gives up rather than hanging.
    #expect(await door.waitUntilDrained(timeout: 0.15) == false)
    door.finishOpen(handle)
    #expect(await door.waitUntilDrained(timeout: 0.15) == true)
}
