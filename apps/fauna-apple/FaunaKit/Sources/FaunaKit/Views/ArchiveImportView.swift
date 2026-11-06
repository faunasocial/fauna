// Excised from the Fauna Kids build — the kids FFI flavor carries no
// `archive-import` feature (family-safety.md § The account age band, the
// kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI
import UniformTypeIdentifiers

/// The user-facing **archive-import** wizard page — Settings → Import from
/// other services (`docs/goal/behavior/archive-import.md` § The wizard and its
/// machine), shared by macOS + iOS. A dumb renderer of `ArchiveImportSnapshot`
/// and a dispatcher of `ArchiveImportAction` over the shared
/// `ArchiveImportMachine` (via `ArchiveImportVM`); the six-screen FSM, the
/// parser, the folder writes and the authoring live in the shared machine, and
/// every word this page shows for a token — a category, a run state, an
/// audience mode, a platform's help — comes from the machine crate's shared
/// vocabulary fns, never a Swift re-spelling. Element IDs match
/// `tests/e2e-unified/ui.yaml` `archive-import` exactly. Lifts the tui lead
/// page (`apps/fauna-tui/src/settings/archive_import.rs`); the `MailImportView`
/// shape.
///
/// - **Exactly one step is in the view tree** (the body `switch`es on `step`),
///   which is what keeps the reused `wizard-back-button` / `wizard-next-button`
///   and the two `archive-import-error-log` copies unique at any moment.
/// - **The path and date fields are page-local drafts**, committed as one
///   ordered dispatch at Open archive / Next (the tui `open_actions` /
///   `scope_next_actions`), never a `Set*` per keystroke.
/// - **The archive is a path field on both macOS and iOS** (ui.yaml: "path entry
///   (tui/desktop)"); Open archive with an empty field launches the system file
///   picker and fills the field from the pick.
/// - **A category toggle echoes synchronously** (``pendingSelection``), so the
///   walk's `state` read right after the click describes that click — the
///   mail-import mailbox-row reasoning; the machine's `ToggleCategory` carries
///   the wanted value, so the echo can never disagree with the outcome.
/// - **The profile prefill is offered, never applied silently**: enabled while
///   the snapshot carries the archive's profile and the tap has not applied it
///   yet; the tap dispatches `ApplyArchiveProfile` (§ What each category
///   becomes, the Profile row).
public struct ArchiveImportView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @Environment(\.openURL) private var openURL
    @Environment(FeedVM.self) private var feedVM: FeedVM?
    @State private var vm = ArchiveImportVM()

    /// View imported → the feed on the imported-posts selection for the
    /// archive's platform (the shared `FeedManager::select_imported`, through
    /// the app-root `FeedVM`); the folder link → Settings → Folders. Wired by
    /// each target's settings shell.
    private let onNavigateToFeed: () -> Void
    private let onNavigateToFolders: () -> Void

    @State private var pendingKind: ArchiveSourceKind?
    @State private var draftPath = ""
    @State private var draftDateFrom = ""
    @State private var draftDateTo = ""
    @State private var pendingSelection: [String: Bool] = [:]
    @State private var pickingArchive = false
    /// Two-click inline confirm for Cancel (what landed is kept).
    @State private var cancelArmed = false
    /// Review skipped has revealed the skip log on the Done screen.
    @State private var showSkipped = false

    /// The session whose secret configures the timeline manager when View
    /// imported fires before the Feed page was ever opened.
    private let session: SessionState?

    public init(session: SessionState? = nil,
                onNavigateToFeed: @escaping () -> Void = {},
                onNavigateToFolders: @escaping () -> Void = {}) {
        self.session = session
        self.onNavigateToFeed = onNavigateToFeed
        self.onNavigateToFolders = onNavigateToFolders
    }

    private var snap: ArchiveImportSnapshot? { vm.snapshot }
    private var step: ArchiveImportStep { snap?.step ?? .source }

    /// Every composed line of the page for the current snapshot, from the
    /// shared composers (`libs/fauna-archive-import-machine/src/text.rs`) —
    /// the wording, the size/duration/date rendering and the joining
    /// punctuation live there once; this page only resolves and paints them.
    private var pageText: ArchiveImportPageText? {
        snap.map { archiveImportPageText(snapshot: $0) }
    }

    public var body: some View {
        // Eager `ScrollView { VStack }`, not a lazy `Form`: the Scope category
        // list and the Progress rows can both grow past one screenful
        // (`MailImportView`'s rule 6 reasoning).
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.archiveImport.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if vm.unavailable {
                    Text(L.archiveImport.unavailable)
                } else {
                    switch step {
                    case .source: sourceSection
                    case .archive: archiveSection
                    case .scope: scopeSection
                    case .confirm: confirmSection
                    case .progress: progressSection
                    case .done: doneSection
                    }
                }
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.archiveImport.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api, deviceId: client.deviceId)
            // A hydrate that found an unfinished import names its archive
            // again: seed the path draft once, never over what was typed.
            if draftPath.isEmpty { draftPath = vm.snapshot?.archivePath ?? "" }
        }
        .onChange(of: step) { _, newStep in
            if newStep != .done { showSkipped = false }
            if newStep != .progress { cancelArmed = false }
        }
        .fileImporter(isPresented: $pickingArchive, allowedContentTypes: [.zip]) { result in
            guard case .success(let url) = result else { return }
            // A picked file outside the sandbox is readable only inside its
            // security scope; the run reads the archive again while it imports,
            // so the scope is held for the life of the process.
            _ = url.startAccessingSecurityScopedResource()
            draftPath = url.path
            openArchive()
        }
        .onDisappear { vm.reset() }
    }

    // MARK: - Step 1 — Source

    private static let sourceKinds: [ArchiveSourceKind] = [.facebook, .instagram]

    private var sourceKind: ArchiveSourceKind { pendingKind ?? snap?.sourceKind ?? .facebook }

    /// The platform's name — the shared label, a brand never translated, which
    /// round-trips as the picker's option text.
    private func kindLabel(_ kind: ArchiveSourceKind) -> String {
        archiveImportSourceKindLabel(kind: kind)
    }

    private var sourceSection: some View {
        groupedSection(title: L.archiveImport.sourceTitle) {
            Picker(L.archiveImport.sourceTitle, selection: Binding(
                get: { sourceKind },
                set: { selectKind($0) }
            )) {
                ForEach(Self.sourceKinds, id: \.self) { kind in
                    Text(kindLabel(kind)).tag(kind)
                }
            }
            .accessibilityIdentifier(Ids.archiveImportSourcePicker)
            .automationSelect(
                Ids.archiveImportSourcePicker,
                value: { kindLabel(sourceKind) },
                options: { Self.sourceKinds.map(kindLabel) },
                set: { label in
                    if let k = Self.sourceKinds.first(where: { kindLabel($0) == label }) {
                        selectKind(k)
                    }
                })
            automationText(Ids.archiveImportSourceHelp,
                           renderLocalizedText(archiveImportSourceHelp(kind: sourceKind)))
            Button(exportLinkText) { openExportPage() }
                .accessibilityIdentifier(Ids.archiveImportSourceExportLink)
                .automationActivate(Ids.archiveImportSourceExportLink,
                                    text: { exportLinkText }) { openExportPage() }
            nextButton { Task { await vm.dispatch(.next) } }
        }
    }

    /// The chosen platform's export page — the snapshot's derived address, a
    /// shared constant never typed by anyone.
    private var exportUrl: String { snap?.sourceExportUrl ?? "" }

    private var exportLinkText: String {
        resolveComposedText(archiveImportSourceLink(url: exportUrl))
    }

    private func openExportPage() {
        guard let url = URL(string: exportUrl), !exportUrl.isEmpty else { return }
        ExternalURLOpener.open(url, with: openURL)
    }

    private func selectKind(_ kind: ArchiveSourceKind) {
        pendingKind = kind
        Task {
            await vm.dispatch(.selectSource(kind: kind))
            pendingKind = nil
        }
    }

    // MARK: - Step 2 — Archive

    private var archiveSection: some View {
        groupedSection(title: L.archiveImport.archiveTitle) {
            TextField(L.archiveImport.archivePathPlaceholder, text: $draftPath)
                .accessibilityIdentifier(Ids.archiveImportArchivePath)
                .automationField(Ids.archiveImportArchivePath, text: $draftPath)
            Button(L.archiveImport.archiveOpenButton) { openOrPick() }
                .accessibilityIdentifier(Ids.archiveImportArchiveOpenButton)
                .automationActivate(Ids.archiveImportArchiveOpenButton) { openOrPick() }
            if let summary = pageText?.archiveSummary {
                automationText(Ids.archiveImportArchiveSummary, resolveComposedText(summary))
            }
            HStack {
                backButton
                nextButton { Task { await vm.dispatch(.next) } }
            }
        }
    }

    /// Open what the field holds; with an empty field, launch the picker.
    private func openOrPick() {
        if draftPath.trimmingCharacters(in: .whitespaces).isEmpty {
            pickingArchive = true
        } else {
            openArchive()
        }
    }

    /// Commit the path draft, then open it — one ordered dispatch.
    private func openArchive() {
        let path = draftPath.trimmingCharacters(in: .whitespaces)
        Task { await vm.dispatchSequence([.setArchivePath(value: path), .openArchive]) }
    }

    // MARK: - Step 3 — Scope

    private static let audienceModes: [AudienceMode] = [.original, .onlyMe]

    private func modeLabel(_ mode: AudienceMode) -> String {
        renderLocalizedText(archiveImportAudienceModeLabel(mode: mode))
    }

    private func isSelected(_ row: CategoryRow) -> Bool {
        pendingSelection[row.token] ?? row.selected
    }

    private var scopeSection: some View {
        groupedSection(title: L.archiveImport.scopeTitle) {
            let rows = snap?.categories ?? []
            let rowTexts = pageText?.scopeRows ?? []
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(rows.enumerated()), id: \.element.token) { index, row in
                    categoryRow(row, text: index < rowTexts.count ? resolveComposedText(rowTexts[index]) : "")
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.archiveImportScopeCategories)
            // A container's identifier alone does not register it in the
            // in-process registry (`MailImportView`'s mailbox list).
            .automationValue(Ids.archiveImportScopeCategories, text: {
                L.archiveImport.scopeCategoriesLabel
            })

            let mode = snap?.audienceMode ?? .original
            Picker(L.archiveImport.scopeAudienceModeLabel, selection: Binding(
                get: { mode },
                set: { m in Task { await vm.dispatch(.setAudienceMode(mode: m)) } }
            )) {
                ForEach(Self.audienceModes, id: \.self) { m in
                    Text(modeLabel(m)).tag(m)
                }
            }
            .accessibilityIdentifier(Ids.archiveImportScopeAudienceMode)
            .automationSelect(
                Ids.archiveImportScopeAudienceMode,
                value: { modeLabel(snap?.audienceMode ?? .original) },
                options: { Self.audienceModes.map(modeLabel) },
                set: { label in
                    if let m = Self.audienceModes.first(where: { modeLabel($0) == label }) {
                        Task { await vm.dispatch(.setAudienceMode(mode: m)) }
                    }
                })
            automationText(Ids.archiveImportScopeAudienceSummary, audienceSummary)
            if snap?.nestSupportsHiddenTiers == false {
                Text(L.archiveImport.scopeHiddenTiersUnavailable)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            TextField(L.archiveImport.scopeDateFromPlaceholder, text: $draftDateFrom)
                .accessibilityIdentifier(Ids.archiveImportScopeDateFrom)
                .automationField(Ids.archiveImportScopeDateFrom, text: $draftDateFrom)
            TextField(L.archiveImport.scopeDateToPlaceholder, text: $draftDateTo)
                .accessibilityIdentifier(Ids.archiveImportScopeDateTo)
                .automationField(Ids.archiveImportScopeDateTo, text: $draftDateTo)
            HStack {
                backButton
                nextButton { scopeNext() }
            }
        }
    }

    /// One indexed `archive-import-scope-category-item`: a toggle for an
    /// importable category, text for a model-only one (kept in the archive
    /// folder, never selectable). Its `state` is `on`/`off`/`kept`.
    @ViewBuilder
    private func categoryRow(_ row: CategoryRow, text: String) -> some View {
        if row.importable {
            Toggle(text, isOn: Binding(
                get: { isSelected(row) },
                set: { _ in toggleCategory(row) }
            ))
            .accessibilityIdentifier(Ids.archiveImportScopeCategoryItem)
            .automationActivate(
                Ids.archiveImportScopeCategoryItem,
                text: { text },
                value: { isSelected(row) ? "on" : "off" },
                attributes: { ["state": isSelected(row) ? "on" : "off"] },
                perform: { toggleCategory(row) })
        } else {
            Text(text)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.archiveImportScopeCategoryItem)
                .automationValue(
                    Ids.archiveImportScopeCategoryItem,
                    text: { text },
                    value: { "kept" },
                    attributes: { ["state": "kept"] })
        }
    }

    /// The local echo is written FIRST and synchronously, then dropped once
    /// the dispatch's own re-read carries the machine's answer.
    private func toggleCategory(_ row: CategoryRow) {
        let next = !isSelected(row)
        pendingSelection[row.token] = next
        Task {
            await vm.dispatch(.toggleCategory(token: row.token, selected: next))
            pendingSelection[row.token] = nil
        }
    }

    private var audienceSummary: String {
        resolveComposedText(archiveImportAudienceSummary(
            summary: snap?.summary, mode: snap?.audienceMode ?? .original))
    }

    /// Scope → Confirm: both date drafts committed, then the advance — the
    /// machine refuses a malformed date or a reversed range here.
    private func scopeNext() {
        let from = draftDateFrom.trimmingCharacters(in: .whitespaces)
        let to = draftDateTo.trimmingCharacters(in: .whitespaces)
        Task { await vm.dispatchSequence([.setDateFrom(value: from), .setDateTo(value: to), .next]) }
    }

    // MARK: - Step 4 — Confirm

    private var confirmSection: some View {
        groupedSection(title: L.archiveImport.confirmTitle) {
            automationText(Ids.archiveImportConfirmSummary,
                           pageText.map { resolveComposedText($0.confirmSummary) } ?? "")
            HStack {
                backButton
                Button(L.archiveImport.startButton) { start() }
                    .accessibilityIdentifier(Ids.archiveImportStartButton)
                    .automationActivate(Ids.archiveImportStartButton) { start() }
            }
        }
    }

    private func start() { Task { await vm.dispatch(.start) } }

    // MARK: - Step 5 — Progress

    private var progressSection: some View {
        groupedSection(title: L.archiveImport.progressTitle) {
            automationText(Ids.archiveImportProgressSummary, progressSummary)
            ProgressView(value: progressFraction)
                .accessibilityIdentifier(Ids.archiveImportProgressBar)
                .automationValue(
                    Ids.archiveImportProgressBar,
                    text: { String(format: "%.0f%%", progressFraction * 100) },
                    attributes: { ["fraction": String(format: "%.4f", progressFraction)] })
            HStack {
                Button(L.archiveImport.pauseButton) { Task { await vm.dispatch(.pause) } }
                    .accessibilityIdentifier(Ids.archiveImportPauseButton)
                    .automationActivate(Ids.archiveImportPauseButton) {
                        Task { await vm.dispatch(.pause) }
                    }
                Button(L.archiveImport.resumeButton) { Task { await vm.dispatch(.resume) } }
                    .accessibilityIdentifier(Ids.archiveImportResumeButton)
                    .automationActivate(Ids.archiveImportResumeButton) {
                        Task { await vm.dispatch(.resume) }
                    }
                Button(cancelLabel, role: .destructive) { tapCancel() }
                    .accessibilityIdentifier(Ids.archiveImportCancelButton)
                    .automationActivate(Ids.archiveImportCancelButton, text: { cancelLabel }) { tapCancel() }
            }
            Text(L.archiveImport.errorLogTitle)
                .font(.headline)
            automationText(Ids.archiveImportErrorLog, skipLog)
                .font(.caption.monospaced())
            categoryProgressList
            // A concluded run may be left (Back starts a fresh wizard); a
            // running, paused or errored one is resumable and has no Back.
            if snap?.runState == .cancelled || snap?.runState == .completed {
                backButton
            }
        }
    }

    private var skipLog: String { (snap?.skipLog ?? []).joined(separator: "\n") }

    private var progressFraction: Double { pageText?.progressFraction ?? 0 }

    /// The run state, the counts, then the elapsed time and the estimate left.
    private var progressSummary: String {
        pageText.map { resolveComposedText($0.progressSummary) } ?? ""
    }

    /// One row per selected importable category with its settled counts.
    private var categoryProgressList: some View {
        let rows = pageText?.progressRows ?? []
        return VStack(alignment: .leading, spacing: 4) {
            ForEach(rows, id: \.token) { row in
                let name = renderLocalizedText(row.label)
                HStack {
                    automationText(Ids.archiveImportCategoryProgressListItemName, name)
                    Spacer()
                    automationText(Ids.archiveImportCategoryProgressListItemProgress,
                                   resolveComposedText(row.progress))
                        .font(.caption.monospaced())
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.archiveImportCategoryProgressListItem)
                .automationValue(Ids.archiveImportCategoryProgressListItem, text: { name })
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.archiveImportCategoryProgressList)
        .automationValue(Ids.archiveImportCategoryProgressList, text: { String(rows.count) })
    }

    /// `Cancel`, relabelled `Confirm?` once armed — shared by the button title
    /// and its automation `text:`.
    private var cancelLabel: String {
        cancelArmed ? L.common.confirmQ : L.archiveImport.cancelButton
    }

    /// Two-click inline arm-then-confirm (no modal): the e2e's two taps on the
    /// same id arm, then dispatch Cancel.
    private func tapCancel() {
        if cancelArmed {
            cancelArmed = false
            Task { await vm.dispatch(.cancel) }
        } else {
            cancelArmed = true
        }
    }

    // MARK: - Step 6 — Done

    /// View imported: the shared `select_imported` on the archive platform's
    /// lowercase token, then the Feed page (archive-import.md step 6).
    private func viewImported() {
        let token: String
        switch snap?.sourceKind ?? sourceKind {
        case .facebook: token = "facebook"
        case .instagram: token = "instagram"
        }
        Task {
            if let client, let feedVM, let secret = session?.secretHex {
                await feedVM.configure(api: client.api, secretHex: secret,
                                       deviceIdHex: client.deviceId)
                await feedVM.selectImported(token: token)
            }
            onNavigateToFeed()
        }
    }

    private var doneSection: some View {
        groupedSection(title: L.archiveImport.doneTitle) {
            automationText(Ids.archiveImportDoneSummary, doneSummary)
            HStack {
                Button(L.archiveImport.viewImportedButton) { viewImported() }
                    .accessibilityIdentifier(Ids.archiveImportViewImportedButton)
                    .automationActivate(Ids.archiveImportViewImportedButton) { viewImported() }
                Button(L.archiveImport.reviewSkippedButton) { showSkipped.toggle() }
                    .accessibilityIdentifier(Ids.archiveImportReviewSkippedButton)
                    .automationActivate(Ids.archiveImportReviewSkippedButton) { showSkipped.toggle() }
            }
            if showSkipped {
                automationText(Ids.archiveImportErrorLog, skipLog)
                    .font(.caption.monospaced())
            }
            Button(L.archiveImport.profilePrefillButton) { applyProfile() }
                .disabled(!profileOffered)
                .accessibilityIdentifier(Ids.archiveImportProfilePrefillButton)
                .automationActivate(Ids.archiveImportProfilePrefillButton,
                                    isEnabled: { profileOffered }) { applyProfile() }
            Button(folderLinkText) { onNavigateToFolders() }
                .accessibilityIdentifier(Ids.archiveImportFolderLink)
                .automationActivate(Ids.archiveImportFolderLink,
                                    text: { folderLinkText }) {
                    onNavigateToFolders()
                }
            backButton
        }
    }

    /// Enabled while the run's end found a profile in the archive and the tap
    /// has not applied it yet — a second tap never re-publishes it.
    private var profileOffered: Bool {
        snap?.archiveProfile != nil && !(snap?.profileApplied ?? false)
    }

    private func applyProfile() { Task { await vm.dispatch(.applyArchiveProfile) } }

    private var doneSummary: String {
        pageText.map { resolveComposedText($0.doneSummary) } ?? ""
    }

    private var folderLinkText: String {
        pageText.map { resolveComposedText($0.folderLink) } ?? ""
    }

    // MARK: - Shared buttons

    /// The reused wizard Back — only the active step's copy is in the tree.
    private var backButton: some View {
        Button(L.archiveImport.back) { Task { await vm.dispatch(.back) } }
            .accessibilityIdentifier(Ids.wizardBackButton)
            .automationActivate(Ids.wizardBackButton) { Task { await vm.dispatch(.back) } }
    }

    private func nextButton(_ action: @escaping () -> Void) -> some View {
        Button(L.archiveImport.next, action: action)
            .accessibilityIdentifier(Ids.wizardNextButton)
            .automationActivate(Ids.wizardNextButton, perform: action)
    }
}
#endif // !FAUNA_KIDS
