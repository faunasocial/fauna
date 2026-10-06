import Testing
@testable import FaunaMacOSLib

// The macOS leg of auto-start at sign-in (`apps/macos.md` § App Lifecycle →
// *Auto-start at sign-in*): the two pure decisions, the truth table linux's
// `autostart.rs` and windows' `AutoStartTests.cs` pin, plus the macOS-only
// half — the toggle telling the truth about a System Settings → Login Items
// opt-out. The `SMAppService` mechanics around them are not unit-testable (a
// real registration writes the machine's login items); these pin everything
// that decides what the mechanics are asked to do.
@Suite struct AutoStartTests {

    // --- shouldRegister: the tri-state choice at the post-auth hook ---

    @Test func neverChosenRegistersByDefault() {
        #expect(AutoStart.shouldRegister(choice: nil, e2e: false))
    }

    @Test func anExplicitOptInRegisters() {
        #expect(AutoStart.shouldRegister(choice: true, e2e: false))
    }

    @Test func anExplicitOptOutIsNeverReRegistered() {
        #expect(!AutoStart.shouldRegister(choice: false, e2e: false))
    }

    @Test func e2eNeverRegistersWhateverTheChoice() {
        for choice in [nil, true, false] as [Bool?] {
            #expect(!AutoStart.shouldRegister(choice: choice, e2e: true))
        }
    }

    // --- the launch route: when the hidden-launch decision can be taken ---

    @Test func aLaunchStillResolvingHasNoRouteYet() {
        #expect(AutoStart.route(gate: .launching, isOnboarded: false) == nil)
        #expect(AutoStart.route(gate: .launching, isOnboarded: true) == nil)
    }

    @Test func aReadyOnboardedLaunchIsAuthenticated() {
        #expect(AutoStart.route(gate: .ready, isOnboarded: true) == .authenticated)
    }

    @Test func aReadyLaunchWithoutAnAccountIsOnboarding() {
        #expect(AutoStart.route(gate: .ready, isOnboarded: false) == .needsUser)
    }

    @Test func everyBlockingLaunchSurfaceNeedsTheUser() {
        let blocking: [LaunchGate] = [
            .retrying(error: "offline"),
            .needsUpdate(message: "outdated"),
            .signInRefused(message: "refused"),
            .identityChanged(pinnedHex: "aa", seenHex: nil),
        ]
        for gate in blocking {
            #expect(AutoStart.route(gate: gate, isOnboarded: true) == .needsUser)
        }
    }

    // --- shouldStartHidden: never a silently dead agent ---

    @Test func anAutostartLaunchThatSignsInStaysHidden() {
        #expect(AutoStart.shouldStartHidden(autostartFlag: true, route: .authenticated))
    }

    @Test func anAutostartLaunchThatNeedsTheUserIsLoud() {
        #expect(!AutoStart.shouldStartHidden(autostartFlag: true, route: .needsUser))
    }

    @Test func aPlainLaunchAlwaysShowsItsWindow() {
        #expect(!AutoStart.shouldStartHidden(autostartFlag: false, route: .authenticated))
        #expect(!AutoStart.shouldStartHidden(autostartFlag: false, route: .needsUser))
    }

    @Test func theFlagIsReadFromArgv() {
        #expect(AutoStart.isAutostartLaunch(arguments: ["/Fauna.app/Contents/MacOS/Fauna", "--autostart"]))
        #expect(!AutoStart.isAutostartLaunch(arguments: ["/Fauna.app/Contents/MacOS/Fauna"]))
    }

    // --- the toggle tells the truth: the OS opt-out is the user's choice too ---

    @Test func theToggleShowsTheChoiceDefaultingOn() {
        #expect(AutoStart.displayedChoice(choice: nil, osOptedOut: false))
        #expect(AutoStart.displayedChoice(choice: true, osOptedOut: false))
        #expect(!AutoStart.displayedChoice(choice: false, osOptedOut: false))
    }

    @Test func aLoginItemsOptOutReadsOffWhateverTheChoice() {
        #expect(!AutoStart.displayedChoice(choice: nil, osOptedOut: true))
        #expect(!AutoStart.displayedChoice(choice: true, osOptedOut: true))
    }

    @Test func turningOnOverAnOSOptOutRoutesToSystemSettings() {
        #expect(AutoStart.enableAction(osOptedOut: true) == .openSystemSettings)
        #expect(AutoStart.enableAction(osOptedOut: false) == .register)
    }

    // --- the post-auth hook's composition ---

    @Test func thePostAuthHookNeverRegistersOverAnOSOptOut() {
        #expect(AutoStart.postAuthAction(choice: nil, e2e: false, osOptedOut: true) == .leaveAlone)
        #expect(AutoStart.postAuthAction(choice: true, e2e: false, osOptedOut: true) == .leaveAlone)
    }

    @Test func thePostAuthHookRegistersADefaultOnInstall() {
        #expect(AutoStart.postAuthAction(choice: nil, e2e: false, osOptedOut: false) == .register)
    }

    @Test func thePostAuthHookWritesNothingUnderE2EOrAfterAnOptOut() {
        #expect(AutoStart.postAuthAction(choice: nil, e2e: true, osOptedOut: false) == .leaveAlone)
        #expect(AutoStart.postAuthAction(choice: false, e2e: false, osOptedOut: false) == .leaveAlone)
    }
}
