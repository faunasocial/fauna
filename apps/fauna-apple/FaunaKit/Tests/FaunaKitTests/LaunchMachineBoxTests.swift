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
}
