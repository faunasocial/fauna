import SwiftUI

/// The multi-account switcher — the **first** section on the Account settings page,
/// shared by macOS + iOS (`long-term-store.md` § Multi-account evolution; ui.yaml
/// `account-switcher-list` and friends). Mirrors the linux reference
/// (`apps/fauna-linux/src/settings/account.rs`), which is also first on its Account page.
///
/// Row rules, all three taken from linux rather than re-derived:
/// - Rows come from `registry.list()` in **add order**.
/// - The **active** row — the account this window *serves* (`servedActorId`), never the
///   registry's active pointer, which a bound macOS secondary does not follow — shows the
///   indicator, is **not** tappable, and has **no** remove button (removing the account
///   you are authenticated as would unlink the stores this window runs from).
/// - A **non-active** row is tappable (→ switch) and carries a remove button, with **no**
///   confirmation dialog.
///
/// The section owns the *read* and the *remove* (both pure registry operations, so they
/// live in `AccountSwitcherVM`). It does **not** own the switch or the append wizard: both
/// tear down / stand up the authenticated session, which is app-owned state a FaunaKit view
/// cannot reach — so they leave through `onSwitch` / `onAddAccount`, the same closure seam
/// `onAccountReset` and `onFactoryReset` already use.
///
/// **A11y containers:** the ids sit on plain `VStack`/`HStack` containers with
/// `.accessibilityElement(children: .contain)`, never bare on the outer `GroupBox` — a bare
/// container identifier clobbers every child id, so the sentinel could not live there and
/// keep `account-item-handle` / `account-remove-button` queryable.
///
/// **`GroupBox`, not `Section`** (moved 2026-08-25): the parent `AccountSettingsView`
/// renders in an eager `ScrollView { VStack }`, not a `Form`, so a bare `Section` here would
/// no longer sit in a container that gives it any chrome — `GroupBox` is the established
/// rule-6 replacement (`apple-e2e-automation.md` § Registration rules rule 6; mirrors
/// `FamilyView`'s `GroupBox` sections).
public struct AccountSwitcherSection: View {
    @State private var vm: AccountSwitcherVM

    /// `(actorId, confirmed)` — `confirmed` is true iff the Stage-2 re-auth prompt
    /// just succeeded for this activation; the app picks `setActiveConfirmed` vs.
    /// plain `setActive` on it (`long-term-store.md` § Multi-account evolution).
    /// Throws the registry's refusal — thrown before any teardown — which this section
    /// paints on its `error-message` (linux's `paint_switch_refusal`).
    private let onSwitch: @MainActor (String, Bool) async throws -> Void
    private let onAddAccount: () -> Void
    /// Spawn a NEW app instance bound to the given account — the running
    /// instance's concurrent-instances affordance (`account-scoping.md`
    /// § Concurrent instances; ui.yaml `account-open-new-instance-button`).
    /// `nil` hides the button: iOS passes nil (the OS admits one instance per
    /// app); macOS passes its spawn glue. Renders on EVERY row, including the
    /// active one — macOS retired its same-account refusal (W5.6 (account-data-plane.md § Workstreams),
    /// 2026-08-16), so a spawn for the served account is now an ordinary
    /// bound launch rather than one the per-account instance lock refuses.
    private let onOpenNewInstance: ((String) -> Void)?

    /// `servedActorId` reads the account this window's session runs as (the app's
    /// `SessionState.actorId`) — the key the active row, and the remove refusal, sit on.
    /// `ownLock` is threaded straight through to `AccountSwitcherVM`'s `ownLock` (see its
    /// doc) — the same raw instance lock `AccountSettingsView` hands `SignOutSection`.
    public init(
        servedActorId: @escaping @MainActor () -> String?,
        ownLock: FfiAccountInstanceLock? = nil,
        onSwitch: @escaping @MainActor (String, Bool) async throws -> Void,
        onAddAccount: @escaping () -> Void,
        onOpenNewInstance: ((String) -> Void)? = nil
    ) {
        _vm = State(initialValue: AccountSwitcherVM(servedActorId: servedActorId, ownLock: ownLock))
        self.onSwitch = onSwitch
        self.onAddAccount = onAddAccount
        self.onOpenNewInstance = onOpenNewInstance
    }

    public var body: some View {
        GroupBox(L.settings.accountPage.accounts) {
            VStack(alignment: .leading, spacing: 8) {
                VStack(spacing: 0) {
                    ForEach(Array(vm.accounts.enumerated()), id: \.element.actorId) { offset, entry in
                        row(entry, index: offset)
                        if offset < vm.accounts.count - 1 { Divider() }
                    }
                }
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.accountSwitcherList)
                .automationValue(Ids.accountSwitcherList, text: { "\(vm.accounts.count)" })

                Button(L.settings.accountPage.addAccount, systemImage: "plus") {
                    onAddAccount()
                }
                .accessibilityIdentifier(Ids.accountAddButton)
                .automationActivate(Ids.accountAddButton) { onAddAccount() }

                if let error = vm.error {
                    ErrorBanner(message: error)
                }

                Text(L.settings.accountPage.accountsSubtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task { vm.reload() }
        // Re-read when the registry is written from OUTSIDE this view model — the
        // admin auto-default fires at an `am-i-admin` observation, which can land
        // while this section is already on screen. Without it the row keeps the
        // pre-write value and the toggle both displays the wrong state and swallows
        // the next tap (it derives `!entry.requireConfirmToActivate` from the stale
        // entry). The on-appear `.task` above only covers a switcher opened AFTER
        // the write.
        .onAccountRegistryChanged { vm.reload() }
    }

    @ViewBuilder
    private func row(_ entry: FfiAccountEntry, index: Int) -> some View {
        let isActive = vm.isActive(entry)
        let title = vm.label(for: entry)

        HStack {
            automationText(Ids.accountItemHandle, title)
                .lineLimit(1)
                .truncationMode(.middle)

            Spacer()

            // Stage-2 "require re-auth to activate" flag — on EVERY row (the natural
            // target is the user's admin identity, which is often the active row).
            // Setting the flag never prompts; only activating a flagged account does.
            Toggle(L.settings.accountPage.requireConfirmToggle, isOn: Binding(
                get: { entry.requireConfirmToActivate },
                set: { vm.setRequireConfirm(actorId: entry.actorId, require: $0) }
            ))
            .labelsHidden()
            .help(L.settings.accountPage.requireConfirmToggle)
            .accessibilityLabel(L.settings.accountPage.requireConfirmToggle)
            .accessibilityIdentifier(Ids.accountRequireConfirmToggle)
            .automationActivate(
                Ids.accountRequireConfirmToggle,
                value: { entry.requireConfirmToActivate ? "on" : "off" }
            ) {
                vm.setRequireConfirm(actorId: entry.actorId,
                                     require: !entry.requireConfirmToActivate)
            }

            if isActive {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .help(L.common.active)
                    .accessibilityLabel(L.common.active)
                    .accessibilityIdentifier(Ids.accountItemActiveIndicator)
                    .automationValue(Ids.accountItemActiveIndicator, text: { L.common.active })
            } else {
                Button {
                    vm.remove(actorId: entry.actorId)
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .help(L.common.remove)
                .accessibilityLabel(L.common.remove)
                .accessibilityIdentifier(Ids.accountRemoveButton)
                .automationActivate(Ids.accountRemoveButton) { vm.remove(actorId: entry.actorId) }
            }

            // Concurrent instances: open this account in a NEW app instance,
            // leaving this window on its own account. Only where the app
            // provides the spawn glue (macOS; iOS passes nil). Every row,
            // including the active one — the account this instance already
            // serves is retired onto a SHARED lock (W5.6, 2026-08-16), so a
            // spawn for it is an ordinary bound launch, not a refused one
            // (`account-scoping.md` § Concurrent instances).
            if let onOpenNewInstance {
                Button {
                    onOpenNewInstance(entry.actorId)
                } label: {
                    Image(systemName: "macwindow.badge.plus")
                }
                .buttonStyle(.borderless)
                .help(L.settings.accountPage.openNewInstance)
                .accessibilityLabel(L.settings.accountPage.openNewInstance)
                .accessibilityIdentifier(Ids.accountOpenNewInstanceButton)
                .automationActivate(Ids.accountOpenNewInstanceButton) {
                    onOpenNewInstance(entry.actorId)
                }
            }
        }
        .padding(.vertical, 6)
        // Make the whole row the hit target, not just the label.
        .contentShape(Rectangle())
        .onTapGesture { activate(entry, isActive: isActive) }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.accountSwitcherItem)
        .automationActivate(Ids.accountSwitcherItem,
                            isEnabled: { !isActive },
                            value: { title }) {
            activate(entry, isActive: isActive)
        }
        // OUTERMOST, so the row's own registration (above) sits INSIDE the scope and
        // records `(account-switcher-item, index)` as its path — its DECLARED position.
        // That is what lets a scoped query (`account-switcher-item[1]`) address the
        // intended row, and it is load-bearing here: a flat `index=` resolves by the
        // registry's *registration* order (the order rows fired `.onAppear`), which is
        // not document order for a list that fills from an async `.task` — these rows
        // register bottom-up, so a flat `index: 1` lands on the ACTIVE row and the switch
        // silently no-ops. The e2e drives these rows with `scope=`, never a bare index.
        // Same idiom as `muted-word-item` / `task-delegation-kind-item`.
        .automationScope(Ids.accountSwitcherItem, index: index)
    }

    /// Tapping the active row is a no-op — never a self-switch (which would tear the
    /// session down and rebuild it as the identity already running). A flagged row
    /// goes through the VM's Stage-2 gate (native re-auth prompt) before the app's
    /// switch seam is invoked; a declined prompt never reaches `onSwitch`. The switch
    /// runs in its own `Task`, enqueued synchronously so the VM's gesture count still
    /// lands after it (see `requestSwitch`); a refusal comes back to `vm.error`.
    private func activate(_ entry: FfiAccountEntry, isActive: Bool) {
        guard !isActive else { return }
        vm.requestSwitch(to: entry) { [vm] actorId, confirmed in
            Task { @MainActor in
                do {
                    try await onSwitch(actorId, confirmed)
                } catch {
                    vm.reportSwitchRefused(error)
                }
            }
        }
    }
}
