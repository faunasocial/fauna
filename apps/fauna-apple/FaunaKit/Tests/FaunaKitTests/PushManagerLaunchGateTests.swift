import Foundation
import Testing
@testable import FaunaKit

// The durable half of the push control (`settings.md` § Push notifications;
// `account-scoping.md` § The scoping taxonomy, class 2).
//
// What these pin is a *negative*: an install that never opted in — or that
// opted out — is never re-subscribed by a launch. Both Apple targets used to
// gate launch re-registration on the OS notification permission alone, which a
// switch-off does not touch and must not, so every switch-off was undone by the
// next authenticated launch: the token arrived, the callback subscribed
// unconditionally, and the row the user had deleted came back with no UI ever
// saying so.
//
// Level: the intent store — the real shared record, through the FFI, at a
// per-test temporary path — plus the pure launch gate; no notification centre
// and no `APIClient`. "A relaunch" is a *fresh* `PushIntentStore` over the same
// path, which is exactly what a new process reads.

private func freshPath() -> String {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("fauna-push-intent-tests-\(UUID().uuidString)")
        .appendingPathComponent(PushIntentStore.fileName).path
}

@Suite struct PushManagerLaunchGateTests {
    @Test func pushIntentIsAbsentOnAFreshInstall() {
        let store = PushIntentStore(path: freshPath())
        #expect(store.isOptedIn == false)
        #expect(store.intent.actor == nil)
    }

    // The gate itself: permission is necessary, never sufficient.
    @Test func launchGateStaysShutWithoutTheInstallsOwnOptIn() {
        for permission: PushManager.Permission in [.authorized, .provisional, .denied, .notDetermined] {
            #expect(PushManager.shouldRegisterAtLaunch(isOptedIn: false,
                                                       permission: permission) == false)
        }
    }

    @Test func launchGateOpensOnlyForAnOptedInInstallTheOSStillAllows() {
        #expect(PushManager.shouldRegisterAtLaunch(isOptedIn: true, permission: .authorized))
        // Opted in, but the user has since refused at the OS level: asking APNs
        // for a token again is pointless, and the settings row sends them to
        // System Settings instead.
        #expect(PushManager.shouldRegisterAtLaunch(isOptedIn: true, permission: .denied) == false)
    }

    // The round trip, end to end at this level: opted in → switched off →
    // relaunch → the gate is SHUT, so no device token is requested and nothing
    // is subscribed. This is the assertion the gate exists for.
    @Test func aSwitchedOffInstallIsNotReSubscribedByTheNextLaunch() throws {
        let path = freshPath()

        try pushSeedOptIn(intentPath: path)
        #expect(PushManager.shouldRegisterAtLaunch(
            isOptedIn: PushIntentStore(path: path).isOptedIn,
            permission: .authorized))

        // The user switches push off. The OS permission is deliberately
        // untouched — revoking it belongs to System Settings — so it is still
        // `.authorized`.
        try pushClearOptIn(intentPath: path)

        // Relaunch: fresh process, same record, permission still granted.
        let afterRelaunch = PushIntentStore(path: path)
        #expect(afterRelaunch.isOptedIn == false)
        #expect(PushManager.shouldRegisterAtLaunch(isOptedIn: afterRelaunch.isOptedIn,
                                                   permission: .authorized) == false)

        // …and the toggle says so, rather than reading the permission and
        // rendering "on" under a switch-off that took.
        #expect(PushSectionState.resolve(optedIn: afterRelaunch.isOptedIn,
                                         permission: .authorized,
                                         lastError: nil).isOn == false)
    }
}
