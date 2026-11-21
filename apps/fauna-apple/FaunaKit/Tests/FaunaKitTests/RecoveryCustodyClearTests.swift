import Testing
@testable import FaunaKit
import FaunaFFISwift

// The custody leg's cleared mark (`box-recovery.md` § The plane-era recovery floor,
// (c) The writes): a later run that confirms custody takes the warning down, but only
// while the banner still shows a custody warning. Twin of android's
// `PostAuthGlueVMTest`. Drives the sink's main-actor appliers directly, no network.

@Test @MainActor func custodyWarningThenClearedMarkTakesTheBannerDown() throws {
    let session = SessionState()
    let custody = try #require(deploymentSeedCustodyWarnings().first)
    MailEnableGlue.applyCustodyWarning(custody, session: session)
    #expect(session.recoveryCustodyWarning == renderLocalizedText(custody))
    MailEnableGlue.applyCustodyCleared(session: session)
    #expect(session.recoveryCustodyWarning == nil)
}

@Test @MainActor func clearedMarkLeavesAnUnrelatedWarningStanding() {
    let session = SessionState()
    session.recoveryCustodyWarning = "some unrelated warning"
    MailEnableGlue.applyCustodyCleared(session: session)
    #expect(session.recoveryCustodyWarning == "some unrelated warning")
}

@Test @MainActor func clearedMarkWithNoBannerIsANoOp() {
    let session = SessionState()
    MailEnableGlue.applyCustodyCleared(session: session)
    #expect(session.recoveryCustodyWarning == nil)
}
