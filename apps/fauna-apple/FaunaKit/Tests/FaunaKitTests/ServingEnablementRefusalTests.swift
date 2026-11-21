#if !FAUNA_KIDS
import Testing
@testable import FaunaKit

// onboarding.md § 3b *The step completes; it is not fired once*: a post-claim
// serving-enablement step the nest refuses for good is said on the page the
// person is on, not only logged. The shared step returns the refusal text over
// the FFI; the glue puts it on the session, which the root banner paints as the
// `error-message`. Drives the main-actor applier directly, no network.

@Test @MainActor func aRefusalForGoodReachesThePagesErrorMessage() {
    let session = SessionState()
    MailEnableGlue.applyServingEnablementRefusal("The nest refused to turn on email.", session: session)
    #expect(session.servingEnablementRefusal == "The nest refused to turn on email.")
}

@Test @MainActor func aRunWithNoRefusalLeavesThePageAlone() {
    let session = SessionState()
    MailEnableGlue.applyServingEnablementRefusal(nil, session: session)
    #expect(session.servingEnablementRefusal == nil)
    session.servingEnablementRefusal = "shown earlier"
    MailEnableGlue.applyServingEnablementRefusal(nil, session: session)
    #expect(session.servingEnablementRefusal == "shown earlier")
}
#endif
