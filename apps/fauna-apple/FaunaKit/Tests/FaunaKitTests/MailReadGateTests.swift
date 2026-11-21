import Testing
@testable import FaunaKit

// The Mail & Calendar and Mail spam controls that write what a read decides
// wait for that read on the apple apps (`docs/goal/ui/mail-settings.md`
// § Errors & edge cases, *Controls wait for their reads*): each is blocked
// *still loading* before its read, *failed to load* after a failed one (a
// failure after a success keeps the earlier answer), and its press refuses
// with the same reason on `error-message`, sending nothing. One suite for
// macos and ios: both read the one FaunaKit view model each page has.

private func settings(read: Bool, enabled: Bool = false, unavailable: Bool = false) -> MailSettingsSnapshot {
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

private func reasonKey(_ answer: Possibility) -> String? {
    if case .blocked(let reason) = answer { return reason.key }
    return nil
}

private let pending = readPossibility(read: .pending)
private let failed = readPossibility(read: .failed)

@Suite @MainActor struct MailReadGateTests {
    // MARK: Mail & Calendar — the enable toggle

    @Test func theEnableToggleWaitsBeforeTheMachineExists() {
        #expect(MailSettingsVM().enableAnswer == pending)
    }

    @Test func theEnableToggleWaitsForTheFirstHydrate() {
        let vm = MailSettingsVM()
        vm.snapshot = settings(read: false)
        #expect(vm.enableAnswer == pending)
        #expect(!vm.enableAnswer.isPossible && vm.enableAnswer.isShown)
    }

    @Test func theEnableToggleBlocksATurnOnWhereMailCanNeverBeServed() {
        let vm = MailSettingsVM()
        vm.snapshot = settings(read: true, unavailable: true)
        #expect(reasonKey(vm.enableAnswer) == "mail_settings.unavailable_notice")
    }

    @Test func theEnableToggleIsLiveOnceRead() {
        let vm = MailSettingsVM()
        vm.snapshot = settings(read: true)
        #expect(vm.enableAnswer == .possible)
        vm.snapshot = settings(read: true, enabled: true, unavailable: true)
        #expect(vm.enableAnswer == .possible)
    }

    // MARK: Mail & Calendar — forward-all, the hourly limit, the automatic reply

    @Test func theForwardingAndReplyControlsWaitForTheirReads() {
        let vm = MailSettingsVM()
        #expect(vm.forwardAllAnswer == pending)
        #expect(vm.forwardPerHourAnswer == pending)
        #expect(vm.autorespondAnswer == pending)
    }

    @Test func aFailedReadBlocksAndAFailureAfterASuccessKeepsTheAnswer() {
        let vm = MailSettingsVM()
        vm.forwardAllRead.failed()
        vm.autorespondRead.failed()
        #expect(vm.forwardAllAnswer == failed)
        #expect(vm.autorespondAnswer == failed)
        vm.forwardAllRead.resolved()
        vm.forwardAllRead.failed()
        #expect(vm.forwardAllAnswer == .possible)
        #expect(vm.autorespondAnswer == failed)
    }

    @Test func aPreReadForwardAllCommitRefusesAndSendsNothing() async {
        let vm = MailSettingsVM()
        vm.forwardAllToDraft = ""
        await vm.commitForwardAllTo()
        // The read's reason, not "not connected": nothing was asked of a nest.
        #expect(vm.errorMessage == pending.refusal)
        #expect(vm.forwardAllToDraft == "")
    }

    @Test func aPreReadForwardPerHourCommitRefuses() async {
        let vm = MailSettingsVM()
        vm.forwardPerHourDraft = "5"
        await vm.commitForwardPerHour()
        #expect(vm.errorMessage == pending.refusal)
    }

    @Test func aPreReadReplyToggleOrMessageRefuses() async {
        let vm = MailSettingsVM()
        vm.autorespondMessageDraft = "away"
        await vm.commitAutorespondMessage()
        #expect(vm.errorMessage == pending.refusal)
        vm.errorMessage = nil
        await vm.toggleAutorespond()
        #expect(vm.errorMessage == pending.refusal)
        vm.autorespondRead.failed()
        await vm.toggleAutorespond()
        #expect(vm.errorMessage == failed.refusal)
    }

    @Test func aResolvedReadLetsThePressThrough() async {
        let vm = MailSettingsVM()
        vm.forwardAllRead.resolved()
        await vm.commitForwardAllTo()
        // Past the read gate, the press reaches the next check: no nest here.
        #expect(vm.errorMessage == L.errors.notConnectedToNest)
    }

    // MARK: Mail spam — contribute, share reports, threshold override

    @Test func theSpamControlsWaitForTheirReads() {
        let vm = MailSpamVM()
        #expect(vm.contributeAnswer == pending)
        #expect(vm.shareAnswer == pending)
        #expect(vm.thresholdAnswer == pending)
    }

    @Test func theSpamControlsFollowTheirOwnReads() {
        let vm = MailSpamVM()
        vm.classifierRead.resolved()
        vm.shareRead.failed()
        #expect(vm.contributeAnswer == .possible)
        #expect(vm.shareAnswer == failed)
        #expect(vm.thresholdAnswer == pending)
        vm.thresholdRead.resolved()
        vm.thresholdRead.failed()
        #expect(vm.thresholdAnswer == .possible)
    }

    @Test func aPreReadShareOrThresholdPressRefusesAndSendsNothing() async {
        let vm = MailSpamVM()
        await vm.setReportShare(true)
        #expect(vm.errorMessage == pending.refusal)
        #expect(vm.reportShare == false)
        vm.errorMessage = nil
        // An empty pre-read draft is never committed as "clear the override".
        await vm.commitThresholdOverride(text: "")
        #expect(vm.errorMessage == pending.refusal)
        vm.thresholdRead.failed()
        await vm.commitThresholdOverride(text: "")
        #expect(vm.errorMessage == failed.refusal)
    }

    @Test func aContributePressWithNoMachineIsTheUnbuiltRefusal() async {
        let vm = MailSpamVM()
        await vm.setContributeBaseline(true)
        #expect(vm.errorMessage == mailUsableUnbuilt().refusal)
    }
}
