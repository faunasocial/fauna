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

    @Test func aNewerLaunchRetiresAnOlderOne() {
        let box = LaunchMachineBox()
        let older = box.beginLaunch()
        let newer = box.beginLaunch()
        #expect(!box.isCurrent(launch: older))
        #expect(box.isCurrent(launch: newer))
    }
}
