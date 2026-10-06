import SwiftUI

/// The user-facing **mail-settings** page (`docs/goal/ui/mail-settings.md`),
/// shared by macOS + iOS (one FaunaKit view, thin per-target call sites —
/// A dumb renderer of `MailSettingsSnapshot` + dispatcher
/// of `MailSettingsAction` over the shared `MailSettingsMachine` (via
/// `MailSettingsVM`); no business logic here. Element IDs match
/// `tests/e2e-unified/ui.yaml` `mail-settings` / `mail-add-credential` /
/// `mail-rotate-keys-confirm` exactly. Reference implementation: linux
/// `apps/fauna-linux/src/settings/mail.rs`. All labels — including the three
/// destructive warnings (disable / rotate / weak-password), now canonicalized to
/// the merged-superset wording shared across all 7 apps — are sourced from the
/// shared `L.settings.mail.*` (+ `L.mailSettings.*`) i18n keys; the only strings
/// still inline are a couple of no-key formatting fragments ("Saving…",
/// "Generating…", the strength label).
///
/// **Container = `ScrollView { VStack { GroupBox } }`, NOT a `Form`.** A SwiftUI
/// `Form` is a lazy `List` on iOS, so once mail is enabled the page grows past
/// one screen and its lower sections (serve-here, MUA, status indicator) never
/// fire the `.onAppear` the in-process `AutomationRegistry` rides — they stay
/// unregistered and unreadable to the iOS driver (the status indicator reads
/// empty, the MUA block is absent), while macOS realizes every row eagerly and so
/// ran 8/8 green. The eager `ScrollView`+`VStack`+`GroupBox` shape — every
/// `Admin*View`'s idiom — realizes all children on both platforms, so every
/// element registers regardless of scroll position. See
/// `docs/goal/architecture/apps/apple-e2e-automation.md` § limitation (b)
/// (lazy containers) and `mail-settings.md` § Disable-mail implementation status.
public struct MailSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = MailSettingsVM()

    /// The add-credential reveal mode, keyed on the per-gesture intent (the
    /// enabled-toggle on-path = `.enable` first-credential; the manage "Add
    /// credential" button = `.add`), NOT derived from `vm.enabled`. The mode is
    /// *inherently* gesture-scoped and cannot be recovered from state: a
    /// CalDAV-only mailbox has `enabled == false` yet is provisioned, and enabling
    /// email on it is an additive `EnableMail` upgrade (`machine.rs` `enable_mail`,
    /// `Some(msek)` arm) — so an email-off actor's submit is ambiguous between the
    /// enable-toggle's `EnableMail` and the add-button's `AddCredential`.
    ///
    /// Presented as an **inline `@State`-driven reveal** mounted in `body`'s
    /// `VStack` (`if let mode = addCredentialMode { MailAddCredentialSheet(…) }`),
    /// NOT a system `.sheet` — a `.sheet`'s children don't reliably
    /// `.onAppear`-register for the iOS in-process automation driver (the
    /// `mail-add-credential-name-input` count==0 / racy-submit gap), so a
    /// `.sheet`-presented form never registered on iOS; inline mounts in the body's
    /// view tree so its children register deterministically. Inline is also the
    /// established cross-app shape web + linux render for add-credential
    /// (`mail-settings.md` § Add credential), matching this page's own
    /// inline `disableConfirmSection`. `body` reads `mode` FRESH at mount, so — like
    /// the old `.sheet(item:)` — it can NOT capture a stale mode the way
    /// `.sheet(isPresented:)` + a separate `@State mode` did: that capture was the
    /// original regression (an add-credential after a navigate-away/return reset the
    /// `@State` mode to its `.enable` default → `EnableMail` on an already-enabled
    /// actor → nest "mail already enabled"). `.id(mode)` re-creates fresh `@State`
    /// on a mode change, matching the old `.sheet(item:)` identity semantics.
    /// Mirrors the linux/windows gesture-based `FormMode::Enable` vs `Add` split
    /// (`apps/fauna-linux/src/settings/mail.rs` `open_form`), whose inline-reveal
    /// form reads its gesture mode at submit and so never had the capture footgun.
    @State private var addCredentialMode: MailCredentialFormMode?
    @State private var showingRotate = false
    @State private var confirmDisable = false

    public init() {}

    public var body: some View {
        // Eager container (ScrollView + VStack + GroupBox), NOT a lazy `Form`, so
        // every element registers in-process on iOS regardless of scroll position —
        // see the type doc above.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                enabledSection
                disableConfirmSection
                // The add-credential form is an **inline `@State`-driven reveal**, NOT a
                // system `.sheet`: a `.sheet`'s children don't reliably `.onAppear`-
                // register for the iOS in-process driver, so the form's
                // `mail-add-credential-name-input` never registered there. Mounting it in
                // the body's VStack (the same shape `disableConfirmSection` uses, and the
                // inline reveal web + linux render) registers the children
                // deterministically. Placed OUTSIDE the `enabled || caldavEnabled` gate
                // below because the enable-toggle on-path opens it in `.enable` mode while
                // the mailbox is still unprovisioned (both flags false). `.id(mode)` gives
                // a fresh `@State` per mode, matching the old `.sheet(item:)` identity.
                if let mode = addCredentialMode {
                    MailAddCredentialSheet(vm: vm, mode: mode) { addCredentialMode = nil }
                        .id(mode)
                }
                // The credential-management section, serve-here toggle, and MUA block
                // render whenever the actor has a **provisioned mailbox** — email OR
                // CalDAV — because the one shared `default` credential AUTHs IMAP +
                // SMTP + CalDAV, so a CalDAV-only deployment (email off, calendar on)
                // must still reach it to obtain/rotate the bridge password its CalDAV
                // MUA needs (`mail-settings.md` § CalDAV-only mailbox; `caldav-server.md`
                // § Independent enablement). The enabled-toggle + status stay
                // email-specific (gated on `enabled`). Mirrors linux
                // (`mail.rs` `let mailbox = enabled || caldav_enabled`) + windows.
                // The credential-management reachability predicate — READ from the shared
                // snapshot field (`enabled || caldav_enabled || carddav_enabled ||
                // serves_webdav_set`, computed once in shared Rust), never re-derived
                // here: a client-side disjunction silently goes stale the moment a new
                // DAV sibling lands (apple's used to read `enabled || caldavEnabled`,
                // hiding this section from a CardDAV-only or WebDAV-only actor who still
                // needs the one shared bridge credential). mail-settings.md
                // § Credential-management reachability.
                if vm.credentialManagementReachable {
                    if let pending = vm.snapshot?.pendingRotation {
                        pendingRotationBanner(pending)
                    }
                    manageSection
                    // Rotate-keys is likewise an inline reveal (same `.sheet`
                    // in-process-registration gap). Reachable only with ≥1 credential, so
                    // it lives inside the provisioned-mailbox gate, below the manage
                    // section whose `mail-settings-rotate-keys-button` arms it.
                    if showingRotate {
                        MailRotateKeysSheet(vm: vm) { showingRotate = false }
                    }
                    serveSection
                    muaSection
                }
                statusSection
                if let error = vm.errorMessage {
                    groupedSection { ErrorBanner(message: error) }
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.mailSettings.title)
        .task {
            guard let client else { return }
            let handle = client.sessionMaterial?.handle ?? ""
            await vm.configure(api: client.api, handle: handle)
        }
    }

    // MARK: - Enable toggle

    private var enabledSection: some View {
        groupedSection {
            Toggle(L.settings.mail.enableTitle, isOn: Binding(
                get: { vm.enabled },
                set: { wantOn in setEnabled(wantOn) }
            ))
            .accessibilityIdentifier(Ids.mailSettingsEnabledToggle)
            .automationActivate(
                Ids.mailSettingsEnabledToggle,
                value: { vm.enabled ? "on" : "off" }
            ) { setEnabled(!vm.enabled) }
            if !vm.enabled {
                Text(L.settings.mail.sectionDescription)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: - Disable-mail confirm (inline destructive overlay)

    /// Inline destructive disable-confirm (the enabled-toggle off-path sets
    /// `confirmDisable`). Option (b) — an inline overlay group rather than a
    /// system `.confirmationDialog` — so BOTH ui.yaml IDs attach to real
    /// elements (`mail-settings-disable-confirm` on the container,
    /// `mail-settings-disable-confirm-button` on the destructive action). This
    /// is the uniform cross-app shape: windows uses an inline destructive
    /// disable-confirm overlay and web a disable-confirm overlay, both carrying
    /// the same 2 IDs. Confirming dispatches the
    /// shared `MailSettingsAction.disableMail` (bulk-revoke every credential +
    /// clear the MSEK per `mail-settings.md` § Disable mail); cancel leaves the
    /// toggle on.
    @ViewBuilder
    private var disableConfirmSection: some View {
        if confirmDisable {
            groupedSection {
                VStack(alignment: .leading, spacing: 8) {
                    // Title + consequence warning from the shared `L.settings.mail.*`
                    // keys. `disableTitle` ("Disable mail?") renders as the heading
                    // here as it does on linux/android/windows; `disableWarning` is
                    // consequence-only (no question prefix), so the two never double up.
                    Text(L.settings.mail.disableTitle)
                        .font(.headline)
                    Text(L.settings.mail.disableWarning)
                        .font(.callout)
                    HStack {
                        Button(L.settings.mail.disableConfirm, role: .destructive) {
                            confirmDisableMail()
                        }
                        .accessibilityIdentifier(Ids.mailSettingsDisableConfirmButton)
                        .automationActivate(Ids.mailSettingsDisableConfirmButton) {
                            confirmDisableMail()
                        }
                        // `DisableMail` bulk-revokes every credential at the
                        // nest before clearing the MSEK. Arming is local: the
                        // enabled-toggle only opens this overlay (`setEnabled`
                        // sets `confirmDisable`), so it stays live — as does
                        // the cancel beside this button.
                        .faunaGate("fauna.bridges.revoke_wrapped_mls_blob")
                        Button(L.settings.mail.cancel, role: .cancel) { confirmDisable = false }
                    }
                }
                // `.contain` keeps BOTH this container id AND the child
                // `mail-settings-disable-confirm-button` queryable — a bare
                // container id on a VStack otherwise clobbers every child id
                // (the e2e waits on the button, not the container).
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.mailSettingsDisableConfirm)
            }
        }
    }

    // MARK: - Pending-rotation banner

    private func pendingRotationBanner(_ pending: PendingRotationStatus) -> some View {
        groupedSection {
            VStack(alignment: .leading, spacing: 8) {
                Text("\(L.settings.mail.bannerTitle) \(L.settings.mail.bannerSubtitle)")
                    .font(.callout)
                Button(L.settings.mail.resume) { Task { await vm.dispatch(.resumeRotation) } }
                    .accessibilityIdentifier(Ids.mailSettingsPendingRotationResumeButton)
            }
            // `.contain` keeps both this container id and the child resume-button
            // id queryable (same clobber guard as the disable-confirm overlay).
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.mailSettingsPendingRotationBanner)
        }
    }

    // MARK: - Manage (add / rotate / keys-info)

    /// The credentials list moved to Settings → Connected apps
    /// (`mail-settings.md` § Where the credential rows render): the app
    /// passwords are rows of that roster, with their login, kind and the
    /// reveal / copy controls. This page keeps *Add password*, *Rotate keys*,
    /// the keys explainer and one pointer line — a row moves, it is never shown
    /// twice. The pointer is chrome, with no id.
    private var manageSection: some View {
        groupedSection {
            Text(L.mailSettings.credentialsOnConnectedApps)
                .font(.caption)
                .foregroundStyle(.secondary)
            Button(L.settings.mail.addCredential) { openAddCredential() }
                .accessibilityIdentifier(Ids.mailSettingsAddCredentialButton)
                .automationActivate(Ids.mailSettingsAddCredentialButton) {
                    openAddCredential()
                }
            if !(vm.snapshot?.credentials.isEmpty ?? true) {
                Button(L.settings.mail.rotateKeys) { showingRotate = true }
                    .accessibilityIdentifier(Ids.mailSettingsRotateKeysButton)
                    .automationActivate(
                        Ids.mailSettingsRotateKeysButton,
                        isEnabled: { !isRotating }
                    ) { showingRotate = true }
                    .disabled(isRotating)
            }
            automationText(Ids.mailSettingsKeysInfo, L.mailSettings.keysInfo)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Serve-here toggle

    private var serveSection: some View {
        groupedSection {
            Toggle(isOn: Binding(
                get: { vm.snapshot?.servingEnabled ?? true },
                set: { on in setServingEnabled(on) }
            )) {
                VStack(alignment: .leading) {
                    Text(L.mailSettings.serveHereLabel)
                    Text(L.mailSettings.serveHereSubtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .accessibilityIdentifier(Ids.mailSettingsServeHereToggle)
            .automationActivate(
                Ids.mailSettingsServeHereToggle,
                value: { (vm.snapshot?.servingEnabled ?? true) ? "on" : "off" }
            ) { setServingEnabled(!(vm.snapshot?.servingEnabled ?? true)) }
            // Dispatch-on-change, and non-optimistic by design: the rendered
            // value only flips after the nest write returns, so with no nest it
            // could never move.
            .faunaGate("fauna.bridges.set_mail_serving_enabled")
        }
    }

    // MARK: - MUA instructions

    @ViewBuilder
    private var muaSection: some View {
        if let mua = vm.snapshot?.mua {
            groupedSection {
                // The component-level `mail-settings-mua-instructions` id must ride a
                // registered leaf, NOT a bare container: a group container's own
                // `.accessibilityIdentifier` doesn't surface as a queryable automation
                // element, so the e2e `wait_for("mail-settings-mua-instructions")`
                // never resolved the wrapper even though every child row registered.
                // Carry the id on the block's header `Text` — the per-row/card sentinel
                // pattern that already works for keys-info.
                automationText(Ids.mailSettingsMuaInstructions, L.settings.mail.muaTitle)
                    .font(.headline)
                // Per-protocol within the one `mail-settings-mua-instructions` block:
                // the IMAP/SMTP host+port rows describe the *email* protocol → gated
                // on `enabled`; the CalDAV host+port rows describe the *calendar*
                // protocol → gated on `caldavEnabled` (`mail.<domain>:443`, served
                // independent of email — `caldav-server.md` § Network exposure); the
                // username-format + auth-mechanism rows are shared by both (one
                // credential AUTHs IMAP+SMTP+CalDAV) → shown whenever the block is.
                if vm.enabled {
                    muaField(L.settings.mail.muaImapHost, mua.imapHost, "mail-settings-mua-imap-host")
                    muaField(L.settings.mail.muaImapPort, String(mua.imapPort), "mail-settings-mua-imap-port")
                    muaField(L.settings.mail.muaSmtpHost, mua.smtpHost, "mail-settings-mua-smtp-host")
                    muaField(L.settings.mail.muaSmtpPort, String(mua.smtpPort), "mail-settings-mua-smtp-port")
                }
                if vm.caldavEnabled {
                    muaField(L.settings.mail.muaCaldavHost, mua.caldavHost, "mail-settings-mua-caldav-host")
                    muaField(L.settings.mail.muaCaldavPort, String(mua.caldavPort), "mail-settings-mua-caldav-port")
                }
                // The FILES protocol — a full collection-root URL rather than a host+port
                // pair, because no SRV autodiscovery exists for WebDAV, so this line is
                // the primary setup surface (the user mounts it in Finder / GNOME Files /
                // rclone). Gated on the PER-ACTOR serve state, never the deployment-wide
                // `webdav_enabled`: that toggle is harmless-on by default, so gating on it
                // would show every actor a dead mount URL. mail-settings.md § WebDAV files.
                if vm.servesWebdavSet {
                    muaField(L.settings.mail.muaWebdavUrl, mua.webdavUrl, "mail-settings-mua-webdav-url")
                }
                muaField(L.settings.mail.muaUsername, mua.usernameFormat, "mail-settings-mua-username-format")
                muaField(L.settings.mail.muaAuth, mua.authMechanism, "mail-settings-mua-auth-mechanism")
            }
        }
    }

    private func muaField(_ label: String, _ value: String, _ id: String) -> some View {
        HStack {
            Text(label).foregroundStyle(.secondary)
            Spacer()
            Text(value)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .accessibilityIdentifier(id)
            CopyButton("\(id)-copy", text: value)
        }
        // Read-only value label with a sibling CopyButton — register the value
        // read on the row so `get_text(id)` resolves the rendered MUA detail.
        .automationValue(id, text: { value })
    }

    // MARK: - Status indicator

    private var statusSection: some View {
        groupedSection {
            automationText(Ids.mailSettingsStatusIndicator, statusText)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var statusText: String {
        mailStatusText(enabled: vm.enabled, status: vm.snapshot?.status ?? .idle)
    }

    private var isRotating: Bool {
        if case .rotationInProgress = vm.snapshot?.status { return true }
        return false
    }

    /// Flip the enable toggle. Shared by the `Toggle`'s `set` closure and its
    /// `.automationActivate` so the two never diverge: turning on opens the
    /// inline add-credential reveal in `.enable` mode (first credential = enable
    /// mail); turning off arms the inline disable-confirm overlay.
    private func setEnabled(_ wantOn: Bool) {
        if wantOn {
            addCredentialMode = .enable
        } else {
            confirmDisable = true
        }
    }

    /// Open the inline add-credential reveal in `.add` mode (an *additional*
    /// credential — the mailbox is already provisioned, so this is reachable for a
    /// CalDAV-only actor whose `enabled` is false). Shared by the manage `Button`
    /// and its `.automationActivate` so the two never diverge. Mirrors linux
    /// `open_form(FormMode::Add)` (the add button is always Add; only the
    /// enabled-toggle on-path is Enable).
    private func openAddCredential() {
        addCredentialMode = .add
    }

    /// Confirm the destructive disable. Shared by the confirm `Button` and its
    /// `.automationActivate` (>1 statement, so extracted per the convention).
    private func confirmDisableMail() {
        Task {
            await vm.dispatch(.disableMail)
            confirmDisable = false
        }
    }

    /// Flip the local IMAP/CalDAV serve-here flag. Shared by the serve-here
    /// `Toggle`'s `set` closure and its `.automationActivate` so the two never
    /// diverge (non-optimistic — the rendered value flips only after the nest
    /// write returns).
    private func setServingEnabled(_ enabled: Bool) {
        Task { await vm.dispatch(.setServingEnabled(enabled: enabled)) }
    }
}

// MARK: - Status-indicator text (cross-app parity)

/// The `mail-settings-status-indicator` string. Delegates to the shared-Rust
/// `settings_status_label(status:enabled:)` (`fauna-client-mail-settings`) — the
/// one cross-app source of truth for the four-arm `SettingsStatus`(+`enabled`)
/// → i18n-key decision that linux/web/android consume too. An
/// `Idle` status reads "All up to date" **only when mail is enabled** — a disabled
/// mailbox reads "Mail is disabled" — because `SettingsStatus.idle` covers both
/// the disabled-and-idle and enabled-and-idle states (distinguished by `enabled`,
/// not the status enum). Apple was once the lone client reporting enabled-when-off
/// (which also made the e2e's `wait_for_enabled_status` poll false-positive against
/// an actually-off mailbox); routing through the shared fn forecloses that re-drift.
/// Kept as a thin Swift seam so the call site and the parity unit test
/// (`MailSettingsStatusTextTests`) are unchanged.
func mailStatusText(enabled: Bool, status: SettingsStatus) -> String {
    renderLocalizedText(settingsStatusLabel(status: status, enabled: enabled))
}

// MARK: - Add-credential sheet (`mail-add-credential`)

/// Which dispatch the add-credential submit maps to — first credential on a
/// never-enabled actor (`EnableMail`) vs. a new credential (`AddCredential`).
enum MailCredentialFormMode: Identifiable { case enable, add; var id: Self { self } }

struct MailAddCredentialSheet: View {
    let vm: MailSettingsVM
    let mode: MailCredentialFormMode
    /// Closes the inline reveal (clears the parent's `addCredentialMode`). Replaces
    /// the old `.sheet` `@Environment(\.dismiss)` — an inline reveal has no
    /// presentation layer to dismiss, so the parent owns the close.
    let onClose: () -> Void

    @State private var name = ""
    @State private var kind: CredentialKind = .oAuthBearer
    @State private var autoGenerate = true
    @State private var password = ""
    @State private var showPassword = false
    /// OAUTHBEARER token minted on appear — shown once for the user to copy.
    @State private var token = ""
    @State private var submitting = false
    @State private var error: String?

    // Mounted INLINE inside `MailSettingsView`'s eager `VStack`, so the body is a
    // `VStack` of `groupedSection` cards (NOT a `Form`) — the same eager shape
    // the host uses, so the form's children `.onAppear`-register in-process on iOS.
    // The token-gen lifecycle hooks ride the first (always-present) card so they
    // fire once, not once-per-card.
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            groupedSection {
                TextField(L.settings.mail.namePlaceholder, text: $name)
                    .accessibilityIdentifier(Ids.mailAddCredentialNameInput)
                    .automationField(Ids.mailAddCredentialNameInput, text: $name)
                Picker("Type", selection: $kind) {
                    Text(L.settings.mail.kindBearer).tag(CredentialKind.oAuthBearer)
                    Text(L.settings.mail.kindPassword).tag(CredentialKind.plain)
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier(Ids.mailAddCredentialTypeSelector)
                // The linux lead drives this kind-selector with a single click
                // (OAUTHBEARER → PLAIN), so the in-process driver actuates it the
                // same way: clicking flips between the two kinds. `value` reads the
                // current kind so `get_text`/`attr` reports it.
                .automationActivate(
                    Ids.mailAddCredentialTypeSelector,
                    value: { kind == .plain ? "plain" : "oauthbearer" }
                ) { kind = (kind == .plain ? .oAuthBearer : .plain) }
            }
            .onAppear { if kind == .oAuthBearer { token = generateBridgeToken() } }
            .onChange(of: kind) { _, newKind in
                if newKind == .oAuthBearer && token.isEmpty { token = generateBridgeToken() }
            }

            if kind == .plain {
                groupedSection {
                    Toggle(L.settings.mail.autogenerate, isOn: $autoGenerate)
                        .accessibilityIdentifier(Ids.mailAddCredentialAutogenerateToggle)
                        .automationActivate(
                            Ids.mailAddCredentialAutogenerateToggle,
                            value: { autoGenerate ? "on" : "off" }
                        ) { autoGenerate.toggle() }
                    // Mounted for the whole PLAIN block, never gated on `autoGenerate`:
                    // with auto-generate ON this field *is* where the minted secret is
                    // shown-once (read-only) for the user to copy into their MUA
                    // (mail-credentials.md § Auto-generated bridge password). Gating the
                    // mount on the toggle leaves the generated secret with nowhere to
                    // appear.
                    HStack {
                        if showPassword {
                            TextField(L.settings.mail.passwordPlaceholder, text: $password)
                                .accessibilityIdentifier(Ids.mailAddCredentialPasswordInput)
                                .automationField(
                                    Ids.mailAddCredentialPasswordInput,
                                    text: $password,
                                    isEnabled: { !autoGenerate }
                                )
                                .disabled(autoGenerate)
                        } else {
                            SecureField(L.settings.mail.passwordPlaceholder, text: $password)
                                .accessibilityIdentifier(Ids.mailAddCredentialPasswordInput)
                                .automationField(
                                    Ids.mailAddCredentialPasswordInput,
                                    text: $password,
                                    isEnabled: { !autoGenerate }
                                )
                                .disabled(autoGenerate)
                        }
                        Button(showPassword ? L.settings.mail.hide : L.settings.mail.show) { showPassword.toggle() }
                            .controlSize(.small)
                            .accessibilityIdentifier(Ids.mailAddCredentialPasswordShowToggle)
                            .automationActivate(Ids.mailAddCredentialPasswordShowToggle) {
                                showPassword.toggle()
                            }
                    }
                    // Absent while auto-generate is ON, not merely blank-texted: the
                    // strength of a client-minted ~143-bit secret is not a user
                    // decision (linux hides the same widget; `ErrorBanner`'s
                    // "a registered-but-empty element would read as present" applies
                    // here too — a bare `Text("")` would still count as present).
                    if !autoGenerate {
                        automationText(
                            Ids.mailAddCredentialPasswordStrengthMeter,
                            passwordStrengthLabel(password: password).map { renderLocalizedText($0) } ?? ""
                        )
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                    if warnWeak {
                        automationText(Ids.mailAddCredentialWeakPasswordWarning, L.settings.mail.weakPasswordWarning)
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }
                // Mint on reveal and on every auto-generate turn-on; clear for manual
                // entry on turn-off. Both hooks are needed: `onChange` alone misses the
                // first reveal, and driving it from the toggle's automation closure
                // alone would skip a *real* user's tap (which mutates the binding, not
                // the closure).
                .onAppear { applyAutogenState() }
                .onChange(of: autoGenerate) { _, _ in applyAutogenState() }
            } else {
                groupedSection {
                    Text(token.isEmpty ? "Generating…" : token)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .accessibilityIdentifier(Ids.mailAddCredentialTokenDisplay)
                        // Report the live token only (empty while generating) so
                        // the driver's `_wait_for_nonempty_token` poll resolves the
                        // minted token, not the "Generating…" placeholder.
                        .automationValue(Ids.mailAddCredentialTokenDisplay, text: { token })
                    CopyButton(Ids.mailAddCredentialTokenCopyButton, text: token)
                    Text(L.settings.mail.tokenWarning)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            if let error {
                groupedSection { ErrorBanner(message: error) }
            }

            groupedSection {
                Button(submitting ? "Saving…" : (mode == .enable ? L.settings.mail.submitEnable : L.settings.mail.submitAdd)) {
                    Task { await submit() }
                }
                .disabled(submitting || name.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier(Ids.mailAddCredentialSubmitButton)
                .automationActivate(
                    Ids.mailAddCredentialSubmitButton,
                    isEnabled: { !submitting && !name.trimmingCharacters(in: .whitespaces).isEmpty }
                ) { Task { await submit() } }
                // Both modes (`.enable` and `.add`) provision the credential's
                // resting blobs at the nest. Every field in this form, and the
                // cancel below, stay live — the commit gates, not the buffer.
                .faunaGate("fauna.bridges.provision_wrapped_mls_blob")
                Button(L.settings.mail.cancel, role: .cancel) { onClose() }
                    .accessibilityIdentifier(Ids.mailAddCredentialCancelButton)
                    .automationActivate(Ids.mailAddCredentialCancelButton) { onClose() }
            }
        }
    }

    private var warnWeak: Bool {
        // Every nest stores at rest encrypted (no-modes, storage-modes.md), so
        // nest_encrypted is unconditionally true — the same constant every app
        // passes to warn_manual_password.
        warnManualBridgePassword(autoGenerate: autoGenerate, nestEncrypted: true)
    }

    /// Mirror of linux's `apply_autogen_state` (`settings/mail.rs:1568`), which its
    /// kind-selector and auto-generate toggle both drive: with auto-generate ON the
    /// password field holds a freshly-minted ~143-bit secret and is read-only;
    /// turning it off clears the field for manual entry (and reveals the
    /// weak-password warning via `warnWeak`). The PLAIN-only, settled-toggle-edge
    /// mint decision is shared Rust now (`resolveAutogeneratedBridgePassword`) —
    /// call ONLY here, never at submit, so a re-mint between display and submit
    /// (a real apple bug this decision point hit 2026-07-13) can't reappear.
    private func applyAutogenState() {
        if let generated = resolveAutogeneratedBridgePassword(kind: kind, autoGenerate: autoGenerate) {
            password = generated
            showPassword = false
        } else {
            password = ""
        }
    }

    private func submit() async {
        submitting = true
        defer { submitting = false }
        let secretString: String
        switch kind {
        case .oAuthBearer:
            secretString = token
        case .plain:
            // The field is the single source of truth (linux submits
            // `password_input.text()` for the same reason): under auto-generate it
            // already holds the shown-once secret, and minting a fresh one here would
            // persist a credential that differs from the one the user copied.
            secretString = password
        }
        let secret = Data(secretString.utf8)
        let action: MailSettingsAction = mode == .enable
            ? .enableMail(displayName: name, kind: kind, secret: secret)
            : .addCredential(displayName: name, kind: kind, secret: secret)
        await vm.dispatch(action)
        if let e = vm.errorMessage {
            error = e
            return
        }
        // PLAIN closes on success — the user already has the secret: they either typed
        // it or copied it out of the read-only field before submitting.
        //
        // OAUTHBEARER must NOT close. The bearer token is shown ONCE and kept nowhere
        // recoverable (`mail-credentials.md` § OAUTHBEARER token issuer: "displays it
        // once on the credential-add screen … does not persist a recoverable copy";
        // lose it and your only move is revoke + re-add). Closing the form the instant
        // the mint succeeds destroys the reveal at exactly the moment it first refers to
        // a credential that actually exists — leaving the user to have copied a token
        // before knowing it would persist. linux/windows/web all keep the reveal up
        // after success (the shared `enable_mail_oauthbearer` action reads the token
        // back from it); apple was the outlier. The user closes it themselves via
        // `mail-add-credential-cancel-button`, having copied the token.
        if kind == .plain { onClose() }
    }
}

// MARK: - Rotate-keys sheet (`mail-rotate-keys-confirm`)

struct MailRotateKeysSheet: View {
    let vm: MailSettingsVM
    /// Closes the inline reveal (clears the parent's `showingRotate`). Replaces the
    /// old `.sheet` `@Environment(\.dismiss)` — see `MailAddCredentialSheet.onClose`.
    let onClose: () -> Void
    @State private var excluded: Set<String> = []
    @State private var rotating = false

    // Mounted INLINE inside `MailSettingsView`'s eager `VStack`: a `VStack` of
    // `groupedSection` cards (NOT a `Form`), the same eager shape as the host —
    // the same in-process-registration reason as `MailAddCredentialSheet`.
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            groupedSection {
                Text(L.settings.mail.rotateWarning)
                    .font(.callout)
                    .accessibilityIdentifier(Ids.mailRotateKeysWarningText)
            }
            groupedSection(title: "Exclude compromised credentials") {
                ForEach(vm.snapshot?.credentials ?? [], id: \.credentialId) { cred in
                    Toggle(cred.displayName, isOn: Binding(
                        get: { excluded.contains(cred.credentialId) },
                        set: { on in
                            if on { excluded.insert(cred.credentialId) }
                            else { excluded.remove(cred.credentialId) }
                        }
                    ))
                }
            }
            .accessibilityIdentifier(Ids.mailRotateKeysExcludeList)
            if rotating {
                groupedSection {
                    ProgressView()
                        .accessibilityIdentifier(Ids.mailRotateKeysProgressIndicator)
                }
            }
            if let error = vm.errorMessage {
                groupedSection { ErrorBanner(message: error) }
            }
            groupedSection {
                Button(L.settings.mail.rotateConfirm, role: .destructive) {
                    confirmRotate()
                }
                .disabled(rotating)
                .accessibilityIdentifier(Ids.mailRotateKeysConfirmButton)
                .automationActivate(
                    Ids.mailRotateKeysConfirmButton,
                    isEnabled: { !rotating }
                ) { confirmRotate() }
                // Rotation revokes each old blob and provisions its replacement
                // at the nest (`rotation.rs`). Arming — `mail-settings-rotate-
                // keys-button` — and the per-credential exclusion checkboxes are
                // local, so only this confirm declares.
                .faunaGate("fauna.bridges.provision_wrapped_mls_blob")
                Button(L.settings.mail.cancel, role: .cancel) { onClose() }
                    .accessibilityIdentifier(Ids.mailRotateKeysCancelButton)
                    .automationActivate(Ids.mailRotateKeysCancelButton) { onClose() }
            }
        }
    }

    /// Run the rotation. Shared by the confirm `Button` and its
    /// `.automationActivate` (multi-statement `Task`, so extracted per convention).
    private func confirmRotate() {
        Task {
            rotating = true
            await vm.dispatch(.startRotation(excludedCredentials: Array(excluded)))
            rotating = false
            if vm.errorMessage == nil { onClose() }
        }
    }
}
