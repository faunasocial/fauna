import Testing
import Foundation
@testable import FaunaKit

/// Unit coverage for the deployment-seed custody leg's outcome → warning mapping
/// (`MailEnableGlue.recoveryCustodyWarning(for:)`) — the apple projection of the
/// shared `selfHealDeploymentSeedCustody` outcome onto `RecoveryCustodyBanner`
/// (`box-recovery.md` § The plane-era recovery floor, (c) The writes). Pins that
/// every *unconfirmed* outcome raises a user-visible warning, so the apple glue
/// never silently drops custody, and never false-alarms
/// on the expected multi-nest no-op (BR-1). Mirrors android's
/// `RecoveryCustodyOutcome` mapping; a thrown leg is the caller's "failed".

@Test func silentOutcomesProduceNoWarning() {
    // Already custodied, not an admin, and a capture that wrote / was idempotent /
    // hit the expected multi-nest no-op (BR-1) are all silent — no banner.
    #expect(MailEnableGlue.recoveryCustodyWarning(for: .alreadyCustodied) == nil)
    #expect(MailEnableGlue.recoveryCustodyWarning(for: .notAdmin) == nil)
    #expect(MailEnableGlue.recoveryCustodyWarning(for: .captured(.wrote)) == nil)
    #expect(MailEnableGlue.recoveryCustodyWarning(for: .captured(.alreadyHeldSame)) == nil)
    #expect(MailEnableGlue.recoveryCustodyWarning(for: .captured(.refusedDiffering)) == nil)
}

@Test func mismatchRaisesTheLoudWarning() {
    // BR-2: the nest handed off a seed that does not derive to its pinned identity —
    // recovery is not protected, warn loudly.
    #expect(
        MailEnableGlue.recoveryCustodyWarning(for: .captured(.refusedMismatch))
            == L.launch.recoveryCustodyMismatch)
    // And it is the *mismatch* string, distinct from the failed string.
    #expect(
        MailEnableGlue.recoveryCustodyWarning(for: .captured(.refusedMismatch))
            != L.launch.recoveryCustodyFailed)
}

@Test func unconfirmedHandoffRaisesTheFailedWarning() {
    // The hand-off round trip was unavailable, or the nest holds no seed: custody is
    // not confirmed, so the admin is told rather than left believing it is protected.
    #expect(
        MailEnableGlue.recoveryCustodyWarning(for: .handoffUnavailable)
            == L.launch.recoveryCustodyFailed)
    #expect(
        MailEnableGlue.recoveryCustodyWarning(for: .nestHoldsNoSeed)
            == L.launch.recoveryCustodyFailed)
}
