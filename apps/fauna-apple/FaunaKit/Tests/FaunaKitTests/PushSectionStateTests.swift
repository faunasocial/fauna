import Testing
@testable import FaunaKit

// What the push-notification control renders (`settings.md` § Push
// notifications). Pure over (the install's stored opt-in, the OS permission,
// the last error) so both Apple targets render one decision rather than two
// hand-rolled ones — the same lift `IdentityChoiceView` took.

@Suite struct PushSectionStateTests {
    private static let everyPermission: [PushManager.Permission?] =
        [nil, .notDetermined, .authorized, .provisional, .denied]

    @Test func theToggleIsTheInstallsOptInWhateverTheOSPermissionSays() {
        // The toggle's state is the user's own opt-in for this install, NOT the
        // platform permission — pivoting on permission makes "off" unreachable
        // on a device the OS still allows.
        for permission in Self.everyPermission {
            #expect(PushSectionState.resolve(optedIn: true, permission: permission,
                                             lastError: nil).isOn)
            #expect(PushSectionState.resolve(optedIn: false, permission: permission,
                                             lastError: nil).isOn == false)
        }
    }

    @Test func aSwitchOffReadsOffWhileThePermissionStaysGranted() {
        // Turning push off leaves the OS permission alone; the toggle must
        // still tell the user their opt-out took.
        let state = PushSectionState.resolve(optedIn: false, permission: .authorized,
                                             lastError: nil)
        #expect(state.isOn == false)
        #expect(state.showsDenied == false)
        #expect(state.error == nil)
    }

    @Test func provisionalPermissionNeverReadsAsOptedIn() {
        // iOS can grant provisional quietly, with no user act at all.
        #expect(PushSectionState.resolve(optedIn: false, permission: .provisional,
                                         lastError: nil).isOn == false)
    }

    @Test func aRefusedPermissionPaintsTheHintAndTheShortcut() {
        #expect(PushSectionState.resolve(optedIn: false, permission: .denied,
                                         lastError: nil).showsDenied)
        // Refused after opting in: the toggle still tells the truth about the
        // install's bit, and the denied pair says why nothing arrives.
        let optedIn = PushSectionState.resolve(optedIn: true, permission: .denied,
                                               lastError: nil)
        #expect(optedIn.isOn)
        #expect(optedIn.showsDenied)
        for permission in Self.everyPermission where permission != .denied {
            #expect(PushSectionState.resolve(optedIn: false, permission: permission,
                                             lastError: nil).showsDenied == false)
        }
    }

    @Test func aFailureRendersItsInlineLineAndAnEmptyOneRendersNothing() {
        #expect(PushSectionState.resolve(optedIn: false, permission: .authorized,
                                         lastError: "boom").error == "boom")
        #expect(PushSectionState.resolve(optedIn: false, permission: .authorized,
                                         lastError: "").error == nil)
        #expect(PushSectionState.resolve(optedIn: false, permission: .authorized,
                                         lastError: nil).error == nil)
    }
}
