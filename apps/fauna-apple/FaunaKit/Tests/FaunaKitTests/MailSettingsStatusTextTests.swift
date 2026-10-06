import Testing
@testable import FaunaKit

// Pins the `mail-settings-status-indicator` text derivation against the
// cross-app contract (linux `settings/mail.rs`, web `MailSettingsSection`):
// an `Idle` status reads "All up to date" ONLY when mail is enabled; a disabled
// mailbox reads "Mail is disabled". Apple previously returned "All up to date"
// for `Idle` regardless of `enabled`, which (a) diverged from every other app
// + lied to the user, and (b) made the e2e's `wait_for_enabled_status` poll
// false-positive on an off mailbox → `ensure_mail_enabled` skipped the enable
// gesture → the whole `if vm.enabled` block (credentials/manage/serve/mua) never
// rendered → `test_mail_credentials.py` 2p/6f. This unit test reproduces the
// off-mailbox case the old inline logic got wrong and guards the fix.
struct MailSettingsStatusTextTests {
    @Test func disabledIdleReadsDisabled() {
        #expect(mailStatusText(enabled: false, status: .idle) == L.settings.mail.statusDisabled)
    }

    @Test func enabledIdleReadsUpToDate() {
        #expect(mailStatusText(enabled: true, status: .idle) == L.settings.mail.statusEnabled)
    }

    @Test func syncingReadsSyncing() {
        // Syncing is enabled-only in practice; the string is enable-independent.
        #expect(mailStatusText(enabled: true, status: .syncing) == L.settings.mail.statusSyncing)
    }

    @Test func rotationReadsRemainingCount() {
        #expect(
            mailStatusText(enabled: true, status: .rotationInProgress(credentialsRemaining: 3))
                == L.settings.mail.statusRotation(count: "3")
        )
    }
}
