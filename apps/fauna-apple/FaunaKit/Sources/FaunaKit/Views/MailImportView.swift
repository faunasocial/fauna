// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// The user-facing **mail-import** wizard page (`docs/goal/behavior/
/// mailbox-migration.md`), shared by macOS + iOS (one FaunaKit view, thin
/// per-target call sites). A dumb renderer of `MailImportSnapshot` + dispatcher
/// of `MailImportAction` over the shared `MailImportMachine` (via
/// `MailImportVM`); the five-screen FSM (Source → Scope → Confirm → Progress →
/// Done) lives in the shared machine, not here. Element IDs match
/// `tests/e2e-unified/ui.yaml` `mail-import` / `mail-import-mailbox-progress-list`
/// exactly. Reference implementations, in the order the trickle-down landed
/// them: tui (`apps/fauna-tui/src/settings/mail_import.rs`, the lead),
/// linux (`apps/fauna-linux/src/settings/mail_import.rs`), web
/// (`MailImportSection.svelte`), android, windows.
///
/// **Unlike the export twin, the backend is REAL end to end** — nest RPC
/// surface plus the shared-Rust foreign-IMAP client both ship — so every
/// `error-message` here is a genuine source/nest rejection.
///
/// # Three things that are NOT the export twin's shape
///
/// 1. **The page spawns `run_import` itself** after a `Start`/`Resume` whose
///    post-dispatch snapshot really reports `.running` — see `MailImportVM`.
/// 2. **`wizard-back-button` / `wizard-next-button` are shared ids painted on
///    more than one screen** (Scope carries Back + Next, Confirm carries Back).
///    Exactly one screen's copy is ever in the view tree, because the body
///    `switch`es on `step` — so only the active screen's registers, from first
///    render, with no state update needed to make it so.
/// 3. **The Source/Scope text fields are page-local drafts** committed as ONE
///    ordered multi-action dispatch at Connect/Next, never a `Set*` per
///    keystroke. The action lists come from the shared `connectActions` /
///    `scopeNextActions` (`libs/fauna-client-mail-settings/src/import.rs`) —
///    this side's only job is locating its own field values. The ordering is a
///    correctness contract: a per-keystroke dispatch races the Connect and can
///    log in to the source with a truncated password. It also means `render`
///    never writes back into a field, so a failed-connect retry keeps the
///    connection details on screen (§ Wizard steps step 2).
///
/// # Two reads the e2e walk makes with NO wait, and what they cost
///
/// A dispatch is `async`; the in-process automation server acks a command as
/// soon as the handler returns. Everywhere `test_mail_import.py` reads
/// immediately after acting, this page must answer from local, synchronous
/// truth — the dispatch's own re-read then re-applies the same thing:
///
/// - **Pick a provider, then type into the field it reveals.**
///   `_fill_generic_source` selects Generic IMAP and types into
///   `mail-import-source-host` with nothing in between; a field not in the view
///   tree is not registered, so the type would 404. ``draftKind`` is written
///   synchronously in the picker's own `set` closure and is what field
///   visibility reads.
/// - **Toggle a mailbox, then read *that row's* `state`.** ``pendingSelection``
///   is written synchronously in the row's activate closure and wins over the
///   snapshot until the dispatch catches up. Leaving it to the async re-read
///   reports the PREVIOUS selection — stale, not merely early. (`ToggleMailbox`
///   is a pure client action in the shared machine — `apply_client_action` —
///   so a synchronous local echo can never disagree with the outcome.)
///
/// A third read needs nothing: because the text fields are drafts committed at
/// the transition, typing then clicking Connect crosses no async boundary.
///
/// # Per-provider Source-screen field visibility
///
/// The table is `mailbox-migration.md` § Wizard steps step 1's "Required
/// fields", as the tui lead app transcribed it:
///
/// - **Gmail / iCloud** show `source-username` + `source-app-password` (plus
///   the provider's app-password help line); host/port/tls-mode stay on the
///   preset the machine's `SelectSourceKind` arm applies.
/// - **Outlook** shows `source-username` (the Microsoft account's address, the
///   XOAUTH2 user) and **Sign in with Microsoft** — no password field, no host
///   field, no Connect button: Microsoft takes no password from a mail app, so
///   the IMAP fallback is withdrawn (`mailbox-migration.md` § Microsoft
///   sign-in — the device-code grant).
/// - **Generic** shows the IMAP fields alone; no app-password, no OAuth.
///
/// # The Outlook source signs in with Microsoft
///
/// `mail-import-source-oauth-button` dispatches the shared
/// `microsoftSignInActions` (commit the address, `BeginMicrosoftSignIn`); once
/// Microsoft has answered with a code ``MailImportVM`` spawns
/// `runMicrosoftSignIn`, the polling half, exactly as it spawns `runImport`.
/// While the code is out `mail-import-source-oauth-code` paints the code and
/// Microsoft's address (its `code` and `url` attributes carry each alone) beside
/// an id-less **Open in browser**; the grant connects by itself and the step
/// moves on to Scope with nothing more pressed. A deployment without a
/// registration paints the unavailable sentence and keeps the button inert.
///
/// # Two controls with no machine action
///
/// `mail-import-scope-mailbox-mapping` is informational: no `MailImportAction`
/// changes the destination mapping and the doc names no control for it
/// (§ Wizard steps step 3 — "1:1 by default"). And the two Done buttons are
/// backed by no action either: `view-imported` navigates to Conversations,
/// where mail lives, and `review-skipped` is a page-local toggle
/// (`showSkipped`, the tui `show_skipped` shape) that reveals the snapshot's
/// `error_log` below the Done card (§ Wizard steps step 6).
public struct MailImportView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MailImportVM()
    @Environment(\.openURL) private var openURL

    /// Both Done deep-links go here — Conversations, where mail lives. Wired by
    /// each target's settings shell (the `MailListsView.onNavigateToMembers`
    /// convention).
    private let onNavigateToConversations: () -> Void

    // ── Source-screen drafts (committed at Connect, never per keystroke) ──
    /// The picked provider, held locally so field visibility is a pure function
    /// of the pick and needs no round trip (see this type's docs).
    @State private var draftKind: ImportSourceKind = .gmail
    @State private var draftHost = ""
    @State private var draftPort = ""
    @State private var draftUsername = ""
    @State private var draftPassword = ""
    @State private var draftAppPassword = ""
    /// Whether the remembered source (`MailImportSnapshot.sourceRemembered`) has
    /// filled the Source drafts on this visit — once, so a re-read never writes
    /// over what the user typed (the tui `apply_mail_import_snapshot` shape).
    @State private var sourcePrefilled = false

    // ── Scope-screen drafts ──
    @State private var draftDateFrom = ""
    @State private var draftMaxSizeMb = ""

    /// Synchronous echo of a mailbox toggle, by mailbox name — read in
    /// preference to the snapshot so the walk's immediate `state` read is never
    /// stale (see this type's docs). Cleared whenever a fresh mailbox list
    /// arrives.
    @State private var pendingSelection: [String: Bool] = [:]

    /// Two-click inline confirm for the destructive Cancel (already-imported
    /// messages are kept, § UX shape step 5). Mirrors `MailListsView.tapDelete`
    /// / linux `wire_two_click`.
    @State private var cancelArmed = false
    /// Whether Review skipped has revealed the skip log below the Done card —
    /// page-local, the tui `show_skipped` shape; cleared when the wizard
    /// leaves Done.
    @State private var showSkipped = false

    public init(onNavigateToConversations: @escaping () -> Void = {}) {
        self.onNavigateToConversations = onNavigateToConversations
    }

    private var step: ImportStep { vm.snapshot?.step ?? .source }

    public var body: some View {
        // Eager `ScrollView { VStack { GroupBox } }`, NOT a lazy `Form` (rule 6
        // — apple-e2e-automation.md § Registration rules): the Scope screen's
        // mailbox-toggle list and the Progress screen's mailbox-progress rows
        // can both grow past one screenful, and an iOS `Form` never registers a
        // row below the fold. Mirrors `MailExportView` and the `GroupBox { VStack }`
        // idiom every `Admin*View` / `MailSettingsView` uses.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.mailImport.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // Exactly one screen is in the view tree at a time — which is
                // what disambiguates the shared `wizard-*-button` ids, from
                // first render (see this type's docs).
                switch step {
                case .source: sourceSection
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
        .pageTitle(L.mailImport.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api)
            syncDraftsFromSnapshot()
        }
        .onChange(of: vm.snapshot?.resumeNeedsCredentials ?? false) { _, needs in
            resumePromptChanged(to: needs)
        }
        // Another device moved this account's import: re-read the session
        // through the machine. Cancel's two-click arm is view state, so the
        // re-read cannot disarm it (`test_cancel_asks_first_and_keeps_what_already_arrived`).
        .onMailImportChanged { await vm.hydrate() }
        .onDisappear { vm.leavePage() }
    }

    // MARK: - Screen 1 — Source

    private var showsAppPassword: Bool { draftKind == .gmail || draftKind == .iCloud }
    private var showsImapFields: Bool { draftKind == .generic }

    /// The provider's app-password sentence — painted under the field and
    /// published as its `help` attribute (what the cross-app journey reads).
    private var appPasswordHelp: String {
        draftKind == .gmail
            ? L.mailImport.sourceAppPasswordHelpGmail
            : L.mailImport.sourceAppPasswordHelpIcloud
    }

    private var sourceSection: some View {
        groupedSection(title: L.mailImport.sourceTitle) {
            Picker(L.mailImport.sourceTitle, selection: Binding(
                get: { draftKind },
                set: { selectKind($0) }
            )) {
                ForEach(Self.sourceKinds, id: \.self) { kind in
                    Text(kindLabel(kind)).tag(kind)
                }
            }
            .accessibilityIdentifier(Ids.mailImportSourcePicker)
            .automationSelect(
                Ids.mailImportSourcePicker,
                value: { kindLabel(draftKind) },
                options: { Self.sourceKinds.map(kindLabel) },
                set: { label in
                    if let k = Self.sourceKinds.first(where: { kindLabel($0) == label }) {
                        selectKind(k)
                    }
                })

            // Painted for every provider kind — nothing in ui.yaml scopes
            // `mail-import-source-username` to a subset the way the other four
            // are annotated. Outlook asks for the Microsoft account's address,
            // the XOAUTH2 user.
            TextField(
                draftKind == .outlook
                    ? L.mailImport.sourceOutlookAddressLabel
                    : L.mailImport.sourceUsernamePlaceholder,
                text: $draftUsername)
                .accessibilityIdentifier(Ids.mailImportSourceUsername)
                .automationField(Ids.mailImportSourceUsername, text: $draftUsername)

            if showsAppPassword {
                SecureField(L.mailImport.sourceAppPasswordLabel, text: $draftAppPassword)
                    .accessibilityIdentifier(Ids.mailImportSourceAppPassword)
                    .automationField(
                        Ids.mailImportSourceAppPassword, text: $draftAppPassword,
                        attributes: { ["help": appPasswordHelp] })
                Text(appPasswordHelp)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if draftKind == .outlook {
                microsoftSignInRows
            }

            if showsImapFields {
                TextField(L.mailImport.sourceHostPlaceholder, text: $draftHost)
                    .accessibilityIdentifier(Ids.mailImportSourceHost)
                    .automationField(Ids.mailImportSourceHost, text: $draftHost)
                TextField(L.mailImport.sourcePortPlaceholder, text: $draftPort)
                    .accessibilityIdentifier(Ids.mailImportSourcePort)
                    .automationField(Ids.mailImportSourcePort, text: $draftPort)
                tlsModePicker
                SecureField(L.mailImport.sourcePasswordPlaceholder, text: $draftPassword)
                    .accessibilityIdentifier(Ids.mailImportSourcePassword)
                    .automationField(Ids.mailImportSourcePassword, text: $draftPassword)
            }

            // The Microsoft grant connects by itself — no Connect button beside
            // it. Deliberately NOT `.faunaGate`d: Connect is a `LOGIN` + `LIST`
            // against the FOREIGN source server over the client's own TLS
            // socket, not a nest RPC — no wire kind exists for it, and the
            // offline gate grades nest reachability. Only the four durable
            // session controls (Start/Pause/Resume/Cancel) touch the nest.
            if draftKind != .outlook {
                Button(L.mailImport.connectButton) { connect() }
                    .accessibilityIdentifier(Ids.mailImportConnectButton)
                    .automationActivate(Ids.mailImportConnectButton) { connect() }
            }

            // The account remembers the source a successful Connect reached;
            // Forget deletes the record and the machine resets the Source
            // fields (`mailbox-migration.md` § Credential handling → The
            // remembered source). Every provider, while a record exists.
            if vm.snapshot?.sourceRemembered ?? false {
                Button(L.mailImport.sourceForgetButton) { forgetSource() }
                    .accessibilityIdentifier(Ids.mailImportSourceForgetButton)
                    .automationActivate(Ids.mailImportSourceForgetButton) { forgetSource() }
            }
        }
    }

    /// The connection-security picker — the Source step's, and the restart-resume
    /// prompt's for a Generic session (the session row does not record it).
    private var tlsModePicker: some View {
        Picker(L.mailImport.tlsImplicit, selection: Binding(
            get: { vm.snapshot?.tlsMode ?? .implicit },
            set: { mode in Task { await vm.dispatch(.setTlsMode(mode: mode)) } }
        )) {
            ForEach(Self.tlsModes, id: \.self) { mode in
                Text(tlsLabel(mode)).tag(mode)
            }
        }
        .accessibilityIdentifier(Ids.mailImportSourceTlsMode)
        .automationSelect(
            Ids.mailImportSourceTlsMode,
            value: { tlsLabel(vm.snapshot?.tlsMode ?? .implicit) },
            options: { Self.tlsModes.map(tlsLabel) },
            set: { label in
                if let m = Self.tlsModes.first(where: { tlsLabel($0) == label }) {
                    Task { await vm.dispatch(.setTlsMode(mode: m)) }
                }
            })
    }

    /// The Outlook source's Microsoft sign-in: the button, the code the grant is
    /// waiting on beside **Open in browser**, and the sentence under them. A
    /// deployment without a registration paints the unavailable sentence and
    /// keeps the button inert (`MailImportSnapshot.microsoftSigninAvailable`).
    @ViewBuilder
    private var microsoftSignInRows: some View {
        let available = vm.snapshot?.microsoftSigninAvailable ?? false
        Button(L.mailImport.sourceOauthButton) {
            Task { await vm.signInWithMicrosoft(username: draftUsername) }
        }
        .disabled(!available || vm.microsoftSignInInFlight)
        .accessibilityIdentifier(Ids.mailImportSourceOauthButton)
        .automationActivate(
            Ids.mailImportSourceOauthButton,
            isEnabled: { available && !vm.microsoftSignInInFlight },
            perform: { Task { await vm.signInWithMicrosoft(username: draftUsername) } })
        if !available {
            Text(L.mailImport.microsoftUnavailable)
                .font(.caption)
                .foregroundStyle(.secondary)
        } else if let view = vm.snapshot?.microsoftSignin,
                  view.state == .waiting, !view.userCode.isEmpty {
            // The code and the address each ride alone as attributes — the
            // observables the cross-app journey reads (tui's `.attr("code", …)`).
            Text(L.mailImport.microsoftCodeFmt(url: view.verificationUri, code: view.userCode))
                .accessibilityIdentifier(Ids.mailImportSourceOauthCode)
                .automationValue(
                    Ids.mailImportSourceOauthCode,
                    text: { L.mailImport.microsoftCodeFmt(url: view.verificationUri, code: view.userCode) },
                    attributes: { ["code": view.userCode, "url": view.verificationUri] })
            // Id-less, like every other app's: the OS opener is the only effect.
            Button(L.mailImport.microsoftOpenBrowser) {
                if let url = URL(string: view.verificationUri) { openURL(url) }
            }
            Text(L.mailImport.microsoftWaiting)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Screen 2 — Scope

    private var scopeSection: some View {
        Group {
            groupedSection(title: L.mailImport.scopeMailboxesLabel) {
                let mailboxes = vm.snapshot?.mailboxes ?? []
                if mailboxes.isEmpty {
                    Text(L.mailImport.scopeMailboxesEmpty).foregroundStyle(.secondary)
                } else {
                    ForEach(Array(mailboxes.enumerated()), id: \.element.name) { _, mb in
                        // The row's own id is indexed (convention 1): its TEXT
                        // is the mailbox name — what the cross-app action layer
                        // matches on — and its VALUE is the `on`/`off` state
                        // contract, so `/element/text` and
                        // `/element/attr?attr=state` answer different things off
                        // one entry. `automationActivate` registers the toggle
                        // and both reads in ONE entry, so the same index slot
                        // serves click and read.
                        Toggle(mb.name, isOn: Binding(
                            get: { isSelected(mb) },
                            set: { _ in toggleMailbox(mb.name) }
                        ))
                        .accessibilityIdentifier(Ids.mailImportScopeMailboxItem)
                        .automationActivate(
                            Ids.mailImportScopeMailboxItem,
                            text: { mb.name },
                            value: { isSelected(mb) ? "on" : "off" },
                            perform: { toggleMailbox(mb.name) })
                    }
                }
            }
            .accessibilityIdentifier(Ids.mailImportScopeMailboxes)
            // A container's `.accessibilityIdentifier` alone does not register
            // it in the in-process registry — `is_visible`/`wait_for` read the
            // registry, not the AX tree.
            .accessibilityElement(children: .contain)
            .automationValue(Ids.mailImportScopeMailboxes, text: {
                L.mailImport.scopeMailboxesLabel
            })

            groupedSection(title: L.mailImport.scopeTitle) {
                TextField(L.mailImport.scopeDateFromPlaceholder, text: $draftDateFrom)
                    .accessibilityIdentifier(Ids.mailImportScopeDateFrom)
                    .automationField(Ids.mailImportScopeDateFrom, text: $draftDateFrom)
                TextField(L.mailImport.scopeMaxSizeLabel, text: $draftMaxSizeMb)
                    .accessibilityIdentifier(Ids.mailImportScopeMaxSize)
                    .automationField(Ids.mailImportScopeMaxSize, text: $draftMaxSizeMb)
                // The state is read from the snapshot, never a click echo; the
                // dispatch is the negated snapshot value. The label carries the
                // double-storage warning itself (`mailbox-migration.md` § Opt-out
                // per session) — never paint the toggle without it.
                Toggle(L.mailImport.scopeImportDuplicatesLabel, isOn: Binding(
                    get: { vm.snapshot?.importDuplicates ?? false },
                    set: { _ in toggleImportDuplicates() }
                ))
                .accessibilityIdentifier(Ids.mailImportScopeImportDuplicatesToggle)
                .automationActivate(
                    Ids.mailImportScopeImportDuplicatesToggle,
                    text: { L.mailImport.scopeImportDuplicatesLabel },
                    value: { (vm.snapshot?.importDuplicates ?? false) ? "on" : "off" },
                    perform: { toggleImportDuplicates() })
                // Informational only — no action changes the mapping (docs above).
                automationText(Ids.mailImportScopeMailboxMapping, L.mailImport.scopeMailboxMappingLabel)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                HStack {
                    backButton
                    Button(L.mailImport.next) { scopeNext() }
                        .accessibilityIdentifier(Ids.wizardNextButton)
                        .automationActivate(Ids.wizardNextButton) { scopeNext() }
                }
            }
        }
    }

    // MARK: - Screen 3 — Confirm

    private var confirmSection: some View {
        groupedSection(title: L.mailImport.confirmTitle) {
            automationText(Ids.mailImportConfirmSummary, confirmSummary)
            HStack {
                backButton
                Button(L.mailImport.startButton) { Task { await vm.dispatch(.start) } }
                    .accessibilityIdentifier(Ids.mailImportStartButton)
                    .automationActivate(Ids.mailImportStartButton) {
                        Task { await vm.dispatch(.start) }
                    }
                    // The durable commit — opens the `import_sessions` row.
                    .faunaGate("fauna.bridges.start_import_session")
            }
        }
    }

    /// The shared wizard Back, painted on Scope (→ Source) and Confirm
    /// (→ Scope). Only the active screen's copy is ever in the view tree.
    private var backButton: some View {
        Button(L.mailImport.back) { Task { await vm.dispatch(.back) } }
            .accessibilityIdentifier(Ids.wizardBackButton)
            .automationActivate(Ids.wizardBackButton) { Task { await vm.dispatch(.back) } }
    }

    // MARK: - Screen 4 — Progress

    private var progressSection: some View {
        groupedSection(title: L.mailImport.progressTitle) {
            automationText(Ids.mailImportProgressSummary, progressSummary)
            automationProgressBar(Ids.mailImportProgressBar, fraction: progressFraction)
            if vm.snapshot?.sessionState == .errored {
                // A failed import is a record the wizard shows until the user
                // moves on (`mailbox-migration.md` § A failed import …): counters
                // frozen, the reason as the log's last line, and Dismiss as the
                // one control — no credential field, Pause, Resume, restart
                // offer or Cancel (the machine refuses each with a sentence).
                Button(L.mailImport.dismissFailedButton) { Task { await vm.dispatch(.dismiss) } }
                    .accessibilityIdentifier(Ids.mailImportDismissButton)
                    .automationActivate(
                        Ids.mailImportDismissButton, text: { L.mailImport.dismissFailedButton }
                    ) {
                        Task { await vm.dispatch(.dismiss) }
                    }
                    .faunaGate("fauna.bridges.discard_import_session")
            } else {
                runControls
            }
            Text(L.mailImport.errorLogTitle)
                .font(.headline)
            automationText(Ids.mailImportErrorLog, (vm.snapshot?.errorLog ?? []).joined(separator: "\n"))
                .font(.caption.monospaced())
            mailboxProgressList
        }
    }

    /// The live session's controls: the restart-resume credential prompt, Pause,
    /// Resume, Cancel, the full-mailbox notice and the rebuilt-mailbox restart.
    @ViewBuilder
    private var runControls: some View {
        if let snap = vm.snapshot, snap.resumeNeedsCredentials {
            resumeCredentialRows(snap)
        }
        Group {
            HStack {
                Button(L.mailImport.pauseButton) { Task { await vm.dispatch(.pause) } }
                    .accessibilityIdentifier(Ids.mailImportPauseButton)
                    .automationActivate(Ids.mailImportPauseButton) {
                        Task { await vm.dispatch(.pause) }
                    }
                    .faunaGate("fauna.bridges.pause_import_session")
                Button(L.mailImport.resumeButton) { resume() }
                    .accessibilityIdentifier(Ids.mailImportResumeButton)
                    .automationActivate(Ids.mailImportResumeButton) { resume() }
                    .faunaGate("fauna.bridges.resume_import_session")
                Button(cancelLabel, role: .destructive) { tapCancel() }
                    .accessibilityIdentifier(Ids.mailImportCancelButton)
                    // `text:` reports the armed/unarmed label
                    // (`apple-e2e-automation.md` rule 11).
                    .automationActivate(Ids.mailImportCancelButton, text: { cancelLabel }) { tapCancel() }
                    // Arm and confirm are the same control (two-click inline),
                    // so this is the control that issues Cancel.
                    .faunaGate("fauna.bridges.cancel_import_session")
            }
            // The full-mailbox notice (§ Failure handling → Nest-side errors):
            // Resume and Cancel are the buttons above, Skip the rest is here.
            if let snap = vm.snapshot, snap.sessionState == .paused,
               snap.pauseReason == .mailboxFull {
                automationText(Ids.mailImportQuotaNotice, L.mailImport.quotaNotice)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button(L.mailImport.quotaSkipRestButton) { Task { await vm.dispatch(.skipRest) } }
                    .accessibilityIdentifier(Ids.mailImportQuotaSkipRestButton)
                    .automationActivate(Ids.mailImportQuotaSkipRestButton) {
                        Task { await vm.dispatch(.skipRest) }
                    }
                    .faunaGate("fauna.bridges.resume_import_session")
            }
            // A source mailbox the provider rebuilt (UIDVALIDITY changed) paused
            // the session; this drops its cursors and resumes. The run is
            // spawned by `MailImportVM` off `importActionStartsRun`.
            if let snap = vm.snapshot, !snap.rebuiltMailboxes.isEmpty {
                Button(L.mailImport.restartButton) { Task { await vm.dispatch(.restartRebuiltMailboxes) } }
                    .accessibilityIdentifier(Ids.mailImportRestartButton)
                    .automationActivate(Ids.mailImportRestartButton) {
                        Task { await vm.dispatch(.restartRebuiltMailboxes) }
                    }
                    .faunaGate("fauna.bridges.resume_import_session")
            }
        }
    }

    /// The Progress step of a session adopted after a restart
    /// (`MailImportSnapshot.resumeNeedsCredentials`, § Resume protocol step 2):
    /// the sentence saying why and the Source step's own credential field for
    /// the session's provider. Outlook asks for the Microsoft sign-in again.
    @ViewBuilder
    private func resumeCredentialRows(_ snap: MailImportSnapshot) -> some View {
        switch snap.sourceKind {
        case .gmail, .iCloud:
            Text(L.mailImport.resumeNeedsPassword).font(.caption)
            SecureField(L.mailImport.sourceAppPasswordLabel, text: $draftAppPassword)
                .accessibilityIdentifier(Ids.mailImportSourceAppPassword)
                .automationField(Ids.mailImportSourceAppPassword, text: $draftAppPassword)
        case .generic:
            Text(L.mailImport.resumeNeedsPassword).font(.caption)
            TextField(L.mailImport.sourcePortPlaceholder, text: $draftPort)
                .accessibilityIdentifier(Ids.mailImportSourcePort)
                .automationField(Ids.mailImportSourcePort, text: $draftPort)
            tlsModePicker
            SecureField(L.mailImport.sourcePasswordPlaceholder, text: $draftPassword)
                .accessibilityIdentifier(Ids.mailImportSourcePassword)
                .automationField(Ids.mailImportSourcePassword, text: $draftPassword)
        case .outlook:
            Text(L.mailImport.resumeNeedsMicrosoft).font(.caption)
            microsoftSignInRows
        }
    }

    /// Per-mailbox progress rows, painted from the shared
    /// `MailImportSnapshot.mailboxProgress` (one row per mailbox the run
    /// takes, laid at Start); the count text is the shared
    /// `importMailboxProgressText`.
    private var mailboxProgressList: some View {
        let rows = vm.snapshot?.mailboxProgress ?? []
        // The container is a real `VStack`, not a bare modifier on the
        // `ForEach`: a modifier applied to a `ForEach` lands on each ROW, so
        // the list id would register once per row (or, unregistered, not at
        // all — `is_visible`/`wait_for` read the registry, not the AX tree).
        // The `VStack` gives the id one host to sit on, and the
        // `automationValue` beside it is what actually puts it in the
        // registry (apple-e2e-automation.md § the AutomationRegistry contract).
        return VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(rows.enumerated()), id: \.element.name) { _, mb in
            HStack {
                automationText(Ids.mailImportMailboxProgressListItemName, mb.name)
                Spacer()
                automationText(Ids.mailImportMailboxProgressListItemProgress, renderLocalizedText(importMailboxProgressText(row: mb)))
                    .font(.caption.monospaced())
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.mailImportMailboxProgressListItem)
            .automationValue(Ids.mailImportMailboxProgressListItem, text: { mb.name })
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.mailImportMailboxProgressList)
        .automationValue(Ids.mailImportMailboxProgressList, text: {
            String(rows.count)
        })
    }

    // MARK: - Screen 5 — Done

    private var doneSection: some View {
        groupedSection(title: L.mailImport.doneTitle) {
            automationText(Ids.mailImportDoneSummary, doneSummary)
            HStack {
                Button(L.mailImport.viewImportedButton) { onNavigateToConversations() }
                    .accessibilityIdentifier(Ids.mailImportViewImportedButton)
                    .automationActivate(Ids.mailImportViewImportedButton) {
                        onNavigateToConversations()
                    }
                Button(L.mailImport.reviewSkippedButton) { showSkipped.toggle() }
                    .accessibilityIdentifier(Ids.mailImportReviewSkippedButton)
                    .automationActivate(Ids.mailImportReviewSkippedButton) {
                        showSkipped.toggle()
                    }
            }
            if showSkipped {
                Text(L.mailImport.errorLogTitle)
                    .font(.headline)
                automationText(Ids.mailImportErrorLog, (vm.snapshot?.errorLog ?? []).joined(separator: "\n"))
                    .font(.caption.monospaced())
            }
        }
        .onChange(of: step) { _, newStep in
            if newStep != .done { showSkipped = false }
        }
    }

    // MARK: - Transitions

    /// Provider pick. The visibility flip is applied SYNCHRONOUSLY here (see
    /// this type's docs); the dispatch is live rather than a draft because the
    /// machine's `SelectSourceKind` arm is what writes the provider preset
    /// (host/port/tls_mode) that `connectActions` then trusts the snapshot to
    /// carry.
    private func selectKind(_ kind: ImportSourceKind) {
        draftKind = kind
        Task {
            await vm.dispatch(.selectSourceKind(kind: kind))
            syncDraftsFromSnapshot()
        }
    }

    /// Step 1→2: commit whichever Source fields the picked kind actually shows,
    /// then Connect — one ordered op. The wire-action shape is the shared
    /// `connectActions`; this side's own job is locating the strings, which is
    /// why the kind-based routing between the password and app-password fields
    /// happens here, before the shared function ever sees a resolved secret.
    private func connect() {
        let secret = showsAppPassword ? draftAppPassword : draftPassword
        let actions = connectActions(
            kind: draftKind,
            host: draftHost,
            port: draftPort,
            username: draftUsername,
            password: secret)
        Task { await vm.dispatchSequence(actions) }
    }

    /// Forget the remembered source: the machine deletes the record and resets
    /// its Source fields; the drafts empty with them (the secrets were never
    /// remembered, so they go too), and a later remembered source may prefill
    /// afresh.
    private func forgetSource() {
        Task {
            await vm.dispatch(.forgetSource)
            draftUsername = ""
            draftPassword = ""
            draftAppPassword = ""
            sourcePrefilled = false
            syncDraftsFromSnapshot()
        }
    }

    /// The Scope toggle: the dispatch is the negated SNAPSHOT value, never a
    /// local echo, so the painted state is always the machine's.
    private func toggleImportDuplicates() {
        let next = !(vm.snapshot?.importDuplicates ?? false)
        Task { await vm.dispatch(.setImportDuplicates(value: next)) }
    }

    /// Step 2→3: commit both Scope drafts, then advance. `scopeNextActions`
    /// also owns the unparseable-max-size fallback.
    private func scopeNext() {
        let actions = scopeNextActions(dateFrom: draftDateFrom, maxSizeInput: draftMaxSizeMb)
        Task { await vm.dispatchSequence(actions) }
    }

    /// Step 5's Resume as one ordered dispatch — the shared `resumeActions`
    /// commits the credential the restart prompt painted before the `Resume`.
    private func resume() {
        guard let snap = vm.snapshot else { return }
        let secret = (snap.sourceKind == .gmail || snap.sourceKind == .iCloud)
            ? draftAppPassword : draftPassword
        let actions = resumeActions(
            needsCredentials: snap.resumeNeedsCredentials, kind: snap.sourceKind,
            port: draftPort, password: secret)
        Task { await vm.dispatchSequence(actions) }
    }

    /// The restart prompt's drafts: the port starts at the one the machine
    /// holds when the flag turns on, and the password drafts are dropped when
    /// it turns off (the secret lives no longer than the prompt).
    private func resumePromptChanged(to needs: Bool) {
        if needs {
            if let snap = vm.snapshot {
                draftPort = String(snap.port)
                if draftUsername.isEmpty { draftUsername = snap.username }
            }
        } else {
            draftPassword = ""
            draftAppPassword = ""
        }
    }

    /// Toggle one mailbox. The local echo is written FIRST and synchronously —
    /// the walk reads this row's `state` back with no wait (see this type's
    /// docs).
    private func toggleMailbox(_ name: String) {
        toggleMailboxPending(name, in: vm.snapshot?.mailboxes, pendingSelection: &pendingSelection)
        Task { await vm.dispatch(.toggleMailbox(mailbox: name)) }
    }

    /// The cancel button's label: `Cancel`, relabelled `Confirm?` once armed.
    /// Shared by the `Button` title and its `.automationActivate` `text:`.
    private var cancelLabel: String {
        cancelArmed ? L.common.confirmQ : L.mailImport.cancelButton
    }

    /// Two-click inline cancel arm-then-confirm (no modal). Shared by the
    /// `Button` and its `.automationActivate` so the e2e's two taps on the same
    /// id arm then dispatch Cancel.
    private func tapCancel() {
        if cancelArmed {
            cancelArmed = false
            Task { await vm.dispatch(.cancel) }
        } else {
            cancelArmed = true
            Task {
                try? await Task.sleep(for: .seconds(4))
                cancelArmed = false
            }
        }
    }

    /// Seed the drafts from the machine's own state after a snapshot that may
    /// have changed it — today only `SelectSourceKind`, which writes the
    /// provider preset. Never called on an ordinary re-read, so a failed
    /// connect keeps exactly what the user typed (§ Wizard steps step 2).
    private func syncDraftsFromSnapshot() {
        guard let snap = vm.snapshot else { return }
        // A remembered source prefills the Source drafts once per visit, never
        // over what the user typed (the snapshot arrives prefilled — kind,
        // host, port, TLS mode, username; the secret is never remembered).
        if snap.sourceRemembered, !sourcePrefilled {
            sourcePrefilled = true
            draftKind = snap.sourceKind
            if draftUsername.isEmpty { draftUsername = snap.username }
        }
        draftHost = snap.host
        draftPort = String(snap.port)
        if draftMaxSizeMb.isEmpty {
            draftMaxSizeMb = String(snap.maxSizeBytes / (1024 * 1024))
        }
    }

    private func isSelected(_ mb: SourceMailboxOption) -> Bool {
        isMailboxSelected(mb, pendingSelection: pendingSelection)
    }

    // MARK: - Derived text

    private static let sourceKinds: [ImportSourceKind] = [.gmail, .outlook, .iCloud, .generic]
    private static let tlsModes: [ImportTlsMode] = [.implicit, .startTls]

    private func kindLabel(_ kind: ImportSourceKind) -> String {
        renderLocalizedText(importSourceKindLabel(kind: kind))
    }

    private func tlsLabel(_ mode: ImportTlsMode) -> String {
        renderLocalizedText(importTlsModeLabel(mode: mode))
    }

    /// The Confirm summary: the source, the selected-mailbox count, their
    /// message total and — when every one of them was sized — roughly how much
    /// data (§ Wizard steps step 4), all from the shared
    /// `importSelectedTotals`; nothing is summed here.
    private var confirmSummary: String {
        guard let snap = vm.snapshot else { return "" }
        let totals = importSelectedTotals(snapshot: snap)
        let source = kindLabel(snap.sourceKind)
        let mailboxes = String(totals.mailboxes)
        let messages = String(totals.messages)
        if let bytes = totals.bytes {
            return L.mailImport.confirmSummarySizedFmt(
                source: source, mailboxes: mailboxes, messages: messages,
                size: ValueFormat.byteSize(bytes))
        }
        return L.mailImport.confirmSummaryFmt(
            source: source, mailboxes: mailboxes, messages: messages)
    }

    /// The shared elapsed time / time left of this session
    /// (`mailbox-migration.md` § Wizard steps steps 5–6), at the device's clock.
    private func timing(_ snap: MailImportSnapshot) -> TransferTiming {
        MailTransferText.timing(
            startedAt: snap.startedAt, lastProgressAt: snap.lastProgressAt,
            running: snap.sessionState == .running,
            processed: UInt64(snap.importedCount) + UInt64(snap.skippedCount) + UInt64(snap.erroredCount),
            total: UInt64(snap.totalCount))
    }

    private var progressSummary: String {
        guard let snap = vm.snapshot else { return "" }
        return MailTransferText.importProgress(
            summary: L.mailImport.progressSummaryFmt(
                imported: String(snap.importedCount), total: String(snap.totalCount),
                skipped: String(snap.skippedCount), errored: String(snap.erroredCount)),
            timing: timing(snap))
    }

    private var progressFraction: Double {
        guard let snap = vm.snapshot else { return 0 }
        return quotaFraction(usedBytes: Int64(snap.importedCount), maxBytes: Int64(snap.totalCount))
    }

    private var doneSummary: String {
        guard let snap = vm.snapshot else { return "" }
        return MailTransferText.importDone(
            imported: UInt64(snap.importedCount), skipped: UInt64(snap.skippedCount),
            errored: UInt64(snap.erroredCount), timing: timing(snap))
    }
}

// `groupedSection` (`GroupedSection.swift`) is the shared eager-container
// replacement for `Form`'s `Section` this file's wizard steps use.
#endif // !FAUNA_KIDS
