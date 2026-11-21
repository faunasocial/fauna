import Testing
@testable import FaunaKit

// The mail pages' in-page predicates on the apple apps (`docs/goal/ui/
// mail-settings.md` § Errors & edge cases, *The in-page predicates*): the
// credential section, the Outlook import source and the alias / list create
// paths each paint the shared answer, carried whole over UniFFI — never the
// bare field it is computed from — and a gesture reaching a mail page with no
// machine refuses with `mail_settings.usable_unbuilt` on `error-message`.
// One suite for macos and ios: both read the one FaunaKit view model.

private func settings(read: Bool, reachable: Bool, unavailable: Bool = false) -> MailSettingsSnapshot {
    MailSettingsSnapshot(
        enabled: reachable,
        caldavEnabled: false,
        carddavEnabled: false,
        servesWebdavSet: false,
        credentialManagementReachable: reachable,
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

private func importSnapshot(transport: Bool, available: Bool) -> MailImportSnapshot {
    MailImportSnapshot(
        step: .source, sourceKind: .outlook, host: "", port: 993, tlsMode: .implicit,
        username: "", password: "", mailboxes: [], dateFrom: "", maxSizeBytes: 0,
        importDuplicates: false, sessionState: nil, importedCount: 0, skippedCount: 0,
        erroredCount: 0, totalCount: 0, errorLog: [], mailboxProgress: [],
        startedAt: 0, lastProgressAt: 0, status: .idle, error: nil, pauseReason: nil,
        rebuiltMailboxes: [], resumeNeedsCredentials: false,
        microsoftSigninAvailable: available, microsoftSigninTransport: transport,
        microsoftSignin: nil, sourceRemembered: false
    )
}

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

@Suite @MainActor struct MailInPagePossibilityTests {
    // MARK: credential section

    @Test func theCredentialSectionIsAbsentBeforeTheFirstRead() {
        let vm = MailSettingsVM()
        #expect(vm.credentialManagementAnswer == .absent)
    }

    @Test func theCredentialSectionIsAbsentOnABoxThatCanNeverServeMail() {
        let answer = mailSettingsCredentialManagementPossibility(
            snapshot: settings(read: true, reachable: false, unavailable: true))
        #expect(answer == .absent)
    }

    @Test func theCredentialSectionIsBlockedNamingTheEnableToggleWhileMailIsOff() {
        let answer = mailSettingsCredentialManagementPossibility(
            snapshot: settings(read: true, reachable: false))
        #expect(reasonKey(answer) == "mail_settings.credentials_need_mail_on")
        #expect(answer.isShown && !answer.isPossible)
    }

    @Test func theCredentialSectionIsPossibleOnceACredentialCanBeUsed() {
        let answer = mailSettingsCredentialManagementPossibility(
            snapshot: settings(read: true, reachable: true))
        #expect(answer == .possible)
    }

    // MARK: Outlook source

    @Test func theOutlookSourceIsWaitingBeforeTheFirstSnapshot() {
        let vm = MailImportVM()
        #expect(reasonKey(vm.outlookAnswer) == "common.still_loading")
    }

    @Test func aBuildThatCanNeverSignInOffersNoOutlookSource() {
        let answer = mailImportOutlookSourcePossibility(
            snapshot: importSnapshot(transport: false, available: false))
        #expect(answer == .absent)
    }

    @Test func theOutlookSignInIsBlockedUntilARegistrationIsSaved() {
        let answer = mailImportOutlookSourcePossibility(
            snapshot: importSnapshot(transport: true, available: false))
        #expect(reasonKey(answer) == "mail_import.microsoft_unavailable")
        #expect(mailImportOutlookSourcePossibility(
            snapshot: importSnapshot(transport: true, available: true)) == .possible)
    }

    // MARK: alias and list create

    @Test func aliasCreateIsWaitingThenBlockedWithoutADomainThenPossible() {
        #expect(reasonKey(MailAliasesVM().createAnswer) == "common.still_loading")
        let without = MailAliasesSnapshot(
            aliases: [], defaultDomain: nil, lastMintedAddress: nil,
            lastImportResult: nil, status: .idle, error: nil, audit: nil)
        #expect(reasonKey(mailAliasesCreatePossibility(snapshot: without)) == "mail_aliases.no_default_domain")
        var with = without
        with.defaultDomain = "example.test"
        #expect(mailAliasesCreatePossibility(snapshot: with) == .possible)
    }

    @Test func listCreateIsWaitingThenBlockedWithoutADomainThenPossible() {
        #expect(reasonKey(MailListsVM().createAnswer) == "common.still_loading")
        let without = MailListsSnapshot(lists: [], localDomains: [], status: .idle, error: nil)
        #expect(reasonKey(mailListsCreatePossibility(snapshot: without)) == "mail_lists.no_domain")
        let with = MailListsSnapshot(lists: [], localDomains: ["example.test"], status: .idle, error: nil)
        #expect(mailListsCreatePossibility(snapshot: with) == .possible)
    }

    // MARK: the forms' drafts (extended 2026-11-17)

    @Test func theListSubmitNeedsALocalPartAndDomainOnCreateAndAWholeNumberCap() {
        #expect(reasonKey(listSubmitPossibility(editing: false, localPart: " ", domain: "example.test", perSendCap: ""))
                == "mail_lists.local_part_required")
        #expect(reasonKey(listSubmitPossibility(editing: false, localPart: "team", domain: "", perSendCap: ""))
                == "mail_lists.domain_required")
        #expect(reasonKey(listSubmitPossibility(editing: true, localPart: "", domain: "", perSendCap: "lots"))
                == "mail_lists.per_send_invalid")
        #expect(listSubmitPossibility(editing: false, localPart: "team", domain: "example.test", perSendCap: "25")
                == .possible)
        #expect(listSubmitPossibility(editing: true, localPart: "", domain: "", perSendCap: "") == .possible)
    }

    @Test func theMemberSubmitsAreBlockedWithNothingTyped() {
        #expect(reasonKey(memberAddPossibility(address: "  ")) == "mail_lists.member_address_required")
        #expect(memberAddPossibility(address: "a@example.test") == .possible)
        #expect(reasonKey(memberImportPossibility(paste: " \n\n")) == "mail_lists.member_import_required")
        #expect(memberImportPossibility(paste: "\na@example.test\n") == .possible)
    }

    @Test func rotateKeysWaitsForTheReadThenIsPossible() {
        let vm = MailSettingsVM()
        #expect(reasonKey(vm.rotateKeysAnswer) == "common.still_loading")
        vm.snapshot = settings(read: true, reachable: true)
        #expect(vm.rotateKeysAnswer == .possible)
    }

    @Test func rotateKeysIsBlockedWhileARotationWaitsToResume() {
        let vm = MailSettingsVM()
        var snap = settings(read: true, reachable: true)
        snap.pendingRotation = PendingRotationStatus(credentialsRemaining: ["c1"])
        vm.snapshot = snap
        #expect(reasonKey(vm.rotateKeysAnswer) == "mail_settings.rotate_pending")
    }

    @Test func rotateKeysIsStillSavingWhileARotationRuns() {
        let vm = MailSettingsVM()
        vm.snapshot = settings(read: true, reachable: true)
        vm.rotationInFlight = true
        #expect(reasonKey(vm.rotateKeysAnswer) == "common.in_flight")
        vm.rotationInFlight = false
        var snap = settings(read: true, reachable: true)
        snap.status = .rotationInProgress(credentialsRemaining: 2)
        vm.snapshot = snap
        #expect(reasonKey(vm.rotateKeysAnswer) == "common.in_flight")
    }

    @Test func aBlockedRotateConfirmRefusesWithItsReasonAndSendsNothing() async {
        let vm = MailSettingsVM()
        var snap = settings(read: true, reachable: true)
        snap.pendingRotation = PendingRotationStatus(credentialsRemaining: ["c1"])
        vm.snapshot = snap
        await vm.startRotation(excludedCredentials: [])
        #expect(vm.errorMessage == vm.rotateKeysAnswer.refusal)
        #expect(vm.errorMessage?.isEmpty == false)
        #expect(vm.rotationInFlight == false)
    }

    // MARK: rule 7 — no machine, never silent

    @Test func aGestureOnAMailPageWithNoMachineRefusesWithTheUnbuiltReason() async {
        let unbuilt = mailUsableUnbuilt().refusal
        #expect(unbuilt?.isEmpty == false)

        let lists = MailListsVM()
        await lists.dispatch(.refresh)
        #expect(lists.errorMessage == unbuilt)

        let export = MailExportVM()
        await export.dispatch(.download)
        #expect(export.errorMessage == unbuilt)

        let mailImport = MailImportVM()
        await mailImport.dispatch(.connect)
        #expect(mailImport.errorMessage == unbuilt)
    }
}
