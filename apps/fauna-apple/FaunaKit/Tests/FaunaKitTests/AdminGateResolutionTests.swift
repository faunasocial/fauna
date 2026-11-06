import Testing
@testable import FaunaKit

// Non-e2e unit tests for the admin-gate re-read rule (transport.md § Which
// surfaces a push invalidates, the `admin_gate` flag; admin.md § Navigation
// model): a FAILED read moves nothing, and the once-per-session admin effects
// (the require-confirm auto-default) fire on the false→true transition only.
// Mirrors tui's `Outcome::GateUnreachable` / `became_admin` and linux's
// `admin_gate_passed` pins.

@MainActor @Suite struct AdminGateResolutionTests {
    @Test func failedReadKeepsAConfirmedAdmin() {
        let r = FaunaClient.resolveAdminGate(read: nil, lastAnswer: true)
        #expect(r.isAdmin)
        #expect(!r.becameAdmin)
    }

    @Test func failedReadKeepsANonAdminAndFiresNothing() {
        let r = FaunaClient.resolveAdminGate(read: nil, lastAnswer: false)
        #expect(!r.isAdmin)
        #expect(!r.becameAdmin)
    }

    @Test func confirmingReReadDoesNotRefireTheAdminEffects() {
        let r = FaunaClient.resolveAdminGate(read: true, lastAnswer: true)
        #expect(r.isAdmin)
        #expect(!r.becameAdmin)
    }

    @Test func firstConfirmationFiresTheAdminEffectsOnce() {
        let r = FaunaClient.resolveAdminGate(read: true, lastAnswer: false)
        #expect(r.isAdmin)
        #expect(r.becameAdmin)
    }

    @Test func definitiveNoClearsAnAdmin() {
        let r = FaunaClient.resolveAdminGate(read: false, lastAnswer: true)
        #expect(!r.isAdmin)
        #expect(!r.becameAdmin)
    }
}
