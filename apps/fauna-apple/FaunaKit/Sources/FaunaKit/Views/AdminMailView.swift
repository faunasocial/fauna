import SwiftUI

/// The flat admin **`admin-mail`** mail-policy page (`docs/goal/behavior/admin.md`
/// § 6 Mail + `docs/goal/behavior/mail-policy-config.md` § Policy catalog), shared
/// by macOS + iOS (one FaunaKit view, thin per-target mount points).
/// A dumb renderer of `MailPolicySnapshot` + dispatcher of
/// `MailPolicyAction` over the shared `MailPolicyMachine` (via `AdminMailVM`); no
/// policy logic here. Element IDs match `tests/e2e-unified/ui.yaml` `admin-mail`
/// exactly. Reference renderer: linux (`apps/fauna-linux/src/settings/admin_mail.rs`).
///
/// The page renders the **live write-path knobs only**: the two deployment-wide
/// toggles (`set_mail_enabled` / `set_auto_enable_mail_for_new_users`) plus the
/// seven full-PUT policy groups (`put_{spam,auth,submission,imap,outbound,alias}_
/// policy`). Automatic concerns (DKIM / TLS / MTA-STS / ACME / DMARC-publish /
/// scanning) have **no manual admin UI** — they auto-provision; DKIM/MX/SPF/DMARC
/// records render read-only on `admin-dns`. The inert Bucket-C catalog rows are not
/// surfaced (`mail-policy-config.md` § Policy catalog banner).
///
/// Each group's controls are read **on Save** (gather → full-PUT), mirroring linux:
/// integer/list fields hold a string edit buffer parsed on Save with a fall-back to
/// the persisted value (a stray edit never silently zeroes a knob), and a save
/// re-reads the whole effective config so every group reflects persisted state.
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation
/// stack (iOS), not this page.
public struct AdminMailView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminMailVM()
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) { self.reloadToken = reloadToken }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.admin.mailPage.title)
                    .font(.title)
                    .accessibilityIdentifier(Ids.pageHeading)
                Text(L.admin.mailPage.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                if let snap = vm.snapshot {
                    enableGroup(snap)
                    MailSpamPolicySection(policy: snap.spam, reloadToken: vm.snapshotVersion,
                                          isBusy: vm.isBusy, baselineResult: snap.baselinePublishResult,
                                          onPublish: { await vm.publishSpamBaseline() }) { await vm.saveSpam($0) }
                    MailAuthPolicySection(policy: snap.auth, reloadToken: vm.snapshotVersion,
                                          isBusy: vm.isBusy) { await vm.saveAuth($0) }
                    MailSubmissionPolicySection(policy: snap.submission, reloadToken: vm.snapshotVersion,
                                                isBusy: vm.isBusy) { await vm.saveSubmission($0) }
                    MailImapPolicySection(policy: snap.imap, reloadToken: vm.snapshotVersion,
                                          isBusy: vm.isBusy) { await vm.saveImap($0) }
                    MailOutboundPolicySection(policy: snap.outbound, reloadToken: vm.snapshotVersion,
                                              isBusy: vm.isBusy) { await vm.saveOutbound($0) }
                    MailAliasPolicySection(policy: snap.alias, reloadToken: vm.snapshotVersion,
                                           isBusy: vm.isBusy) { await vm.saveAlias($0) }
                } else {
                    ProgressView().frame(maxWidth: .infinity)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
    }

    // MARK: - Deployment-wide toggles (dispatch-on-change)

    private func enableGroup(_ snap: MailPolicySnapshot) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Toggle(L.admin.mailPage.enabledLabel, isOn: Binding(
                    get: { snap.mailEnabled },
                    set: { on in Task { await vm.setMailEnabled(on) } }
                ))
                .accessibilityIdentifier(Ids.adminMailEnabledToggle)
                // Read the LIVE snapshot (not the captured `snap`) and flip via the
                // SAME VM dispatch the Toggle's setter runs.
                .automationActivate(
                    Ids.adminMailEnabledToggle,
                    value: { (vm.snapshot?.mailEnabled ?? false) ? "on" : "off" }
                ) {
                    let next = !(vm.snapshot?.mailEnabled ?? false)
                    Task { await vm.setMailEnabled(next) }
                }
                // Dispatch-on-change (no Save), so the toggle itself is the write.
                .faunaGate("fauna.bridges.set_mail_enabled")
                Text(L.admin.mailPage.enabledSubtitle)
                    .font(.caption).foregroundStyle(.secondary)

                Divider()

                Toggle(L.admin.mailPage.autoEnableNewUsersLabel, isOn: Binding(
                    get: { snap.autoEnableMailForNewUsers },
                    set: { on in Task { await vm.setAutoEnableMailForNewUsers(on) } }
                ))
                .accessibilityIdentifier(Ids.adminMailAutoEnableNewUsersToggle)
                .automationActivate(
                    Ids.adminMailAutoEnableNewUsersToggle,
                    value: { (vm.snapshot?.autoEnableMailForNewUsers ?? false) ? "on" : "off" }
                ) {
                    let next = !(vm.snapshot?.autoEnableMailForNewUsers ?? false)
                    Task { await vm.setAutoEnableMailForNewUsers(next) }
                }
                .faunaGate("fauna.bridges.set_auto_enable_mail_for_new_users")
                Text(L.admin.mailPage.autoEnableNewUsersSubtitle)
                    .font(.caption).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .disabled(vm.isBusy)
    }
}

// MARK: - Shared row helpers (file-private; reused by every policy section)

/// A captioned multiline editor (one value per line) for the list-valued knobs
/// (dnsbl servers, retry schedule, transient-5xx codes, reserved local-parts).
@ViewBuilder
private func policyMultiline(_ label: String, _ id: String, _ text: Binding<String>,
                             subtitle: String? = nil) -> some View {
    VStack(alignment: .leading, spacing: 3) {
        Text(label)
        TextEditor(text: text)
            .font(.body.monospaced())
            .frame(minHeight: 72)
            .padding(4)
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(.secondary.opacity(0.3)))
            .accessibilityIdentifier(id)
            .automationField(id, text: text)
        if let subtitle {
            Text(subtitle).font(.caption).foregroundStyle(.secondary)
        }
    }
}

/// A toggle row carrying its ui.yaml id (read on Save, not dispatch-on-change).
@ViewBuilder
private func policyToggle(_ label: String, _ id: String, _ isOn: Binding<Bool>) -> some View {
    Toggle(label, isOn: isOn)
        .accessibilityIdentifier(id)
        // Read the live bound state ("on"/"off") and flip the SAME binding the
        // Toggle drives — gathered on the group's Save (not dispatch-on-change).
        .automationActivate(id, value: { isOn.wrappedValue ? "on" : "off" }) {
            isOn.wrappedValue.toggle()
        }
}

/// A group container: titled `GroupBox` with a description, the rows, and a
/// trailing per-group Save button (full-PUT). Disabled while a round-trip runs.
///
/// `saveKind` is the wire kind the Save issues — the offline gate's input
/// (`Core/OfflineGate.swift`). It is a parameter rather than a constant because
/// each group full-PUTs its own policy sub-struct under its own kind, and it is
/// **required** so a seventh group cannot be added without answering the offline
/// question: this is the local stand-in for tui's exhaustive `Action::wire_kind`
/// match. Only the Save gates — editing a group's fields is a local buffer edit
/// that works fine with no nest.
@ViewBuilder
private func policyGroup<Content: View>(
    title: String, desc: String, saveLabel: String, saveId: String, saveKind: String,
    isBusy: Bool, onSave: @escaping () -> Void,
    @ViewBuilder content: () -> Content
) -> some View {
    GroupBox(title) {
        VStack(alignment: .leading, spacing: 10) {
            Text(desc).font(.caption).foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
            content()
            HStack {
                Spacer()
                Button(saveLabel) { onSave() }
                    .disabled(isBusy)
                    .accessibilityIdentifier(saveId)
                    .automationActivate(saveId, isEnabled: { !isBusy }) { onSave() }
                    .faunaGate(saveKind)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Parse helpers (Save-time; mirror linux gather_*)

private func parseLines(_ s: String) -> [String] {
    s.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
}

/// Parse one `UInt64` per non-blank line; fall back to the whole persisted list if
/// any line fails (full-PUT replaces the list — never send a partially parsed one).
private func parseU64Lines(_ s: String, fallback: [UInt64]) -> [UInt64] {
    let lines = parseLines(s)
    let parsed = lines.compactMap { UInt64($0) }
    return parsed.count == lines.count ? parsed : fallback
}

private func parseU32(_ s: String, _ fallback: UInt32) -> UInt32 {
    parseCount(input: s) ?? fallback
}

private func parseU64(_ s: String, _ fallback: UInt64) -> UInt64 {
    parseCountU64(input: s) ?? fallback
}

// MARK: - Spam / inbound perimeter (put_spam_policy)

private struct MailSpamPolicySection: View {
    let policy: SpamPolicyView
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (SpamPolicyView) async -> Void
    // Deployment baseline (admin opt-in aggregate; publish_spam_baseline).
    let baselineResult: BaselinePublishView?
    let onPublish: () async -> Void

    @State private var draft: SpamPolicyView
    // Integer / list fields edit as string buffers (parsed on Save).
    @State private var junk = ""
    @State private var reject = ""
    @State private var dnsbl = ""
    @State private var greylistDelay = ""
    @State private var maxConnPerMin = ""
    @State private var maxMessageBytes = ""
    // Per-user training (Tier-2 combined-score knobs; same put_spam_policy).
    @State private var bayesianWeight = ""
    @State private var bayesianMinSamples = ""
    @State private var bayesianFullConfidenceSamples = ""
    @State private var trainingHistoryRetention = ""
    // Deployment-wide catch-all recipient penalty (points; 0 = off). Applied at
    // the MTA per catch-all recipient, not in decide_spam_disposition
    // (mail-spam.md § Unlisted-recipient penalty).
    @State private var unlistedRecipientPenalty = ""

    init(policy: SpamPolicyView, reloadToken: Int, isBusy: Bool,
         baselineResult: BaselinePublishView?, onPublish: @escaping () async -> Void,
         onSave: @escaping (SpamPolicyView) async -> Void) {
        self.policy = policy
        self.reloadToken = reloadToken
        self.isBusy = isBusy
        self.baselineResult = baselineResult
        self.onPublish = onPublish
        self.onSave = onSave
        _draft = State(initialValue: policy)
    }

    /// `published` comes straight from the shared BaselinePublishView, so the
    /// client just picks the message + interpolates.
    private var baselineResultMessage: String {
        guard let r = baselineResult else { return "" }
        let base = r.published
            ? L.admin.mailPage.spamBaselinePublished(
                contributors: String(r.contributors), samples: String(r.sampleCount))
            : L.admin.mailPage.spamBaselineWithheld(contributors: String(r.contributors))
        // The holder-side erosion count (silent-erosion fix, mail-spam.md
        // § Encrypted-mode interaction) — surfaced beside the published/
        // withheld message whenever the last run skipped anyone.
        guard r.skippedContributors > 0 else { return base }
        return base + " " + L.admin.mailPage.spamBaselineSkippedContributors(
            count: String(r.skippedContributors))
    }

    var body: some View {
        policyGroup(title: L.admin.mailPage.spamGroupTitle, desc: L.admin.mailPage.spamGroupDesc,
                    saveLabel: L.admin.mailPage.spamSave, saveId: Ids.adminMailSpamSaveButton,
                    saveKind: "fauna.bridges.put_spam_policy",
                    isBusy: isBusy, onSave: { Task { await onSave(gather()) } }) {
            EditableFieldRow(L.admin.mailPage.thresholdJunkLabel, Ids.adminMailSpamThresholdJunk,
                        $junk, subtitle: L.admin.mailPage.thresholdJunkSubtitle)
            EditableFieldRow(L.admin.mailPage.thresholdRejectLabel, Ids.adminMailSpamThresholdReject,
                        $reject, subtitle: L.admin.mailPage.thresholdRejectSubtitle)
            policyMultiline(L.admin.mailPage.dnsblLabel, Ids.adminMailDnsblServers,
                            $dnsbl, subtitle: L.admin.mailPage.dnsblSubtitle)
            policyToggle(L.admin.mailPage.rejectNoRdnsLabel, Ids.adminMailRejectNoRdnsToggle,
                         $draft.rejectNoRdns)
            policyToggle(L.admin.mailPage.greylistEnabledLabel, Ids.adminMailGreylistEnabledToggle,
                         $draft.greylistEnabled)
            EditableFieldRow(L.admin.mailPage.greylistDelayLabel, Ids.adminMailGreylistDelayInput,
                        $greylistDelay)
            EditableFieldRow(L.admin.mailPage.maxConnPerMinLabel, Ids.adminMailMaxConnPerMinInput,
                        $maxConnPerMin)
            Picker(L.admin.mailPage.fcrdnsModeLabel, selection: $draft.fcrdnsMode) {
                // The value set + order come from the shared Rust catalog, not
                // hand-typed literals (mail-policy-config.md § An enumerated
                // knob's value set has ONE owner) — mirrors
                // `AdminUsersHubView.registrationModePicker`.
                ForEach(fcrdnsModeOptions(), id: \.value) { option in
                    Text(renderLocalizedText(option.label)).tag(option.value)
                }
            }
            .accessibilityIdentifier(Ids.adminMailFcrdnsModeSelect)
            // String-backed selection: read/write the SAME `$draft.fcrdnsMode`.
            .automationSelect(Ids.adminMailFcrdnsModeSelect,
                              value: { $draft.fcrdnsMode.wrappedValue },
                              options: { fcrdnsModeOptions().map { $0.value } }) { $draft.fcrdnsMode.wrappedValue = $0 }
            policyToggle(L.admin.mailPage.heloIdentityLabel, Ids.adminMailHeloIdentityRequiredToggle,
                         $draft.heloIdentityRequired)
            policyToggle(L.admin.mailPage.rejectFcrdnsFailLabel, Ids.adminMailRejectFcrdnsFailToggle,
                         $draft.rejectFcrdnsFail)
            EditableFieldRow(L.admin.mailPage.maxMessageBytesLabel, Ids.adminMailMaxMessageBytesInput,
                        $maxMessageBytes)
            EditableFieldRow(L.admin.mailPage.bayesianWeightLabel, Ids.adminMailSpamBayesianWeight,
                        $bayesianWeight, subtitle: L.admin.mailPage.bayesianWeightSubtitle)
            EditableFieldRow(L.admin.mailPage.bayesianMinSamplesLabel, Ids.adminMailSpamBayesianMinSamples,
                        $bayesianMinSamples, subtitle: L.admin.mailPage.bayesianMinSamplesSubtitle)
            EditableFieldRow(L.admin.mailPage.bayesianFullConfidenceSamplesLabel, Ids.adminMailSpamBayesianFullConfidenceSamples,
                        $bayesianFullConfidenceSamples, subtitle: L.admin.mailPage.bayesianFullConfidenceSamplesSubtitle)
            EditableFieldRow(L.admin.mailPage.trainingHistoryRetentionLabel, Ids.adminMailSpamTrainingHistoryRetention,
                        $trainingHistoryRetention, subtitle: L.admin.mailPage.trainingHistoryRetentionSubtitle)
            EditableFieldRow(L.admin.mailPage.unlistedRecipientPenaltyLabel, Ids.adminMailUnlistedRecipientPenalty,
                        $unlistedRecipientPenalty, subtitle: L.admin.mailPage.unlistedRecipientPenaltySubtitle)
            // ── Deployment baseline (admin opt-in aggregate; publish_spam_baseline) ──
            Divider()
            Button(L.admin.mailPage.publishSpamBaselineButton) { Task { await onPublish() } }
                .disabled(isBusy)
                .accessibilityIdentifier(Ids.adminMailPublishSpamBaselineButton)
                .automationActivate(Ids.adminMailPublishSpamBaselineButton, isEnabled: { !isBusy }) {
                    Task { await onPublish() }
                }
                .faunaGate("fauna.bridges.publish_spam_baseline")
            Text(L.admin.mailPage.publishSpamBaselineSubtitle).font(.caption).foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
            automationText(Ids.adminMailPublishSpamBaselineResult, baselineResultMessage)
                .font(.caption)
                .frame(maxWidth: .infinity, alignment: .leading)
                // `.id` keyed on the rendered message so a *post-onAppear* value change
                // (publish sets `baselineResult` nil→withheld/published via a no-refresh
                // snapshot update, no view re-identity, no `reloadToken` bump) forces a
                // fresh identity → the in-process `_AutomationRegister` re-fires `.onAppear`
                // and re-registers the CURRENT `text:` closure. Without this the registry
                // keeps the closure captured on the `nil`-era struct copy (reads `''`) —
                // the automation registration is `.onAppear`-once, unlike the knob fields'
                // live `.automationField` binding read. See apple-e2e-automation.md
                // § Stale-capture discipline (`.id`-on-content remedy).
                .id("admin-mail-publish-spam-baseline-result:\(baselineResultMessage)")
        }
        .task(id: reloadToken) { seed() }
    }

    private func seed() {
        draft = policy
        junk = String(policy.maxScoreBeforeSpamFolder)
        reject = String(policy.maxScoreBeforeReject)
        dnsbl = policy.dnsblServers.joined(separator: "\n")
        greylistDelay = String(policy.greylistDelaySecs)
        maxConnPerMin = String(policy.maxConnPerMin)
        maxMessageBytes = String(policy.maxMessageBytes)
        bayesianWeight = String(policy.bayesianWeightMilli)
        bayesianMinSamples = String(policy.bayesianMinSamples)
        bayesianFullConfidenceSamples = String(policy.bayesianFullConfidenceSamples)
        trainingHistoryRetention = String(policy.trainingHistoryRetentionDays)
        unlistedRecipientPenalty = String(policy.unlistedRecipientPenalty)
    }

    private func gather() -> SpamPolicyView {
        var p = draft
        p.maxScoreBeforeSpamFolder = parseU32(junk, policy.maxScoreBeforeSpamFolder)
        p.maxScoreBeforeReject = parseU32(reject, policy.maxScoreBeforeReject)
        p.dnsblServers = parseLines(dnsbl)
        p.greylistDelaySecs = parseU32(greylistDelay, policy.greylistDelaySecs)
        p.maxConnPerMin = parseU32(maxConnPerMin, policy.maxConnPerMin)
        p.maxMessageBytes = parseU32(maxMessageBytes, policy.maxMessageBytes)
        p.bayesianWeightMilli = parseU32(bayesianWeight, policy.bayesianWeightMilli)
        p.bayesianMinSamples = parseU32(bayesianMinSamples, policy.bayesianMinSamples)
        p.bayesianFullConfidenceSamples = parseU32(bayesianFullConfidenceSamples, policy.bayesianFullConfidenceSamples)
        p.trainingHistoryRetentionDays = parseU32(trainingHistoryRetention, policy.trainingHistoryRetentionDays)
        p.unlistedRecipientPenalty = parseU32(unlistedRecipientPenalty, policy.unlistedRecipientPenalty)
        return p
    }
}

// MARK: - Inbound authentication enforcement (put_auth_policy) — incl. conn-cap

private struct MailAuthPolicySection: View {
    let policy: AuthPolicyView
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (AuthPolicyView) async -> Void

    @State private var draft: AuthPolicyView
    @State private var maxFailures = ""
    @State private var maxConnPerIp = ""

    init(policy: AuthPolicyView, reloadToken: Int, isBusy: Bool,
         onSave: @escaping (AuthPolicyView) async -> Void) {
        self.policy = policy
        self.reloadToken = reloadToken
        self.isBusy = isBusy
        self.onSave = onSave
        _draft = State(initialValue: policy)
    }

    var body: some View {
        policyGroup(title: L.admin.mailPage.authGroupTitle, desc: L.admin.mailPage.authGroupDesc,
                    saveLabel: L.admin.mailPage.authSave, saveId: Ids.adminMailAuthSaveButton,
                    saveKind: "fauna.bridges.put_auth_policy",
                    isBusy: isBusy, onSave: { Task { await onSave(gather()) } }) {
            policyToggle(L.admin.mailPage.enforceDmarcLabel, Ids.adminMailAuthEnforceDmarcToggle,
                         $draft.enforceDmarc)
            policyToggle(L.admin.mailPage.enforceDmarcQuarantineLabel,
                         Ids.adminMailAuthEnforceDmarcQuarantineToggle, $draft.enforceDmarcQuarantine)
            policyToggle(L.admin.mailPage.enforceSpfHardfailLabel,
                         Ids.adminMailAuthEnforceSpfHardfailToggle, $draft.enforceSpfHardfail)
            policyToggle(L.admin.mailPage.enforceDkimLabel, Ids.adminMailAuthEnforceDkimToggle,
                         $draft.enforceDkim)
            policyToggle(L.admin.mailPage.logOnlyLabel, Ids.adminMailAuthLogOnlyToggle,
                         $draft.logOnly)
            EditableFieldRow(L.admin.mailPage.maxFailuresLabel, Ids.adminMailAuthMaxFailuresInput,
                        $maxFailures)
            // The per-IP concurrent-connection cap (the seventh AuthPolicy field). 0 = disabled. mail-policy-config.md
            // § Submission policy (per-IP concurrent-connection-cap row).
            EditableFieldRow(L.admin.mailPage.maxConnPerIpLabel, Ids.adminMailAuthMaxConnPerIpInput,
                        $maxConnPerIp)
        }
        .task(id: reloadToken) { seed() }
    }

    private func seed() {
        draft = policy
        maxFailures = String(policy.maxAuthFailuresPerMinute)
        maxConnPerIp = String(policy.maxConnPerIp)
    }

    private func gather() -> AuthPolicyView {
        var p = draft
        p.maxAuthFailuresPerMinute = parseU32(maxFailures, policy.maxAuthFailuresPerMinute)
        p.maxConnPerIp = parseU32(maxConnPerIp, policy.maxConnPerIp)
        return p
    }
}

// MARK: - Submission quotas (put_submission_policy)

private struct MailSubmissionPolicySection: View {
    let policy: SubmissionPolicyView
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (SubmissionPolicyView) async -> Void

    @State private var maxPerDay = ""
    @State private var maxRecipients = ""

    var body: some View {
        policyGroup(title: L.admin.mailPage.submissionGroupTitle,
                    desc: L.admin.mailPage.submissionGroupDesc,
                    saveLabel: L.admin.mailPage.submissionSave,
                    saveId: Ids.adminMailSubmissionSaveButton,
                    saveKind: "fauna.bridges.put_submission_policy",
                    isBusy: isBusy, onSave: { Task { await onSave(gather()) } }) {
            EditableFieldRow(L.admin.mailPage.submissionMaxPerDayLabel,
                        Ids.adminMailSubmissionMaxPerDayInput, $maxPerDay,
                        subtitle: L.admin.mailPage.submissionMaxPerDaySubtitle)
            EditableFieldRow(L.admin.mailPage.submissionMaxRecipientsLabel,
                        Ids.adminMailSubmissionMaxRecipientsInput, $maxRecipients,
                        subtitle: L.admin.mailPage.submissionMaxRecipientsSubtitle)
        }
        .task(id: reloadToken) { seed() }
    }

    private func seed() {
        maxPerDay = String(policy.maxPerDay)
        maxRecipients = String(policy.maxRecipientsPerMessage)
    }

    private func gather() -> SubmissionPolicyView {
        var p = policy
        p.maxPerDay = parseU32(maxPerDay, policy.maxPerDay)
        p.maxRecipientsPerMessage = parseU32(maxRecipients, policy.maxRecipientsPerMessage)
        return p
    }
}

// MARK: - IMAP server policy (put_imap_policy)

private struct MailImapPolicySection: View {
    let policy: ImapPolicyView
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (ImapPolicyView) async -> Void

    @State private var draft: ImapPolicyView
    @State private var idleTimeout = ""
    @State private var tombstoneRetention = ""
    @State private var bodystructureCache = ""
    @State private var storageBytes = ""
    @State private var messageCount = ""

    init(policy: ImapPolicyView, reloadToken: Int, isBusy: Bool,
         onSave: @escaping (ImapPolicyView) async -> Void) {
        self.policy = policy
        self.reloadToken = reloadToken
        self.isBusy = isBusy
        self.onSave = onSave
        _draft = State(initialValue: policy)
    }

    var body: some View {
        policyGroup(title: L.admin.mailPage.imapGroupTitle, desc: L.admin.mailPage.imapGroupDesc,
                    saveLabel: L.admin.mailPage.imapSave, saveId: Ids.adminMailImapSaveButton,
                    saveKind: "fauna.bridges.put_imap_policy",
                    isBusy: isBusy, onSave: { Task { await onSave(gather()) } }) {
            EditableFieldRow(L.admin.mailPage.imapIdleTimeoutLabel, Ids.adminMailImapIdleTimeoutInput,
                        $idleTimeout, subtitle: L.admin.mailPage.imapIdleTimeoutSubtitle)
            EditableFieldRow(L.admin.mailPage.imapTombstoneRetentionLabel,
                        Ids.adminMailImapTombstoneRetentionInput, $tombstoneRetention,
                        subtitle: L.admin.mailPage.imapTombstoneRetentionSubtitle)
            Picker(L.admin.mailPage.imapDeleteNonemptyLabel, selection: $draft.deleteNonempty) {
                // The value set + order come from the shared Rust catalog, not
                // hand-typed literals (mail-policy-config.md § An enumerated
                // knob's value set has ONE owner) — mirrors
                // `AdminUsersHubView.registrationModePicker`.
                ForEach(imapDeleteNonemptyOptions(), id: \.value) { option in
                    Text(renderLocalizedText(option.label)).tag(option.value)
                }
            }
            .accessibilityIdentifier(Ids.adminMailImapDeleteNonemptySelect)
            // String-backed selection: read/write the SAME `$draft.deleteNonempty`.
            .automationSelect(Ids.adminMailImapDeleteNonemptySelect,
                              value: { $draft.deleteNonempty.wrappedValue },
                              options: { imapDeleteNonemptyOptions().map { $0.value } }) { $draft.deleteNonempty.wrappedValue = $0 }
            EditableFieldRow(L.admin.mailPage.imapBodystructureCacheLabel,
                        Ids.adminMailImapBodystructureCacheInput, $bodystructureCache,
                        subtitle: L.admin.mailPage.imapBodystructureCacheSubtitle)
            EditableFieldRow(L.admin.mailPage.imapStorageBytesLabel, Ids.adminMailImapStorageBytesInput,
                        $storageBytes, subtitle: L.admin.mailPage.imapStorageBytesSubtitle)
            EditableFieldRow(L.admin.mailPage.imapMessageCountLabel, Ids.adminMailImapMessageCountInput,
                        $messageCount, subtitle: L.admin.mailPage.imapMessageCountSubtitle)
        }
        .task(id: reloadToken) { seed() }
    }

    private func seed() {
        draft = policy
        idleTimeout = String(policy.idleTimeoutSecs)
        tombstoneRetention = String(policy.tombstoneRetentionDays)
        bodystructureCache = String(policy.bodystructureCacheMax)
        storageBytes = String(policy.storageBytesDefault)
        messageCount = String(policy.messageCountDefault)
    }

    private func gather() -> ImapPolicyView {
        var p = draft
        p.idleTimeoutSecs = parseU32(idleTimeout, policy.idleTimeoutSecs)
        p.tombstoneRetentionDays = parseU32(tombstoneRetention, policy.tombstoneRetentionDays)
        p.bodystructureCacheMax = parseU32(bodystructureCache, policy.bodystructureCacheMax)
        p.storageBytesDefault = parseU64(storageBytes, policy.storageBytesDefault)
        p.messageCountDefault = parseU32(messageCount, policy.messageCountDefault)
        return p
    }
}

// MARK: - Outbound delivery (put_outbound_policy)

private struct MailOutboundPolicySection: View {
    let policy: OutboundPolicyView
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (OutboundPolicyView) async -> Void

    @State private var draft: OutboundPolicyView
    @State private var retrySchedule = ""
    @State private var permfailTimeout = ""
    @State private var delayWarning = ""
    @State private var ndrRateLimit = ""
    @State private var treat5xx = ""

    init(policy: OutboundPolicyView, reloadToken: Int, isBusy: Bool,
         onSave: @escaping (OutboundPolicyView) async -> Void) {
        self.policy = policy
        self.reloadToken = reloadToken
        self.isBusy = isBusy
        self.onSave = onSave
        _draft = State(initialValue: policy)
    }

    var body: some View {
        policyGroup(title: L.admin.mailPage.outboundGroupTitle, desc: L.admin.mailPage.outboundGroupDesc,
                    saveLabel: L.admin.mailPage.outboundSave, saveId: Ids.adminMailOutboundSaveButton,
                    saveKind: "fauna.bridges.put_outbound_policy",
                    isBusy: isBusy, onSave: { Task { await onSave(gather()) } }) {
            policyMultiline(L.admin.mailPage.outboundRetryScheduleLabel, Ids.adminMailOutboundRetrySchedule,
                            $retrySchedule, subtitle: L.admin.mailPage.outboundRetryScheduleSubtitle)
            EditableFieldRow(L.admin.mailPage.outboundPermfailTimeoutLabel,
                        Ids.adminMailOutboundPermfailTimeoutInput, $permfailTimeout,
                        subtitle: L.admin.mailPage.outboundPermfailTimeoutSubtitle)
            EditableFieldRow(L.admin.mailPage.outboundDelayWarningLabel,
                        Ids.adminMailOutboundDelayWarningInput, $delayWarning,
                        subtitle: L.admin.mailPage.outboundDelayWarningSubtitle)
            EditableFieldRow(L.admin.mailPage.outboundNdrRateLimitLabel,
                        Ids.adminMailOutboundNdrRateLimitInput, $ndrRateLimit,
                        subtitle: L.admin.mailPage.outboundNdrRateLimitSubtitle)
            policyToggle(L.admin.mailPage.outboundSuppressNdrSpfLabel,
                         Ids.adminMailOutboundSuppressNdrSpfToggle, $draft.suppressNdrSpfHardfail)
            policyToggle(L.admin.mailPage.outboundSuppressNdrDmarcLabel,
                         Ids.adminMailOutboundSuppressNdrDmarcToggle, $draft.suppressNdrDmarcReject)
            // postmaster-cc is read-only — project policy never CCs the postmaster
            // (mail-policy-config.md § Outbound delivery). Rendered insensitive; the
            // gathered value is the unchanged persisted one.
            VStack(alignment: .leading, spacing: 3) {
                policyToggle(L.admin.mailPage.outboundPostmasterCcLabel,
                             Ids.adminMailOutboundPostmasterCcToggle, $draft.postmasterCcBounces)
                    .disabled(true)
                Text(L.admin.mailPage.outboundPostmasterCcSubtitle)
                    .font(.caption).foregroundStyle(.secondary)
            }
            policyToggle(L.admin.mailPage.outboundTlsrptSendLabel,
                         Ids.adminMailOutboundTlsrptSendToggle, $draft.tlsrptSendReports)
            policyToggle(L.admin.mailPage.outboundIpv6Label, Ids.adminMailOutboundIpv6Toggle,
                         $draft.ipv6Enabled)
            policyMultiline(L.admin.mailPage.outboundTreat5xxLabel, Ids.adminMailOutboundTreat5xxTransient,
                            $treat5xx, subtitle: L.admin.mailPage.outboundTreat5xxSubtitle)
        }
        .task(id: reloadToken) { seed() }
    }

    private func seed() {
        draft = policy
        retrySchedule = policy.retryScheduleSeconds.map(String.init).joined(separator: "\n")
        permfailTimeout = String(policy.permanentFailureTimeoutHours)
        delayWarning = String(policy.delayWarningAtHours)
        ndrRateLimit = String(policy.ndrRateLimitDays)
        treat5xx = policy.treat5xxAsTransient.joined(separator: "\n")
    }

    private func gather() -> OutboundPolicyView {
        var p = draft
        p.retryScheduleSeconds = parseU64Lines(retrySchedule, fallback: policy.retryScheduleSeconds)
        p.permanentFailureTimeoutHours = parseU32(permfailTimeout, policy.permanentFailureTimeoutHours)
        p.delayWarningAtHours = parseU32(delayWarning, policy.delayWarningAtHours)
        p.ndrRateLimitDays = parseU32(ndrRateLimit, policy.ndrRateLimitDays)
        p.treat5xxAsTransient = parseLines(treat5xx)
        return p
    }
}

// MARK: - Aliases (put_alias_policy — nest-side, separate get_alias_policy twin)

private struct MailAliasPolicySection: View {
    let policy: AliasPolicyView
    let reloadToken: Int
    let isBusy: Bool
    let onSave: (AliasPolicyView) async -> Void

    @State private var draft: AliasPolicyView
    @State private var exactMax = ""
    @State private var reserved = ""

    init(policy: AliasPolicyView, reloadToken: Int, isBusy: Bool,
         onSave: @escaping (AliasPolicyView) async -> Void) {
        self.policy = policy
        self.reloadToken = reloadToken
        self.isBusy = isBusy
        self.onSave = onSave
        _draft = State(initialValue: policy)
    }

    var body: some View {
        policyGroup(title: L.admin.mailPage.aliasGroupTitle, desc: L.admin.mailPage.aliasGroupDesc,
                    saveLabel: L.admin.mailPage.aliasSave, saveId: Ids.adminMailAliasSaveButton,
                    saveKind: "fauna.bridges.put_alias_policy",
                    isBusy: isBusy, onSave: { Task { await onSave(gather()) } }) {
            EditableFieldRow(L.admin.mailPage.aliasExactMaxLabel, Ids.adminMailAliasExactMaxInput,
                        $exactMax, subtitle: L.admin.mailPage.aliasExactMaxSubtitle)
            policyMultiline(L.admin.mailPage.aliasReservedLabel, Ids.adminMailAliasReservedLocalParts,
                            $reserved, subtitle: L.admin.mailPage.aliasReservedSubtitle)
            policyToggle(L.admin.mailPage.aliasSubaddressingLabel, Ids.adminMailAliasSubaddressingToggle,
                         $draft.subaddressingEnabled)
            policyToggle(L.admin.mailPage.aliasWildcardPrefixLabel, Ids.adminMailAliasWildcardPrefixToggle,
                         $draft.wildcardPrefixEnabled)
        }
        .task(id: reloadToken) { seed() }
    }

    private func seed() {
        draft = policy
        exactMax = String(policy.exactAliasesMax)
        reserved = policy.reservedLocalParts.joined(separator: "\n")
    }

    private func gather() -> AliasPolicyView {
        var p = draft
        p.exactAliasesMax = parseU32(exactMax, policy.exactAliasesMax)
        // An empty box clears the reservation (full-PUT — distinct from "default").
        p.reservedLocalParts = parseLines(reserved)
        return p
    }
}
