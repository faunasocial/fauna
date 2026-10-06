import SwiftUI

/// The admin **`admin-aliases`** page (`docs/goal/behavior/admin.md` § 4 Aliases),
/// shared by macOS + iOS (one FaunaKit view, thin per-target mount points).
/// A dumb renderer of `ForwardersSnapshot` + dispatcher of
/// `ForwarderAction` over the shared `ForwarderMachine` (via `ForwardersVM`); no
/// business logic here. Element IDs match `tests/e2e-unified/ui.yaml`
/// `admin-aliases` / `admin-aliases-forwarder-list` exactly. Reference renderers:
/// linux (`apps/fauna-linux/src/views/admin.rs::build_admin_aliases_page`),
/// windows (`AdminAliasesViewModel`), web (`routes/admin/aliases/+page.svelte`).
///
/// This page owns ONLY the external-forwarder (Kind 7) slice of the admin-tier
/// alias surface: an indexed list of `<pattern>@<local_domain> → external` rows
/// (each with a delete button) above an add form (hosted-domain picker +
/// local-part + external target). The user-tier alias kinds live on the user
/// `mail-aliases` page; the per-domain catch-all (Kind 4) lives on `admin-dns`.
///
/// `admin-nav-back` is provided by the admin shell rail (macOS), not this page.
public struct AdminAliasesView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = ForwardersVM()
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    @State private var addDomain = ""
    @State private var addPattern = ""
    @State private var addTarget = ""

    public init(reloadToken: Int = 0) {
        self.reloadToken = reloadToken
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.admin.aliasesPage.title)
                    .font(.title)
                    .accessibilityIdentifier(Ids.adminAliasesHeading)
                    .automationValue(Ids.adminAliasesHeading,
                                     text: { L.admin.aliasesPage.title })

                forwardersSection

                if let actionError = vm.actionError {
                    Text(actionError)
                        .foregroundStyle(.red)
                        .font(.caption)
                        .accessibilityIdentifier(Ids.adminAliasesActionError)
                        .automationValue(Ids.adminAliasesActionError,
                                         text: { actionError })
                }
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(api: client.api)
            if addDomain.isEmpty || !vm.localDomains.contains(addDomain) {
                addDomain = vm.localDomains.first ?? ""
            }
        }
    }

    // MARK: - Forwarders section (add form + indexed list)

    private var forwardersSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L.admin.aliasesPage.forwardersTitle)
                .font(.headline)
            Text(L.admin.aliasesPage.forwardersDesc)
                .font(.caption)
                .foregroundStyle(.secondary)

            addForm
            forwarderList
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminAliasesForwardersSection)
        // A container's `.accessibilityIdentifier` + `.contain` registers it in
        // the AX tree but NOT in `AutomationRegistry`, which is what
        // `is_visible`/`wait_for` actually read — so the section needs its own
        // reader to be present at all for the in-process driver.
        .automationValue(Ids.adminAliasesForwardersSection, text: { "" })
    }

    private var addForm: some View {
        VStack(alignment: .leading, spacing: 8) {
            Picker(L.admin.aliasesPage.forwarderDomain, selection: $addDomain) {
                ForEach(vm.localDomains, id: \.self) { domain in
                    Text(domain).tag(domain)
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier(Ids.adminAliasesForwarderAddDomainSelect)
            // The in-process driver's `/element/select` resolves an entry's
            // `setValue`, which only `automationSelect` registers — an
            // `accessibilityIdentifier` alone leaves the picker unresolvable and
            // the route 404s "element not found" (it is not an AX-tree read).
            // Same seam the sibling admin pickers already carry
            // (`AdminDnsView`'s four call sites); `options:` supplies
            // convention 11's twin rule, so a domain the frame has not painted
            // yet is refused with a 409 the action layer retries against while
            // the page's own `list_local_domains` fetch is still in flight,
            // instead of being written through unchecked.
            .automationSelect(
                Ids.adminAliasesForwarderAddDomainSelect,
                value: { addDomain },
                options: { vm.localDomains },
                isEnabled: { !vm.isBusy }
            ) { addDomain = $0 }

            TextField(L.admin.aliasesPage.forwarderLocalPartPlaceholder, text: $addPattern)
                .autocorrectionDisabled()
                .accessibilityIdentifier(Ids.adminAliasesForwarderAddPatternInput)
                .automationField(Ids.adminAliasesForwarderAddPatternInput,
                                 text: $addPattern, isEnabled: { !vm.isBusy })

            TextField(L.admin.aliasesPage.forwarderTargetPlaceholder, text: $addTarget)
                .autocorrectionDisabled()
                .accessibilityIdentifier(Ids.adminAliasesForwarderAddTargetInput)
                .automationField(Ids.adminAliasesForwarderAddTargetInput,
                                 text: $addTarget, isEnabled: { !vm.isBusy })

            Button(L.admin.aliasesPage.createForwarder) {
                submit()
            }
            .disabled(vm.isBusy || addDomain.isEmpty
                      || addPattern.trimmed.isEmpty || addTarget.trimmed.isEmpty)
            .accessibilityIdentifier(Ids.adminAliasesForwarderAddSubmitButton)
            .automationActivate(
                Ids.adminAliasesForwarderAddSubmitButton,
                isEnabled: {
                    !(vm.isBusy || addDomain.isEmpty
                      || addPattern.trimmed.isEmpty || addTarget.trimmed.isEmpty)
                }
            ) { submit() }
            .faunaGate("fauna.bridges.create_forwarder")
        }
    }

    @ViewBuilder
    private var forwarderList: some View {
        if vm.forwarders.isEmpty {
            Text(L.admin.aliasesPage.noForwarders)
                .font(.callout)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.adminAliasesForwarderList)
                .automationValue(Ids.adminAliasesForwarderList,
                                 text: { L.admin.aliasesPage.noForwarders })
        } else {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(vm.forwarders, id: \.aliasIdHex) { view in
                    forwarderRow(view)
                }
            }
            .accessibilityIdentifier(Ids.adminAliasesForwarderList)
            .automationValue(Ids.adminAliasesForwarderList, text: { "" })
        }
    }

    @ViewBuilder
    private func forwarderRow(_ view: ForwarderView) -> some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                // `address` is pre-composed `<pattern>@<localDomain>` by the machine.
                Text(view.address)
                    .font(.body)
                    .textSelection(.enabled)
                    .accessibilityIdentifier(Ids.adminAliasesForwarderRowAddress)
                    .automationValue(Ids.adminAliasesForwarderRowAddress,
                                     text: { view.address })
                Text(view.forwardTarget)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .accessibilityIdentifier(Ids.adminAliasesForwarderRowTarget)
                    .automationValue(Ids.adminAliasesForwarderRowTarget,
                                     text: { view.forwardTarget })
            }
            Spacer()
            Button(L.admin.aliasesPage.deleteForwarder, role: .destructive) {
                Task { await vm.delete(aliasIdHex: view.aliasIdHex) }
            }
            .controlSize(.small)
            .disabled(vm.isBusy)
            .accessibilityIdentifier(Ids.adminAliasesForwarderRowDeleteButton)
            .automationActivate(Ids.adminAliasesForwarderRowDeleteButton,
                                isEnabled: { !vm.isBusy }) {
                Task { await vm.delete(aliasIdHex: view.aliasIdHex) }
            }
            .faunaGate("fauna.bridges.delete_forwarder")
        }
        .padding(.vertical, 4)
    }

    // MARK: - Actions

    private func submit() {
        let pattern = addPattern.trimmed
        let target = addTarget.trimmed
        guard !addDomain.isEmpty, !pattern.isEmpty, !target.isEmpty else { return }
        let domain = addDomain
        Task {
            await vm.create(localDomain: domain, pattern: pattern, forwardTarget: target)
            // Clear the local-part + target on success (matches linux/windows);
            // keep the domain selection for adding several to the same domain.
            if vm.actionError == nil {
                addPattern = ""
                addTarget = ""
            }
        }
    }
}

private extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
