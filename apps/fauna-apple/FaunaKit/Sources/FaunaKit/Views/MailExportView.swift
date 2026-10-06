import SwiftUI

/// The user-facing **mail-export** wizard page (`docs/goal/behavior/mail-export.md`),
/// shared by macOS + iOS (one FaunaKit view, thin per-target call sites).
/// A dumb renderer of `MailExportSnapshot` + dispatcher of
/// `MailExportAction` over the shared `MailExportMachine` (via `MailExportVM`); the
/// five-step FSM (Format → Scope → Confirm → Progress → Done) lives in the shared
/// machine, not here. Element IDs match `tests/e2e-unified/ui.yaml` `mail-export` /
/// `mail-export-mailbox-progress-list` exactly. Reference implementation: linux
/// `apps/fauna-linux/src/settings/mail_export.rs`.
///
/// **This page drives the export** (`MailExportVM`'s doc): Start spawns the
/// shared drive loop, Progress repaints on a tick, and Download writes the
/// recovered archive — the Done summary names where it went, and on iOS the
/// finished file is offered through the share sheet (an app-owned directory is
/// the save target there, `APIClient.mailExportSaveDir`). Only the active step's
/// section is shown, which is exactly what makes the SHARED `wizard-next-button` /
/// `wizard-back-button` unambiguous although each is painted on two steps
/// (Format+Scope carry a Next, Scope+Confirm a Back): the `switch step` below puts
/// exactly one of the two in the view tree at a time, the same answer tui, linux,
/// android and web give for these ids. The durable commit stays the separately
/// tagged `mail-export-start-button`.
public struct MailExportView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MailExportVM()

    /// Synchronous echo of a mailbox toggle, by mailbox name — read in preference
    /// to the snapshot so a driver's immediate `state` read-back is never stale
    /// (reading the last-rendered snapshot reports the PREVIOUS selection: early
    /// would be tolerable, wrong is not). Safe by construction: `ToggleMailbox` is
    /// a pure client action in the shared machine (`fauna-client-mail-settings`'s
    /// `apply_client_action`), so the echo can never disagree with the outcome.
    /// `MailImportView` keeps its own copy of this state (each `@State` is
    /// per-View), but the toggle/read logic itself is shared —
    /// `toggleMailboxPending`/`isMailboxSelected` (`MailboxToggleSelection.swift`).
    @State private var pendingSelection: [String: Bool] = [:]

    public init() {}

    private var step: ExportStep { vm.snapshot?.step ?? .format }

    public var body: some View {
        // Eager `ScrollView { VStack { GroupBox } }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules): the scope step's
        // mailbox-toggle list and the progress step's mailbox-progress rows can
        // both grow past one screenful, and an iOS `Form` never registers a row
        // below the fold. Mirrors the `GroupBox { VStack }` idiom every
        // `Admin*View`/`MailSettingsView` uses.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.mailExport.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                switch step {
                case .format: formatSection
                case .scope: scopeSection
                case .confirm: confirmSection
                case .progress: progressSection
                case .done: doneSection
                }
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.mailExport.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api) { client.sessionMaterial?.handle ?? "" }
        }
    }

    // MARK: - Step 1 — Format

    private var formatSection: some View {
        groupedSection(title: L.mailExport.formatTitle) {
            Picker(L.mailExport.formatTitle, selection: Binding(
                get: { vm.snapshot?.format ?? .mbox },
                set: { fmt in Task { await vm.dispatch(.selectFormat(format: fmt)) } }
            )) {
                Text(L.mailExport.formatMbox).tag(ExportFormat.mbox)
                Text(L.mailExport.formatMaildir).tag(ExportFormat.maildirPlus)
                Text(L.mailExport.formatEml).tag(ExportFormat.emlZip)
            }
            .accessibilityIdentifier(Ids.mailExportFormatPicker)
            .automationSelect(
                Ids.mailExportFormatPicker,
                value: { renderLocalizedText(exportFormatLabel(format: vm.snapshot?.format ?? .mbox)) },
                options: { Self.exportFormats.map { renderLocalizedText(exportFormatLabel(format: $0)) } },
                set: { label in
                    if let fmt = Self.exportFormats.first(where: {
                        renderLocalizedText(exportFormatLabel(format: $0)) == label
                    }) {
                        Task { await vm.dispatch(.selectFormat(format: fmt)) }
                    }
                })
            nextButton
        }
    }

    // MARK: - Step 2 — Scope

    private var scopeSection: some View {
        Group {
            groupedSection(title: L.mailExport.scopeMailboxesLabel) {
                let mailboxes = vm.snapshot?.mailboxes ?? []
                if mailboxes.isEmpty {
                    Text(L.mailExport.scopeMailboxesEmpty).foregroundStyle(.secondary)
                } else {
                    ForEach(Array(mailboxes.enumerated()), id: \.element.name) { _, mb in
                        // The row's own id is indexed (convention 1) and addressed by
                        // FLAT position: `actions/mail_export.py` finds a mailbox by
                        // reading `get_text(id, index=i)` and its selection by
                        // `get_attr(id, "state", index=…)`, with no `scope=`. So `text`
                        // must be the mailbox NAME — `/element/text` answers
                        // `text ?? value`, so a value-only entry would report "on"/"off"
                        // as the row's text and no name could ever match — and `value`
                        // carries the on/off contract. Both faces go on ONE entry: a
                        // second registration of the same id splits click and read
                        // across two index slots. No `.automationScope` — that is the
                        // container idiom (apple-e2e-automation.md rule 5) and this leaf
                        // has no child ids to scope.
                        Toggle(mb.name, isOn: Binding(
                            get: { isSelected(mb) },
                            set: { _ in toggleMailbox(mb.name) }
                        ))
                        .accessibilityIdentifier(Ids.mailExportScopeMailboxItem)
                        .automationActivate(
                            Ids.mailExportScopeMailboxItem,
                            text: { mb.name },
                            value: { isSelected(mb) ? "on" : "off" },
                            perform: { toggleMailbox(mb.name) })
                    }
                }
            }
            .accessibilityIdentifier(Ids.mailExportScopeMailboxes)
            // A container's `.accessibilityIdentifier` alone does not register it in
            // the in-process registry — `is_visible`/`wait_for` read the registry, not
            // the AX tree — and `.contain` keeps the child row ids queryable.
            .accessibilityElement(children: .contain)
            .automationValue(Ids.mailExportScopeMailboxes, text: {
                L.mailExport.scopeMailboxesLabel
            })

            groupedSection(title: L.mailExport.scopeTitle) {
                let dateFromBinding = Binding<String>(
                    get: { vm.snapshot?.dateFrom ?? "" },
                    set: { v in Task { await vm.dispatch(.setDateFrom(value: v)) } }
                )
                TextField(L.mailExport.scopeDateFromPlaceholder, text: dateFromBinding)
                    .accessibilityIdentifier(Ids.mailExportScopeDateFrom)
                    .automationField(Ids.mailExportScopeDateFrom, text: dateFromBinding)
                let dateToBinding = Binding<String>(
                    get: { vm.snapshot?.dateTo ?? "" },
                    set: { v in Task { await vm.dispatch(.setDateTo(value: v)) } }
                )
                TextField(L.mailExport.scopeDateToPlaceholder, text: dateToBinding)
                    .accessibilityIdentifier(Ids.mailExportScopeDateTo)
                    .automationField(Ids.mailExportScopeDateTo, text: dateToBinding)
                Toggle(L.mailExport.scopeStripHeadersLabel, isOn: Binding(
                    get: { vm.snapshot?.stripHeaders ?? false },
                    set: { on in Task { await vm.dispatch(.setStripHeaders(on: on)) } }
                ))
                .accessibilityIdentifier(Ids.mailExportScopeStripHeadersToggle)
                .automationActivate(
                    Ids.mailExportScopeStripHeadersToggle,
                    value: { (vm.snapshot?.stripHeaders ?? false) ? "on" : "off" },
                    perform: {
                        let now = vm.snapshot?.stripHeaders ?? false
                        Task { await vm.dispatch(.setStripHeaders(on: !now)) }
                    })
                HStack {
                    backButton
                    nextButton
                }
            }
        }
    }

    // MARK: - Step 3 — Confirm

    private var confirmSection: some View {
        groupedSection(title: L.mailExport.confirmTitle) {
            automationText(Ids.mailExportConfirmSummary, confirmSummary)
            HStack {
                backButton
                Button(L.mailExport.startButton) { Task { await vm.dispatch(.start) } }
                    .accessibilityIdentifier(Ids.mailExportStartButton)
                    .automationActivate(Ids.mailExportStartButton) {
                        Task { await vm.dispatch(.start) }
                    }
            }
        }
    }

    // MARK: - Shared wizard navigation

    /// The shared wizard Next, painted on Format (→ Scope) and Scope (→ Confirm).
    /// Only the active step's copy is ever in the view tree, so the two paint sites
    /// never put two `wizard-next-button` slots in the registry at once.
    private var nextButton: some View {
        Button(L.mailExport.next) { Task { await vm.dispatch(.next) } }
            .accessibilityIdentifier(Ids.wizardNextButton)
            // Same dispatch the Button action runs. Env-gated no-op in production.
            .automationActivate(Ids.wizardNextButton) { Task { await vm.dispatch(.next) } }
    }

    /// The shared wizard Back, painted on Scope (→ Format) and Confirm (→ Scope).
    private var backButton: some View {
        Button(L.mailExport.back) { Task { await vm.dispatch(.back) } }
            .accessibilityIdentifier(Ids.wizardBackButton)
            .automationActivate(Ids.wizardBackButton) { Task { await vm.dispatch(.back) } }
    }

    // MARK: - Step 4 — Progress

    private var progressSection: some View {
        groupedSection(title: L.mailExport.progressTitle) {
            automationText(Ids.mailExportProgressSummary, progressSummary)
            automationProgressBar(Ids.mailExportProgressBar, fraction: progressFraction)
            HStack {
                Button(L.mailExport.pauseButton) { Task { await vm.dispatch(.pause) } }
                    .accessibilityIdentifier(Ids.mailExportPauseButton)
                    .automationActivate(Ids.mailExportPauseButton) {
                        Task { await vm.dispatch(.pause) }
                    }
                Button(L.mailExport.resumeButton) { Task { await vm.dispatch(.resume) } }
                    .accessibilityIdentifier(Ids.mailExportResumeButton)
                    .automationActivate(Ids.mailExportResumeButton) {
                        Task { await vm.dispatch(.resume) }
                    }
                Button(L.mailExport.cancelButton, role: .destructive) { Task { await vm.dispatch(.cancel) } }
                    .accessibilityIdentifier(Ids.mailExportCancelButton)
                    .automationActivate(Ids.mailExportCancelButton) {
                        Task { await vm.dispatch(.cancel) }
                    }
            }
            automationText(Ids.mailExportErrorLog, (vm.snapshot?.errorLog ?? []).joined(separator: "\n"))
                .font(.caption.monospaced())
            mailboxProgressList
        }
    }

    private var mailboxProgressList: some View {
        let rows = vm.snapshot?.mailboxProgress ?? []
        // The container is a real `VStack`, not a bare modifier on the
        // `ForEach`: a modifier applied to a `ForEach` lands on each ROW, so
        // the list id would register once per row — mirrors
        // `MailImportView.mailboxProgressList`'s own `VStack` wrap. No
        // `.automationScope` on the rows: the import twin's row-level ids
        // (matching this file's) are read by FLAT count/index in the action
        // layer (`actions/mail_export.py::mailbox_progress_count`), not by
        // scoped containment.
        return VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(rows.enumerated()), id: \.element.name) { _, mp in
                HStack {
                    automationText(Ids.mailExportMailboxProgressListItemName, mp.name)
                    Spacer()
                    automationText(Ids.mailExportMailboxProgressListItemProgress, "\(mp.exported)/\(mp.total)")
                        .font(.caption.monospaced())
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.mailExportMailboxProgressListItem)
                .automationValue(Ids.mailExportMailboxProgressListItem, text: { mp.name })
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mailExportMailboxProgressList)
        .automationValue(Ids.mailExportMailboxProgressList, text: {
            String((vm.snapshot?.mailboxProgress ?? []).count)
        })
    }

    // MARK: - Step 5 — Done

    private var doneSection: some View {
        groupedSection(title: L.mailExport.doneTitle) {
            automationText(Ids.mailExportDoneSummary, doneSummary)
            Button(L.mailExport.downloadButton) { Task { await download() } }
                .accessibilityIdentifier(Ids.mailExportDownloadButton)
                .automationActivate(Ids.mailExportDownloadButton) {
                    Task { await download() }
                }
            automationText(Ids.mailExportDownloadUrl, vm.snapshot?.downloadUrl ?? "")
                .font(.caption.monospaced())
                .textSelection(.enabled)
            Button(L.mailExport.discardButton, role: .destructive) { Task { await vm.dispatch(.discard) } }
                .accessibilityIdentifier(Ids.mailExportDiscardButton)
                .automationActivate(Ids.mailExportDiscardButton) {
                    Task { await vm.dispatch(.discard) }
                }
        }
    }

    private static let exportFormats: [ExportFormat] = [.mbox, .maildirPlus, .emlZip]

    /// § Download flow: the shared machine GETs, opens and writes the archive
    /// (a refused one leaves no file). On iOS the save target is app-owned, so a
    /// completed download is handed to the share sheet — the user's own
    /// destination, as the account data export does (`AccountSettingsView`).
    /// Under e2e the file stays in `FAUNA_E2E_DOWNLOAD_DIR` with no sheet.
    private func download() async {
        await vm.dispatch(.download)
        #if os(iOS)
        guard SnapshotFileSaver.e2eDownloadDir == nil, vm.errorMessage == nil,
              let path = vm.snapshot?.savedArchivePath, !path.isEmpty else { return }
        ShareSheet.present(items: [URL(fileURLWithPath: path)])
        #endif
    }

    // MARK: - Selection

    /// Toggle one mailbox. The local echo is written FIRST and synchronously — the
    /// driver reads this row's `state` back with no wait in between.
    private func toggleMailbox(_ name: String) {
        toggleMailboxPending(name, in: vm.snapshot?.mailboxes, pendingSelection: &pendingSelection)
        Task { await vm.dispatch(.toggleMailbox(mailbox: name)) }
    }

    private func isSelected(_ mb: MailboxOption) -> Bool {
        isMailboxSelected(mb, pendingSelection: pendingSelection)
    }

    // MARK: - Derived text

    private var confirmSummary: String {
        guard let snap = vm.snapshot else { return L.mailExport.confirmPending }
        // Through the same echo the rows render from, so the Confirm step never
        // disagrees with the checkboxes the user just left behind.
        let selected = snap.mailboxes.filter { isSelected($0) }.count
        return L.mailExport.confirmSummaryFmt(
            format: renderLocalizedText(exportFormatLabel(format: snap.format)),
            mailboxes: String(selected))
    }

    private var progressSummary: String {
        guard let snap = vm.snapshot else { return "" }
        return L.mailExport.progressSummaryFmt(
            exported: String(snap.exportedCount), total: String(snap.totalCount),
            skipped: String(snap.skippedCount), errored: String(snap.erroredCount))
    }

    private var progressFraction: Double {
        guard let snap = vm.snapshot else { return 0 }
        return quotaFraction(usedBytes: Int64(snap.exportedCount), maxBytes: Int64(snap.totalCount))
    }

    private var doneSummary: String {
        guard let snap = vm.snapshot else { return "" }
        let format = renderLocalizedText(exportFormatLabel(format: snap.format))
        if let bytes = snap.blobBytes {
            // Once the archive is on disk the summary says WHERE — the visible
            // answer to the Download press (tui's and linux's shape).
            if !snap.savedArchivePath.isEmpty {
                return L.mailExport.savedSummaryFmt(
                    format: format, bytes: String(bytes), path: snap.savedArchivePath)
            }
            return L.mailExport.doneSummaryFmt(format: format, bytes: String(bytes))
        }
        return format
    }
}

// `groupedSection` (`GroupedSection.swift`) is the shared eager-container
// replacement for `Form`'s `Section` this file's wizard steps use.
