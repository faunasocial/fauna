import Testing
@testable import FaunaKit

// ui/README.md § Offer only what is possible, rules 2 and 6: the wizard's
// Continues are painted from the machine's own `Possibility` answers
// (`OnboardingMachine.*Possibility()`, the UniFFI faces tui, linux and
// android paint), never from a bool or `Option` the app pairs with a reason
// itself. These pin what the apple views now read: blocked with a reason
// where the page would offer nothing, possible where it would.
//
// The possible arms of the providers and DNS Continues need a verified
// provider or a covered goal, which no FFI test hook reaches; the Rust
// tests own them (`continue_from_dns_refuses_what_the_page_does_not_offer`).

private final class NoopObserver: OnboardingObserver, @unchecked Sendable {
    func onChanged() {}
}

private func wizard() -> OnboardingMachine {
    let m = OnboardingMachine(observer: NoopObserver())
    m.beginCreateIdentity()
    m.setCurrentHandle(h: "alice@fresh.test")
    return m
}

@Test func providersContinueIsBlockedWithItsReasonWhileNoGoalIsCovered() {
    let answer = wizard().providersContinuePossibility()
    #expect(!answer.isPossible, "nothing covered: Continue is dead")
    #expect(answer.isShown, "dead, never hidden — the page names why")
    #expect(answer.reasonText?.isEmpty == false, "the status line has words to paint")
}

@Test func dnsContinueIsBlockedWithItsStatusLineUntilAProviderIsVerified() {
    let m = wizard()
    m.setStepForTest(step: .dnsConfig)
    let answer = m.dnsContinuePossibility()
    #expect(!answer.isPossible, "no verified provider: Continue is dead")
    #expect(answer.isShown)
    #expect(answer.reasonText?.isEmpty == false, "its reason is the page's status line")
}

@Test func inviteContinueFollowsTheOutOfBandCode() {
    let m = wizard()
    #expect(!m.inviteContinuePossibility().isPossible, "no code yet: blocked")

    let idle = m.inviteRequestSnapshot()
    var valid = idle
    valid.outOfBandCodeState = .valid(inviteId: "inv-1", supervisedBy: nil)
    m.setInviteRequestSnapshotForTest(snap: valid)
    #expect(m.inviteContinuePossibility().isPossible, "a valid code is the gate")
}

@Test func provisioningStartIsPossibleOffTheProvidersPath() {
    // Not entered from the choose-providers page: the routing guard each app
    // used to add itself is folded into the machine's answer.
    #expect(wizard().provisioningStartPossibility().isPossible)
}
