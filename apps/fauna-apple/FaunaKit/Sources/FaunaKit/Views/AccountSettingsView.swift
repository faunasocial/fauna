import SwiftUI

/// The user-facing **Account** settings sub-page (`docs/goal/ui/settings.md` § Account),
/// shared by macOS + iOS — one FaunaKit view, mounted from each
/// platform's settings shell. Renders identity export, handle change, connected services
/// (Bluesky), data export, sign-out, and account deletion over the shared
/// `AccountSettingsVM`.
///
/// **Seam:** unlike the Encryption/Privacy lifts, Account needs the shared `SessionState`
/// (a FaunaKit type, so passable across the `MacAppState`/`AppState` divide) — its
/// `changeHandle`/`IdentityExportSection`/`SignOutSection` operate on it. Account deletion
/// does NOT: `fauna.account.delete` only schedules a 14-day cancellable pending action
/// (`settings.md` § User actions, ruled 2026-08-26), so the app stays signed in on this
/// page and `onAccountReset` fires only from `SignOutSection` — the app-state side-effect
/// (reset to onboarding on sign-out) each platform passes (`MacAppState.isOnboarded = false`
/// / `AppState.isOnboarding = true`). The `APIClient` comes from the shared `FaunaClient`
/// environment.
///
/// **`ScrollView { VStack { GroupBox } }`, not `Form`** (moved 2026-08-25): on iOS a `Form`
/// is a lazy `List` whose off-screen sections never fire the `.onAppear` the in-process
/// `AutomationRegistry` rides, and this page is taller than one iOS screen — `sign-out-button`
/// registered `count=0` for exactly this reason once it moved onto this page (below the fold).
/// macOS has no such laziness and renders the eager shape identically (`apple-e2e-automation.md`
/// § Registration rules rule 6; mirrors `FamilyView`/`AdminView`).
///
/// **Documented platform divergences** (genuine, not drift): the **Identity** section
/// (actor-id + node) renders only on iOS — on macOS identity lives on the Status sub-page
/// (`MacStatusView`, settings.md:20), and ui.yaml lists `account-actor-id` on the account
/// page which iOS satisfies here. **Data export** uses `NSSavePanel` on macOS and
/// `UIActivityViewController` on iOS (platform-divergent presentation over the shared
/// `vm.exportData()`). **Sign-out** renders here on both targets — it used to also live on
/// iOS's settings root list, a duplicate that briefly worked as the ONLY iOS mount by
/// accident (`applyNavPatch` once read only `stack.first`, so `_navigate_subpage("account")`
/// never actually pushed here); fixed to this uniform placement once the nav fix
/// (`stack[1].id`) made the accident visible.
public struct AccountSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AccountSettingsVM()
    /// The account-deletion type-to-confirm buffer (`settings-delete-confirm-field`).
    @State private var deleteConfirmText = ""

    private let session: SessionState
    /// This window's raw per-account instance lock — threaded straight through to
    /// `SignOutSection`'s `ownLock` (see its doc). `nil` on iOS, which passes nothing;
    /// macOS's shell hands in `MacAppState.instanceLock`.
    private let ownLock: FfiAccountInstanceLock?
    /// Reset to onboarding after sign-out (account deletion does NOT call this
    /// — see the type doc comment above). macOS sets `isOnboarded = false`;
    /// iOS sets `isOnboarding = true`.
    private let onAccountReset: () -> Void
    /// Activate another identity held on this install. The app root tears the
    /// authenticated session down and rebuilds it (`long-term-store.md` § Multi-account
    /// evolution — switch-first); this view only names the target. The Bool is the
    /// Stage-2 `confirmed` bit (true iff the re-auth prompt just succeeded — the app
    /// calls `setActiveConfirmed` on it, plain `setActive` otherwise). Throws the
    /// registry's refusal, which the switcher paints on the page's `error-message`.
    private let onSwitchAccount: @MainActor (String, Bool) async throws -> Void
    /// Launch onboarding in **append** mode to add another identity.
    private let onAddAccount: () -> Void

    public init(
        session: SessionState,
        ownLock: FfiAccountInstanceLock? = nil,
        onAccountReset: @escaping () -> Void,
        onSwitchAccount: @escaping @MainActor (String, Bool) async throws -> Void,
        onAddAccount: @escaping () -> Void
    ) {
        self.session = session
        self.ownLock = ownLock
        self.onAccountReset = onAccountReset
        self.onSwitchAccount = onSwitchAccount
        self.onAddAccount = onAddAccount
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                // First on the page — the linux reference puts the switcher group first too,
                // and "which identity am I?" is the question that frames everything below it.
                #if os(macOS)
                // macOS additionally offers "open in new window" on non-active rows
                // (concurrent instances — the running instance's spawn affordance;
                // iOS passes no glue and renders no button: one instance per app).
                AccountSwitcherSection(
                    servedActorId: { session.actorId },
                    ownLock: ownLock,
                    onSwitch: onSwitchAccount,
                    onAddAccount: onAddAccount,
                    onOpenNewInstance: { InstanceSpawner.openNewInstance(actorIdHex: $0) }
                )
                #else
                AccountSwitcherSection(
                    servedActorId: { session.actorId },
                    ownLock: ownLock,
                    onSwitch: onSwitchAccount, onAddAccount: onAddAccount)
                #endif

                #if os(iOS)
                // iOS-only: identity lives here (on macOS it's on the Status sub-page).
                GroupBox(L.settings.accountPage.identity) {
                    VStack(alignment: .leading, spacing: 8) {
                        if let actorId = session.actorId {
                            LabeledContent(L.settings.accountPage.actorId) {
                                Text(actorId)
                                    .textSelection(.enabled)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                    .font(.caption.monospaced())
                                    .accessibilityIdentifier(Ids.accountActorId)
                                    // Optional + selectable/monospaced label: keep the Text +
                                    // a11y id, add a live read for the in-process driver.
                                    .automationValue(Ids.accountActorId, text: { actorId })
                            }
                        }
                        if let nodeUrl = session.nodeUrl {
                            LabeledContent(L.settings.accountPage.node) {
                                Text(nodeUrl).textSelection(.enabled)
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                #endif

                GroupBox(L.settings.identityExport.title) {
                    IdentityExportSection(secretHex: session.secretHex, handle: session.handle)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                // Immediately after Identity export, which `settings.md` § Recovery
                // kit ratifies and which is also the reason: both reveal a root
                // secret once, as 64-hex + QR, with a warning beside it, and neither
                // persists anything. A user who just read "whoever scans this QR
                // gains the identity" is in exactly the frame of mind this needs.
                GroupBox(L.settings.recoveryKit.title) {
                    // A landed succession revokes this session's bearers inside the
                    // nest's own transaction, so the app has to come back up — but as
                    // the SUCCESSOR, not at onboarding. The successor's seed is
                    // already a registry row by now (the ceremony persists it and
                    // verifies the persist by read-back), so this is an ordinary
                    // account switch to it, exactly as tui's `adopt_successor` ends
                    // in `switch_account`. ⚠ `onAccountReset` would strand the user
                    // in the wizard holding an account they own.
                    //
                    // `confirmed: false` — the Stage-2 bit asserts a re-auth prompt
                    // just succeeded, and no prompt ran here: the kit was the
                    // authorization (same bit tui passes).
                    RecoveryKitSection(
                        sessionActorIdHex: session.actorId,
                        onSucceeded: { successorActorIdHex in
                            // The switch logs its own refusal; the kit
                            // section has nothing further to paint.
                            Task { try? await onSwitchAccount(successorActorIdHex, false) }
                        })
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                // Apple-only: opt the identity secret into iCloud Keychain sync. Default OFF
                // (device-bound); apps/ios.md § Credential Storage.
                GroupBox(L.settings.icloudBackup.title) {
                    VStack(alignment: .leading, spacing: 8) {
                        Toggle(L.settings.icloudBackup.toggle, isOn: Binding(
                            get: { vm.iCloudBackupEnabled },
                            set: { vm.setICloudBackup($0) }
                        ))
                        .accessibilityIdentifier(Ids.settingsIcloudBackupToggle)
                        .automationActivate(
                            Ids.settingsIcloudBackupToggle,
                            value: { vm.iCloudBackupEnabled ? "on" : "off" }
                        ) { vm.setICloudBackup(!vm.iCloudBackupEnabled) }
                        Text(L.settings.icloudBackup.footer)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                GroupBox(L.settings.accountPage.changeHandle) {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            TextField(L.settings.accountPage.newHandle, text: $vm.newHandle)
                                .textFieldStyle(.roundedBorder)
                                #if os(iOS)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                #endif
                                .accessibilityIdentifier(Ids.newHandle)
                                .automationField(Ids.newHandle, text: $vm.newHandle)
                            Button(L.common.change) {
                                Task { await vm.changeHandle(session: session) }
                            }
                            .disabled(vm.newHandle.trimmingCharacters(in: .whitespaces).isEmpty || vm.changingHandle)
                            .accessibilityIdentifier(Ids.changeHandle)
                            .automationActivate(Ids.changeHandle,
                                                isEnabled: { !vm.newHandle.trimmingCharacters(in: .whitespaces).isEmpty && !vm.changingHandle }) {
                                Task { await vm.changeHandle(session: session) }
                            }
                            // `api.changeHandle` → `fauna.profile.handle.change`. The
                            // new-handle field beside it is buffer and stays typeable,
                            // and the shared `validateHandle` still runs locally — so
                            // with no nest the user can still see a bad handle refused.
                            .faunaGate("fauna.profile.handle.change")
                        }
                        if let error = vm.changeHandleError {
                            ErrorBanner(message: error)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                GroupBox(L.settings.accountPage.connectedServices) {
                    blueskyContent
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                GroupBox(L.settings.accountPage.dataExport) {
                    VStack(alignment: .leading, spacing: 8) {
                        Button(L.settings.accountPage.exportMyData) {
                            Task { await exportAndPresent() }
                        }
                        .disabled(vm.exporting)
                        .accessibilityIdentifier(Ids.settingsExportDataButton)
                        .automationActivate(Ids.settingsExportDataButton,
                                            isEnabled: { !vm.exporting }) {
                            Task { await exportAndPresent() }
                        }
                        if let error = vm.exportError {
                            ErrorBanner(message: error)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                SignOutSection(session: session, ownLock: ownLock, onSignedOut: onAccountReset)

                GroupBox(L.common.dangerZone) {
                    // ── Type-to-confirm, the DECLARED cross-app idiom (ui.yaml
                    // optional_elements → `settings-delete-confirm-field`;
                    // `settings.md` § User actions). It replaces the system `.alert`
                    // this page used to raise, for two reasons that are the same
                    // reason:
                    //
                    //  1. An `.alert`'s content is a separate presentation context.
                    //     The confirm carried no automation id a driver could reach,
                    //     so the most destructive gesture in the app had no e2e; and
                    //     `.faunaGate` could not read a `FaunaClient` from an
                    //     environment that does not reliably cross that boundary, so
                    //     the control could not be gated either. Inline, the commit is
                    //     an ordinary view: one id, one gate, one existing test.
                    //  2. It resolves a RECORDED divergence rather than adding one —
                    //     web/windows/tui already ship exactly this, and apple's
                    //     native-alert shape was named in `settings.md` as the
                    //     odd-one-out. No new element id is needed (priority #4:
                    //     resolve drift, and take the richest existing pattern).
                    //
                    // `tests/e2e-unified/tests/test_delete_account.py` needed no
                    // apple-specific change to cover this; the action layer is
                    // id-driven. Shape reference: `apps/fauna-web/src/routes/
                    // settings/[[subpage]]/+page.svelte`'s danger-zone section.
                    VStack(alignment: .leading, spacing: 8) {
                        Text(L.settings.accountPage.deleteConfirmText)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        TextField(L.settings.deleteConfirmPlaceholder, text: $deleteConfirmText)
                            .textFieldStyle(.roundedBorder)
                            #if os(iOS)
                            // The gate compares against a literal, so an autocapitalized
                            // "Delete" would never arm the button and the user would have
                            // no way to see why.
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            #endif
                            .accessibilityIdentifier(Ids.settingsDeleteConfirmField)
                            .automationField(Ids.settingsDeleteConfirmField, text: $deleteConfirmText)
                        Button(L.settings.accountPage.deleteAccount, role: .destructive) {
                            confirmDeleteAccount()
                        }
                        .disabled(!deleteArmed)
                        .accessibilityIdentifier(Ids.settingsDeleteAccountButton)
                        .automationActivate(Ids.settingsDeleteAccountButton,
                                            isEnabled: { deleteArmed }) { confirmDeleteAccount() }
                        // Rule 1 — the COMMIT gates, not the buffer. This button IS the
                        // commit now (there is no opener left to arm), so it declares;
                        // the field beside it is buffer and stays typeable with no nest,
                        // exactly as the new-handle field above does.
                        .faunaGate("fauna.account.delete")
                        if let error = vm.deleteAccountError {
                            ErrorBanner(message: error)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }

                // Pending actions (settings.md § Pending actions) — STANDING,
                // sitting below the two delayed verbs this page hosts (the
                // third, snapshot delete, schedules from the Backups page and
                // appears here on the next Account visit's hydrate). Always
                // rendered — a conditional render would hide the affordance
                // exactly when a mis-clicker goes looking for it. Mirrors
                // tui's/linux's/web's/android's own section exactly.
                PendingActionsSection(
                    pendingActions: vm.pendingActions,
                    onCancel: { id in Task { await vm.cancelPendingAction(id: id) } }
                )
                if let error = vm.pendingActionsError {
                    ErrorBanner(message: error)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        #if os(iOS)
        .pageTitle(L.settings.accountPage.title)
        #endif
        .task {
            if let client { vm.configure(api: client.api) }
            vm.loadICloudBackupState()
            await vm.checkBlueskyStatus()
            await vm.loadPendingActions()
        }
    }

    /// Whether the delete button is armed: the confirm field reads the exact
    /// literal, and no deletion is already in flight.
    ///
    /// `"DELETE"` is deliberately **not** localized and deliberately not lifted
    /// into shared Rust — every app spells the same literal at its own call site
    /// (tui `settings/account.rs:283`, windows `SettingsAccountPage.xaml.cs:257`,
    /// web's `deleteConfirmText !== 'DELETE'`), and `apps/fauna-tui/src/settings/
    /// recovery.rs` records the ruling: the *prompt* is translated, the word the
    /// user types is not, or the gate would differ per locale.
    private var deleteArmed: Bool {
        deleteConfirmText == "DELETE" && !vm.deletingAccount
    }

    /// Commit the deletion. `fauna.account.delete` only SCHEDULES a 14-day
    /// cancellable pending action (`settings.md` § User actions, ruled
    /// 2026-08-26) — the app stays signed in on this page, no sign-out, no
    /// credential/store erase, no navigation; the pending-actions row (once
    /// apple builds one) is the receipt and its
    /// cancel button the way back. Clearing the confirm field on success
    /// disarms the button so a lingering "DELETE" can't re-submit.
    private func confirmDeleteAccount() {
        Task {
            if await vm.deleteAccount() {
                deleteConfirmText = ""
            }
        }
    }

    /// Connected-services (Bluesky) content — shown on both platforms (iOS gains it in
    /// this lift; richest pattern, priority #4). `vm.checkBlueskyStatus()` runs on both.
    @ViewBuilder private var blueskyContent: some View {
        if vm.bskyAvailable {
            if vm.bskyLinked {
                HStack {
                    Text(L.settings.accountPage.bluesky).fontWeight(.semibold)
                    Text("@\(vm.bskyHandle)").font(.caption.monospaced())
                    Spacer()
                    Button(L.settings.accountPage.unlink) {
                        Task { await vm.unlinkBluesky() }
                    }
                    .disabled(vm.bskyLoading)
                    .foregroundStyle(.red)
                    .faunaGate("fauna.bridges.unlink")
                }
            } else {
                HStack {
                    Text(L.settings.accountPage.bluesky).fontWeight(.semibold)
                    Text(L.common.notLinked).foregroundStyle(.secondary)
                }
                HStack {
                    TextField(L.settings.accountPage.blueskyHandlePlaceholder, text: $vm.bskyInputHandle)
                        .textFieldStyle(.roundedBorder)
                    Button(L.settings.accountPage.link) {
                        Task { await vm.linkBluesky() }
                    }
                    .disabled(vm.bskyInputHandle.isEmpty || vm.bskyLoading)
                    .buttonStyle(.borderedProminent)
                    // `linkBluesky` issues `fauna.bridges.link` (mode "oauth")
                    // before any browser hop, so the nest is needed first.
                    .faunaGate("fauna.bridges.link")
                }
            }
            if let error = vm.bskyError {
                ErrorBanner(message: error)
            }
        } else {
            Text(L.errors.blueskyBridgeNotAvailable)
                .foregroundStyle(.secondary)
        }
    }

    /// Export the user's data, then present a platform-native save/share affordance
    /// over the shared `vm.exportData()` bytes.
    ///
    /// Under e2e, bypasses the dialog entirely and writes straight into
    /// `SnapshotFileSaver.e2eDownloadDir` as the fixed name
    /// `tests/e2e-unified/actions/settings.py::export_my_data` reads back —
    /// mirrors `MacSnapshotFileListView`/`SnapshotFileListView`'s
    /// `downloadFile`, and linux's `account.rs` `export_btn` handler (same
    /// `FAUNA_E2E_DOWNLOAD_DIR` seam, same filename).
    private func exportAndPresent() async {
        guard let data = await vm.exportData() else { return }
        if SnapshotFileSaver.e2eDownloadDir != nil {
            SnapshotFileSaver.saveForE2E(suggestedFileName: "fauna-export.zip", data: data)
            return
        }
        let name = "fauna-export-\(ISO8601DateFormatter().string(from: Date()).prefix(10)).zip"
        #if os(macOS)
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        guard panel.runModal() == .OK, let url = panel.url else { return }
        try? data.write(to: url)
        #elseif os(iOS)
        let tempUrl = FileManager.default.temporaryDirectory.appendingPathComponent(name)
        try? data.write(to: tempUrl)
        ShareSheet.present(items: [tempUrl])
        #endif
    }
}
