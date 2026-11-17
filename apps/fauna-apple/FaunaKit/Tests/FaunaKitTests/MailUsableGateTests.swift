import Testing
@testable import FaunaKit

// The mail-usable gate on the apple apps (`docs/goal/ui/mail-settings.md`
// § Errors & edge cases, the mail-usable gate): the six mail sub-pages and
// their Settings rail rows paint the shared `mail_usable()` answer, carried
// whole over UniFFI, in each of the four states the predicate tells apart —
// never read, read with mail off, read with mail on, a box that can never serve
// mail — plus a machine whose build failed. One suite for macos and ios: both
// rails and both page routers read the one FaunaKit `MailUsableGate`.

private func snapshot(read: Bool, enabled: Bool, unavailable: Bool = false) -> MailSettingsSnapshot {
    MailSettingsSnapshot(
        enabled: enabled,
        caldavEnabled: false,
        carddavEnabled: false,
        servesWebdavSet: false,
        credentialManagementReachable: enabled,
        servingEnabled: true,
        mailUnavailableHere: unavailable,
        read: read,
        credentials: [],
        pendingRotation: nil,
        mua: MuaInstructions(
            imapHost: "", imapPort: 0, smtpHost: "", smtpPort: 0,
            caldavHost: "", caldavPort: 0, webdavUrl: "", domain: "",
            usernameFormat: "", authMechanism: ""
        ),
        status: .idle,
        error: nil
    )
}

@Suite @MainActor struct MailUsableGateTests {
    @Test func aGateWithNoMachineYetIsBlockedWaiting() {
        let gate = MailUsableGate()
        #expect(gate.answer == mailUsableUnread())
        #expect(!gate.answer.isPossible)
        #expect(gate.answer.isShown)
        #expect(gate.answer.reasonText == L.mailSettings.usableReading)
    }

    @Test func unreadIsBlockedWaitingEvenWithMailOn() {
        let gate = MailUsableGate()
        gate.observe(snapshot(read: false, enabled: true))
        #expect(!gate.answer.isPossible)
        #expect(gate.answer.isShown)
        #expect(gate.answer.reasonText == L.mailSettings.usableReading)
        #expect(mailSubpageGateText(gate.answer(for: .mailAliases)) == L.mailSettings.usableReading)
    }

    @Test func offIsBlockedNamingTheMailAndCalendarStep() {
        let gate = MailUsableGate()
        gate.observe(snapshot(read: true, enabled: false))
        #expect(!gate.answer.isPossible)
        #expect(gate.answer.isShown)
        #expect(gate.answer.reasonText == L.mailSettings.usableNeedsMailOn)
        for page in MailUsableGate.gatedPages {
            #expect(mailSubpageGateText(gate.answer(for: page)) == L.mailSettings.usableNeedsMailOn)
        }
    }

    @Test func onIsPossibleAndTheSubpagesShowTheirContent() {
        let gate = MailUsableGate()
        gate.observe(snapshot(read: true, enabled: true))
        #expect(gate.answer == .possible)
        for page in MailUsableGate.gatedPages {
            #expect(mailSubpageGateText(gate.answer(for: page)) == nil)
        }
    }

    @Test func unavailableIsAbsentAndAnotherDoorShowsTheUnavailableNotice() {
        let gate = MailUsableGate()
        for (read, enabled) in [(false, false), (true, false), (true, true)] {
            gate.observe(snapshot(read: read, enabled: enabled, unavailable: true))
            #expect(gate.answer == .absent)
            #expect(!gate.answer.isShown)
            #expect(mailSubpageGateText(gate.answer(for: .mailSpam)) == L.mailSettings.unavailableNotice)
        }
    }

    @Test func anUnbuiltMachineIsBlockedWithItsOwnReasonNeverAbsent() {
        let gate = MailUsableGate()
        gate.observeUnbuilt()
        #expect(gate.answer.isShown)
        #expect(!gate.answer.isPossible)
        #expect(gate.answer.reasonText == L.mailSettings.usableUnbuilt)
    }

    @Test func onlyTheSixMailSubpagesAreGated() {
        let gate = MailUsableGate()
        gate.observe(snapshot(read: true, enabled: false))
        #expect(MailUsableGate.gatedPages == [
            .mailAliases, .mailSpam, .mailExport, .mailImport, .mailLists, .mailListMembers,
        ])
        // Mail & Calendar is the one step that unblocks the rest: it stays live.
        #expect(gate.answer(for: .mail) == .possible)
        #expect(gate.answer(for: .archiveImport) == .possible)
        #expect(gate.answer(for: .account) == .possible)
    }
}
