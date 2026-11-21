import Testing
@testable import FaunaKit

#if !FAUNA_KIDS
// The mail-export, mail-import and archive-import wizards on the apple apps
// paint Next, Start and the archive profile prefill from the shared answers
// (`docs/goal/ui/README.md` § Offer only what is possible; `mail-export.md`
// § Wizard steps, `mailbox-migration.md` § *Next and Start offer only an
// import that can start*, `archive-import.md` § The wizard and its machine),
// and a press asks the same answer and refuses on `error-message` with its
// reason — nothing reaches a machine (none is built here). One suite for
// macos and ios: both read the one FaunaKit view model.

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

private func exportSnapshot(selected: Bool, from: String = "", to: String = "") -> MailExportSnapshot {
    MailExportSnapshot(
        step: .scope, format: .mbox,
        mailboxes: [MailboxOption(name: "INBOX", selected: selected, messageCount: 1, sealedBytes: 0)],
        dateFrom: from, dateTo: to, stripHeaders: false, sessionState: nil,
        exportedCount: 0, skippedCount: 0, erroredCount: 0, totalCount: 0,
        mailboxProgress: [], errorLog: [], blobBytes: nil, downloadUrl: "",
        savedArchivePath: "", startedAt: 0, lastProgressAt: 0, status: .idle, error: nil
    )
}

private func importSnapshot(selected: Bool) -> MailImportSnapshot {
    MailImportSnapshot(
        step: .scope, sourceKind: .gmail, host: "", port: 993, tlsMode: .implicit,
        username: "", password: "",
        mailboxes: [SourceMailboxOption(name: "INBOX", selected: selected, messageCount: 1)],
        dateFrom: "", maxSizeBytes: 0,
        importDuplicates: false, sessionState: nil, importedCount: 0, skippedCount: 0,
        erroredCount: 0, totalCount: 0, errorLog: [], mailboxProgress: [],
        startedAt: 0, lastProgressAt: 0, status: .idle, error: nil, pauseReason: nil,
        rebuiltMailboxes: [], resumeNeedsCredentials: false,
        microsoftSigninAvailable: false, microsoftSigninTransport: false,
        microsoftSignin: nil, sourceRemembered: false
    )
}

private let anArchive = ArchiveSummaryView(
    platformLabel: "Instagram", ownerDisplayName: "someone", firstAt: 0, lastAt: 0,
    mediaBytes: 0, archiveBytes: 0, knownAudience: 0, unknownAudience: 0)

private let aProfile = ArchiveProfileView(displayName: "someone", bio: nil, links: [], hasPicture: false)

private func archiveSnapshot(
    step: ArchiveImportStep, summary: ArchiveSummaryView? = nil,
    from: String = "", to: String = "",
    profile: ArchiveProfileView? = nil, applied: Bool = false
) -> ArchiveImportSnapshot {
    ArchiveImportSnapshot(
        step: step, sourceKind: .instagram, sourceExportUrl: "", archivePath: "",
        summary: summary, categories: [], audienceMode: .original,
        dateFrom: from, dateTo: to, confirmRecords: 0, confirmBytes: 0,
        folderName: nil, runState: nil, total: 0, imported: 0, skipped: 0, errored: 0,
        currentCategory: nil, elapsedSecs: 0, etaSecs: nil, skipLog: [],
        nestSupportsHiddenTiers: nil, resumeAvailable: false, status: .idle, error: nil,
        linkClaimed: 0, linkUnmatched: 0, commentsUnplaced: 0,
        archiveProfile: profile, profileApplied: applied
    )
}

@Suite @MainActor struct ImportExportWizardPossibilityTests {
    // MARK: mail export

    @Test func exportNextAndStartWaitForTheFirstSnapshot() {
        let vm = MailExportVM()
        #expect(reasonKey(vm.nextAnswer) == "common.still_loading")
        #expect(reasonKey(vm.startAnswer) == "common.still_loading")
    }

    @Test func exportWithNoMailboxBlocksNextAndStartAndThePressRefuses() async {
        let vm = MailExportVM()
        vm.snapshot = exportSnapshot(selected: false)
        #expect(reasonKey(vm.nextAnswer) == "mail_export.scope_needs_mailbox")
        #expect(reasonKey(vm.startAnswer) == "mail_export.scope_needs_mailbox")
        await vm.pressNext()
        #expect(vm.errorMessage == vm.nextAnswer.refusal)
        #expect(vm.errorMessage?.isEmpty == false)
        vm.errorMessage = nil
        await vm.pressStart()
        #expect(vm.errorMessage == vm.startAnswer.refusal)
    }

    @Test func exportMalformedOrInvertedDatesBlockNextAndStart() {
        let vm = MailExportVM()
        vm.snapshot = exportSnapshot(selected: true, from: "2026-13-40")
        #expect(!vm.nextAnswer.isPossible && vm.nextAnswer.isShown)
        #expect(!vm.startAnswer.isPossible)
        vm.snapshot = exportSnapshot(selected: true, from: "2026-05-01", to: "2026-01-01")
        #expect(!vm.nextAnswer.isPossible && vm.nextAnswer.isShown)
        #expect(!vm.startAnswer.isPossible)
    }

    @Test func exportWithAMailboxAndAGoodRangeIsPossible() {
        let vm = MailExportVM()
        vm.snapshot = exportSnapshot(selected: true, from: "2026-01-01", to: "2026-05-01")
        #expect(vm.nextAnswer == .possible)
        #expect(vm.startAnswer == .possible)
    }

    // MARK: mail import

    @Test func importNextAndStartWaitForTheFirstSnapshot() {
        let vm = MailImportVM()
        #expect(reasonKey(vm.nextAnswer(dateFrom: "")) == "common.still_loading")
        #expect(reasonKey(vm.startAnswer) == "common.still_loading")
    }

    @Test func importWithNoMailboxBlocksNextAndStartAndThePressRefuses() async {
        let vm = MailImportVM()
        vm.snapshot = importSnapshot(selected: false)
        #expect(reasonKey(vm.nextAnswer(dateFrom: "")) == "mail_import.scope_needs_mailbox")
        #expect(reasonKey(vm.startAnswer) == "mail_import.scope_needs_mailbox")
        await vm.pressScopeNext(dateFrom: "", maxSizeInput: "")
        #expect(vm.errorMessage == vm.nextAnswer(dateFrom: "").refusal)
        #expect(vm.errorMessage?.isEmpty == false)
        vm.errorMessage = nil
        await vm.pressStart()
        #expect(vm.errorMessage == vm.startAnswer.refusal)
    }

    @Test func importNextIsAskedOverTheTypedSinceDateDraft() async {
        let vm = MailImportVM()
        vm.snapshot = importSnapshot(selected: true)
        #expect(vm.nextAnswer(dateFrom: "") == .possible)
        #expect(vm.nextAnswer(dateFrom: " 2026-01-01 ") == .possible)
        let bad = vm.nextAnswer(dateFrom: "not-a-date")
        #expect(reasonKey(bad) == "mail_import.scope_date_from_invalid")
        await vm.pressScopeNext(dateFrom: "not-a-date", maxSizeInput: "")
        #expect(vm.errorMessage == bad.refusal)
    }

    // MARK: archive import

    @Test func archiveAnswersWaitForTheFirstSnapshot() {
        let vm = ArchiveImportVM()
        #expect(reasonKey(vm.nextAnswer(dateFrom: "", dateTo: "")) == "common.still_loading")
        #expect(reasonKey(vm.startAnswer) == "common.still_loading")
        #expect(reasonKey(vm.profilePrefillAnswer) == "common.still_loading")
    }

    @Test func archiveNextWaitsForAnOpenedArchiveAndThePressRefuses() async {
        let vm = ArchiveImportVM()
        vm.snapshot = archiveSnapshot(step: .archive)
        let blocked = vm.nextAnswer(dateFrom: "", dateTo: "")
        #expect(!blocked.isPossible && blocked.isShown)
        await vm.pressNext([.next], dateFrom: "", dateTo: "")
        #expect(vm.errorMessage == blocked.refusal)
        #expect(vm.errorMessage?.isEmpty == false)
        vm.snapshot = archiveSnapshot(step: .archive, summary: anArchive)
        #expect(vm.nextAnswer(dateFrom: "", dateTo: "") == .possible)
    }

    @Test func archiveScopeNextIsAskedOverTheTypedDateDrafts() {
        let vm = ArchiveImportVM()
        vm.snapshot = archiveSnapshot(step: .scope, summary: anArchive)
        #expect(vm.nextAnswer(dateFrom: "2026-01-01", dateTo: "2026-05-01") == .possible)
        #expect(!vm.nextAnswer(dateFrom: "2026-05-01", dateTo: "2026-01-01").isPossible)
        #expect(!vm.nextAnswer(dateFrom: "garbage", dateTo: "").isPossible)
    }

    @Test func archiveStartNeedsAnOpenedArchiveAndAResolvingRange() async {
        let vm = ArchiveImportVM()
        vm.snapshot = archiveSnapshot(step: .confirm)
        #expect(!vm.startAnswer.isPossible)
        await vm.pressStart()
        #expect(vm.errorMessage == vm.startAnswer.refusal)
        vm.snapshot = archiveSnapshot(step: .confirm, summary: anArchive, from: "2026-05-01", to: "2026-01-01")
        #expect(!vm.startAnswer.isPossible)
        vm.snapshot = archiveSnapshot(step: .confirm, summary: anArchive)
        #expect(vm.startAnswer == .possible)
    }

    @Test func archivePrefillIsBlockedWithNoProfileAndOnceApplied() async {
        let vm = ArchiveImportVM()
        vm.snapshot = archiveSnapshot(step: .done)
        #expect(reasonKey(vm.profilePrefillAnswer) == "archive_import.profile_prefill_none")
        await vm.pressApplyProfile()
        #expect(vm.errorMessage == vm.profilePrefillAnswer.refusal)
        vm.snapshot = archiveSnapshot(step: .done, profile: aProfile, applied: true)
        #expect(reasonKey(vm.profilePrefillAnswer) == "archive_import.profile_prefill_applied")
        vm.snapshot = archiveSnapshot(step: .done, profile: aProfile)
        #expect(vm.profilePrefillAnswer == .possible)
    }
}
#endif
