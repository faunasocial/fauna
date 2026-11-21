import Foundation
import Testing
@testable import FaunaKit

// ui/README.md § Offer only what is possible, rule 7 — a wizard gesture's
// refusal is never silent. `OnboardingMachine.surface` is the apple twin of
// tui's `surface`: the machine's own refusal stays on `errorMessage()`, any
// other thrown error lands there when the machine left it empty, and never
// over the machine's own reason.

private final class NoopObserver: OnboardingObserver, @unchecked Sendable {
    func onChanged() {}
}

private struct ProbeError: LocalizedError {
    var errorDescription: String? { "probe refusal" }
}

private func wizard() -> OnboardingMachine {
    let m = OnboardingMachine(observer: NoopObserver())
    m.beginCreateIdentity()
    m.setCurrentHandle(h: "alice@fresh.test")
    return m
}

@Test func aRefusedGoalSelectShowsItsReasonOnErrorMessage() {
    let m = wizard()
    #expect(m.errorMessage() == nil)
    // The server cannot be covered by the manual row: the machine refuses.
    let ok = m.surface { try $0.selectGoalCover(goal: .vps, cover: .manual) }
    #expect(!ok)
    let shown = m.errorMessage()
    #expect(shown != nil)
    #expect(shown?.isEmpty == false)
}

@Test func anErrorTheMachineLeftUnsaidLandsOnErrorMessage() {
    let m = wizard()
    let ok = m.surface { _ in throw ProbeError() }
    #expect(!ok)
    #expect(m.errorMessage() == "probe refusal")
}

@Test func theMachinesOwnReasonIsNeverOverwritten() {
    let m = wizard()
    m.surface { try $0.selectGoalCover(goal: .vps, cover: .manual) }
    let machineReason = m.errorMessage()
    m.surface { _ in throw ProbeError() }
    #expect(m.errorMessage() == machineReason)
}

@Test func aGestureThatSucceedsReportsTrueAndAddsNoError() {
    let m = wizard()
    #expect(m.surface { _ in })
    #expect(m.errorMessage() == nil)
}
