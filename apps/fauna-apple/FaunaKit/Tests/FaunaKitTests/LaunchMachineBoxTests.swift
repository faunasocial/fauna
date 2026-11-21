import Testing
@testable import FaunaKit

/// The launch-freshness token (`LaunchMachineBox.beginLaunch()`).
///
/// A launch that must `await` before it can build its machine — macOS resolves
/// the launch binding first — is in flight before the box holds anything, so a
/// superseding clear (the e2e session patch) found an empty box, retired
/// nothing, and the launch then installed its machine and ran
/// `completeAuthenticatedLaunch` on top of the patched-in session. Measured on a
/// macOS builder seat: two live conversations sessions in one process, the
/// first's replica load refused as "handed over to a newer engine".
@Suite struct LaunchMachineBoxTests {
    @Test func aLaunchIsCurrentUntilSomethingSupersedesIt() {
        let box = LaunchMachineBox()
        let launch = box.beginLaunch()
        #expect(box.isCurrent(launch: launch))
    }

    @Test func clearingAnEmptyBoxStillRetiresALaunchStillAwaitingItsMachine() {
        let box = LaunchMachineBox()
        let launch = box.beginLaunch()
        // The session patch's clear, landing before the launch installed anything.
        box.machine = nil
        #expect(!box.isCurrent(launch: launch))
    }

    // ── `claim(resuming:)` — the sign-out gate's deferred re-entry ──────────
    // The first launch of every process is deferred across the gate's detached
    // finish and re-enters with the claim it was deferred with.

    @Test func aFreshEntryTakesANewClaim() {
        let box = LaunchMachineBox()
        let launch = box.claim(resuming: nil)
        #expect(launch != nil)
        #expect(box.isCurrent(launch: launch!))
    }

    @Test func aDeferredReEntryKeepsItsClaimWhenNothingSupersededIt() {
        let box = LaunchMachineBox()
        let deferred = box.claim(resuming: nil)!
        #expect(box.claim(resuming: deferred) == deferred)
    }

    @Test func aSessionPatchDuringTheGatesFinishRetiresTheDeferredReEntry() {
        let box = LaunchMachineBox()
        let deferred = box.claim(resuming: nil)!
        // The e2e session patch's clear, landing while the finish runs and no
        // machine was ever installed.
        box.machine = nil
        #expect(box.claim(resuming: deferred) == nil)
    }

    @Test func aNewerEntryDuringTheGatesFinishRetiresTheDeferredReEntry() {
        let box = LaunchMachineBox()
        let deferred = box.claim(resuming: nil)!
        // A switch's teardown re-runs the launch while the first is deferred.
        let newer = box.claim(resuming: nil)!
        #expect(box.claim(resuming: deferred) == nil)
        #expect(box.claim(resuming: newer) == newer)
    }

    @Test func aNewerLaunchRetiresAnOlderOne() {
        let box = LaunchMachineBox()
        let older = box.beginLaunch()
        let newer = box.beginLaunch()
        #expect(!box.isCurrent(launch: older))
        #expect(box.isCurrent(launch: newer))
    }

    // ── The finished wizard's hold (`onboarding.md` § Wizard exit handling →
    // *The finished wizard holds until the session is signed in*) ────────────
    // iOS enters a launch optimistically off the cached session; a launch the
    // wizard's `LoggedIn` handoff or an append finish starts has no cached
    // session this device entered before, so it gates as macOS does: the
    // spinner, the tabs only on `Online`, `launch_retry` on a transient failure.

    @Test func aColdLaunchEntersOptimistically() {
        let box = LaunchMachineBox()
        #expect(box.entersOptimistically(pendingFactoryReset: false))
    }

    @Test func aPendingFactoryResetWithholdsTheEntry() {
        let box = LaunchMachineBox()
        #expect(!box.entersOptimistically(pendingFactoryReset: true))
    }

    @Test func aFinishedWizardsLaunchWithholdsTheEntry() {
        let box = LaunchMachineBox()
        box.holdUntilOnline()
        #expect(box.heldUntilOnline)
        #expect(!box.entersOptimistically(pendingFactoryReset: false))
    }

    @Test func theHoldOutlivesEveryTeardownAndRetryBeforeOnline() {
        let box = LaunchMachineBox()
        box.holdUntilOnline()
        // The append finish's switch teardown, then Retry on `launch_retry`:
        // each clears the box and claims a fresh launch.
        box.machine = nil
        _ = box.claim(resuming: nil)
        _ = box.beginLaunch()
        #expect(!box.entersOptimistically(pendingFactoryReset: false))
    }

    @Test func onlineOrAPlainSwitchReleasesTheHold() {
        let box = LaunchMachineBox()
        box.holdUntilOnline()
        box.releaseHold()
        #expect(!box.heldUntilOnline)
        #expect(box.entersOptimistically(pendingFactoryReset: false))
    }
}
