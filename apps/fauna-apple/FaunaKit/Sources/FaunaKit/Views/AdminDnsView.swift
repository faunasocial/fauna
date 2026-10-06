import SwiftUI

/// The admin **`admin-dns`** page (`docs/goal/behavior/dns-management.md`
/// § App surface + `docs/goal/architecture/nest/tls-certificates.md` § C),
/// shared by macOS + iOS (one FaunaKit view, thin per-target mount points).
/// A dumb renderer of two shared-Rust snapshots merged by
/// domain name (`DnsSnapshot` + `LocalDomainsSnapshot`, via `AdminDnsVM`) and a
/// dispatcher of `DnsAction` / `LocalDomainAction`; no DNS logic here. Element IDs
/// match `tests/e2e-unified/ui.yaml` `admin-dns` / `admin-dns-domain` /
/// `admin-dns-record` / `admin-dns-credential-item` / `admin-dns-removed-domain`
/// exactly. Reference renderer: linux
/// (`apps/fauna-linux/src/views/admin.rs::build_dns_page`).
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation
/// stack (iOS), not this page.
public struct AdminDnsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminDnsVM()
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    @State private var showAddDomain = false
    @State private var addDomainText = ""
    /// The domain whose inline delegate zone-picker is currently revealed.
    @State private var delegatingDomain: String?
    @State private var delegateZone = ""
    // Add-credential reveal form (managed-mode DNS-provider credential).
    @State private var showAddCredential = false
    @State private var selectedProvider: String?
    @State private var credValues: [String: String] = [:]

    // Primary-domain rename (mail-primary-domain-rename.md § UX surface).
    // Inline @State-driven reveals — NOT a real `.sheet`/`.confirmationDialog`:
    // system presentation layers don't reliably `.onAppear`-register in-process
    // (`apple-e2e-automation.md` § limitation 3) — matching this file's own
    // `addDomainSection`/`addCredentialForm` idiom.
    @State private var renameSheetOpen = false
    @State private var renameTarget = ""       // the picked new-primary domain name
    @State private var renameGraceDays = ""    // '' → nest default (7)
    @State private var confirmingComplete = false
    @State private var confirmingAbort = false
    @State private var extendDays = "7"

    public init(reloadToken: Int = 0) { self.reloadToken = reloadToken }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.admin.dns.title)
                    .font(.title)
                    .accessibilityIdentifier(Ids.pageHeading)
                Text(L.admin.dns.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                HStack {
                    manageAllToggle
                    Spacer()
                    Button {
                        Task { await vm.refresh() }
                    } label: {
                        Label(L.admin.dns.refresh, systemImage: "arrow.clockwise")
                    }
                    .disabled(vm.isBusy)
                    .accessibilityIdentifier(Ids.adminDnsRefreshButton)
                    .automationActivate(Ids.adminDnsRefreshButton, isEnabled: { !vm.isBusy }) {
                        Task { await vm.refresh() }
                    }
                }

                addDomainSection

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                renameBanner
                renameSheetSection

                domainList
                removedDomainsSection
                credentialsSection
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
    }

    // MARK: - Manage-all + add-domain

    private var manageAllToggle: some View {
        Toggle(isOn: Binding(
            get: { vm.allManaged },
            set: { v in Task { await vm.setAllManaged(v) } }
        )) {
            Text(L.admin.dns.manageAll)
        }
        .toggleStyle(.switch)
        .disabled(vm.isBusy || vm.activeDomainNames.isEmpty)
        .accessibilityIdentifier(Ids.adminDnsManageAllToggle)
        .automationActivate(
            Ids.adminDnsManageAllToggle,
            isEnabled: { !vm.isBusy && !vm.activeDomainNames.isEmpty },
            value: { vm.allManaged ? "on" : "off" }
        ) {
            Task { await vm.setAllManaged(!vm.allManaged) }
        }
    }

    @ViewBuilder
    private var addDomainSection: some View {
        if showAddDomain {
            VStack(alignment: .leading, spacing: 8) {
                TextField(L.admin.dns.addDomainPlaceholder, text: $addDomainText)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier(Ids.adminDnsAddDomainInput)
                    .automationField(Ids.adminDnsAddDomainInput, text: $addDomainText)
                // A domainless nest's first add is a one-way door — say so
                // before the submit that makes it irreversible
                // (`mail-multidomain.md` § Removing a local domain). The
                // nest, not this client, is authority on whether the add is
                // actually first — `addingFirstDomain` is a UX hint off the
                // already-fetched domain list. Untagged chrome (no ui.yaml
                // id), mirroring tui's `ADD_DOMAIN_PRIMARY_WARNING`.
                if vm.local?.addingFirstDomain == true {
                    Text(L.admin.dns.addDomainPrimaryWarning)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                HStack {
                    Button(L.admin.dns.addDomainSubmit) { submitAddDomain() }
                        .disabled(vm.isBusy || addDomainText.trimmed.isEmpty)
                        .accessibilityIdentifier(Ids.adminDnsAddDomainSubmitButton)
                        .automationActivate(
                            Ids.adminDnsAddDomainSubmitButton,
                            isEnabled: { !vm.isBusy && !addDomainText.trimmed.isEmpty }
                        ) { submitAddDomain() }
                        // The nest-side domain plane: adding a local domain is a
                        // nest write. (Revealing the form and typing into it are
                        // local, so those stay live — same split as the invite
                        // form.) The admin's OWN config document, by contrast,
                        // saves through `fauna.account.state.put`, which is OfflineSafe:
                        // the credential/managed/auto-renew/delegation controls on
                        // this page therefore carry NO gate on purpose.
                        .faunaGate("fauna.bridges.add_local_domain")
                    Button(L.common.cancel) {
                        showAddDomain = false
                        addDomainText = ""
                    }
                    .accessibilityIdentifier(Ids.adminDnsAddDomainCancelButton)
                }
            }
        } else {
            Button(L.admin.dns.addDomain) { showAddDomain = true }
                .accessibilityIdentifier(Ids.adminDnsAddDomainButton)
                .automationActivate(Ids.adminDnsAddDomainButton) { showAddDomain = true }
        }
    }

    private func submitAddDomain() {
        let d = addDomainText.trimmed
        guard !d.isEmpty else { return }
        Task {
            await vm.addDomain(d)
            if vm.errorMessage == nil {
                addDomainText = ""
                showAddDomain = false
            }
        }
    }

    // MARK: - Domain list

    @ViewBuilder
    private var domainList: some View {
        let names = vm.activeDomainNames
        if names.isEmpty {
            Text(L.admin.dns.empty)
                .font(.callout)
                .foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 20) {
                ForEach(Array(names.enumerated()), id: \.element) { offset, name in
                    // Scoped container (goal-doc rule 5): the admin actions drive
                    // `scope="admin-dns-domain[N]/..."`, and the cert-delegate
                    // form's submit/cancel are SINGLETONS revealed on one row —
                    // the flat occurrence-index heuristic maps scope index N to
                    // occurrence N of a 1-element id and can never resolve N≥1.
                    // Real subtree paths make those scoped queries containment-
                    // based, like media-item / post-card.
                    domainSection(name)
                        .automationScope(Ids.adminDnsDomain, index: offset)
                    Divider()
                }
            }
        }
    }

    private func domainSection(_ name: String) -> some View {
        let ld = vm.localDomain(name)
        let dd = vm.dnsDomain(name)
        let isPrimary = ld?.isPrimary ?? dd?.isPrimary ?? false
        return VStack(alignment: .leading, spacing: 10) {
            domainHeader(name: name, isPrimary: isPrimary, hasLocal: ld != nil, hasDns: dd != nil)
            if vm.isManagedOrDelegated(name) {
                autoRenewToggle(name: name, current: dd?.autoRenew ?? true)
            }
            if let ld {
                catchAllPicker(ld)
                catchAllClearedBySuccessionState(ld)
                roleAddressPickers(ld)
            }
            certStatusBadge(name)
            certIssuanceBlock(name)
            certDelegationBlock(name)
            if let dd {
                ForEach(Array(dd.records.enumerated()), id: \.offset) { _, record in
                    recordCard(record)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminDnsDomain)
        // Per-row presence entry so the flat in-process registry can `count`
        // `admin-dns-domain` sections (one entry per rendered domain).
        .automationValue(Ids.adminDnsDomain, text: { name })
    }

    private func domainHeader(name: String, isPrimary: Bool, hasLocal: Bool, hasDns: Bool) -> some View {
        HStack(alignment: .firstTextBaseline) {
            automationText(Ids.adminDnsDomainName, name)
                .font(.headline)
                .textSelection(.enabled)
            if isPrimary {
                Text(L.admin.dns.primaryBadge)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier(Ids.adminDnsDomainPrimaryBadge)
            }
            Spacer()
            if hasDns {
                Toggle(isOn: Binding(
                    get: { vm.isManaged(name) },
                    set: { v in Task { await vm.setMode(domain: name, managed: v) } }
                )) {
                    Text(vm.isManaged(name) ? L.admin.dns.modeManaged : L.admin.dns.modeManual)
                        .font(.caption)
                }
                .toggleStyle(.button)
                .disabled(vm.isBusy)
                .accessibilityIdentifier(Ids.adminDnsDomainMode)
                .automationActivate(
                    Ids.adminDnsDomainMode,
                    isEnabled: { !vm.isBusy },
                    value: { vm.isManaged(name) ? L.admin.dns.modeManaged : L.admin.dns.modeManual }
                ) {
                    Task { await vm.setMode(domain: name, managed: !vm.isManaged(name)) }
                }
            }
            // Present on every active row; disabled on the primary (cannot be removed).
            Button(L.admin.dns.remove, role: .destructive) {
                Task { await vm.removeDomain(name) }
            }
            .controlSize(.small)
            .disabled(vm.isBusy || isPrimary || !hasLocal)
            .accessibilityIdentifier(Ids.adminDnsDomainRemoveButton)
            .automationActivate(
                Ids.adminDnsDomainRemoveButton,
                isEnabled: { !vm.isBusy && !isPrimary && hasLocal }
            ) {
                Task { await vm.removeDomain(name) }
            }
            .faunaGate("fauna.bridges.remove_local_domain")
            renamePromoteAffordance(name: name, isPrimary: isPrimary)
        }
    }

    /// Primary-domain rename affordances (`mail-primary-domain-rename.md`
    /// § UX surface). On the **primary** row: "Rename primary" (disabled until
    /// a non-primary domain exists — the two-step rule — or while a rename is
    /// already in flight) + the in-flight state text. On a **non-primary** row:
    /// "Promote to primary" (hidden while a rename is running). Structural
    /// if/else — each branch is a genuine mount/unmount as `isPrimary`/
    /// `vm.activeRename` flip (`apple-e2e-automation.md`'s documented-safe
    /// "conditionally-hidden element must be ABSENT" pattern) — NOT a captured
    /// struct snapshot, so this never hits the stale-closure class of bug
    /// found in `task-delegation-kind-runner` (every dynamic read
    /// below routes through `vm.` live, never a locally-bound `let` snapshot).
    @ViewBuilder
    private func renamePromoteAffordance(name: String, isPrimary: Bool) -> some View {
        if isPrimary {
            Button(L.admin.dns.rename.button) { openRenameSheet(target: "") }
                .controlSize(.small)
                .disabled(vm.isBusy || !vm.renameAvailable || vm.activeRename != nil)
                .accessibilityIdentifier(Ids.adminDnsDomainRenameButton)
                .automationActivate(
                    Ids.adminDnsDomainRenameButton,
                    isEnabled: { !vm.isBusy && vm.renameAvailable && vm.activeRename == nil }
                ) { openRenameSheet(target: "") }
            if vm.activeRename != nil {
                automationText(
                    Ids.adminDnsDomainRenameState,
                    vm.activeRename.map { "\(L.admin.dns.rename.renamingTo) \($0.newPrimaryDomain) (\($0.state))" } ?? ""
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        } else if vm.activeRename == nil {
            Button(L.admin.dns.rename.promote) { openRenameSheet(target: name) }
                .controlSize(.small)
                .disabled(vm.isBusy)
                .accessibilityIdentifier(Ids.adminDnsDomainPromoteButton)
                .automationActivate(Ids.adminDnsDomainPromoteButton, isEnabled: { !vm.isBusy }) {
                    openRenameSheet(target: name)
                }
        }
    }

    private func autoRenewToggle(name: String, current: Bool) -> some View {
        Toggle(isOn: Binding(
            get: { current },
            set: { v in Task { await vm.setAutoRenew(domain: name, enabled: v) } }
        )) {
            Text(L.admin.dns.cert.autoRenew).font(.caption)
        }
        // `.switch` (not macOS-only `.checkbox`, which fails to compile on iOS:
        // "'checkbox' is unavailable in iOS" — it broke the FaunaiOS build, so
        // the whole iOS e2e suite skipped "iOS setup not available"). Matches the
        // sibling toggles in this view (domain-mode l.85) — cross-platform + uniform.
        .toggleStyle(.switch)
        .disabled(vm.isBusy)
        .accessibilityIdentifier(Ids.adminDnsDomainAutoRenew)
        .automationActivate(
            Ids.adminDnsDomainAutoRenew,
            isEnabled: { !vm.isBusy },
            // Re-read live state (the `current` param is the render-time snapshot);
            // `value` backs the `state` test-attr → "on"/"off".
            value: { (vm.dnsDomain(name)?.autoRenew ?? current) ? "on" : "off" }
        ) {
            let now = vm.dnsDomain(name)?.autoRenew ?? current
            Task { await vm.setAutoRenew(domain: name, enabled: !now) }
        }
    }

    // MARK: - Catch-all + role-address pickers

    private func catchAllPicker(_ d: LocalDomainView) -> some View {
        let current = d.catchAllActorId.map { data_to_hex($0) } ?? ""
        return Picker(L.admin.dns.catchAllLabel, selection: Binding(
            get: { current },
            set: { hex in
                Task { await vm.setCatchAll(domain: d.domain, actorId: hex.isEmpty ? nil : hex_to_data(hex)) }
            }
        )) {
            Text(L.admin.dns.catchAllNone).tag("")
            ForEach(vm.actors, id: \.actorId) { a in
                Text(vm.actorLabel(a.actorId)).tag(data_to_hex(a.actorId))
            }
            // Keep the current designation selectable if it's outside the loaded page.
            if let id = d.catchAllActorId, !vm.actors.contains(where: { $0.actorId == id }) {
                Text(vm.actorLabel(id)).tag(current)
            }
        }
        .pickerStyle(.menu)
        .disabled(vm.isBusy)
        .accessibilityIdentifier(Ids.adminDnsDomainCatchAllSelect)
        .automationSelect(
            Ids.adminDnsDomainCatchAllSelect,
            // Read the current selection's *display text* (the action layer's
            // `catch_all_selected` get_text expects the label, not the hex tag).
            value: {
                guard let id = vm.localDomain(d.domain)?.catchAllActorId else {
                    return L.admin.dns.catchAllNone
                }
                return vm.actorLabel(id)
            },
            isEnabled: { !vm.isBusy }
        ) { displayText in
            let actorId = actorIdForLabel(displayText)
            Task { await vm.setCatchAll(domain: d.domain, actorId: actorId) }
        }
        // The picker COMMITS on pick (no draft), and designate/clear are the same
        // kind — `actor: None` is the clear.
        .faunaGate("fauna.bridges.set_catch_all_actor")
    }

    /// Present only when a SUCCESSION (not an admin) last cleared this domain's
    /// catch-all — tells the admin why the picker above reads "None"
    /// and that unmatched mail is now bouncing; re-designating via that same
    /// picker is the fix. Same read-only-explainer idiom as the rename-state
    /// label (`Ids.adminDnsDomainRenameState` above) — linux's `build_domain_section`
    /// is the reference (`ADMIN_DNS_DOMAIN_CATCH_ALL_CLEARED_STATE`).
    @ViewBuilder
    private func catchAllClearedBySuccessionState(_ d: LocalDomainView) -> some View {
        if d.catchAllClearedBySuccessionAt != nil {
            automationText(Ids.adminDnsDomainCatchAllClearedState, L.admin.dns.catchAllClearedBySuccession)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    /// Map a picker option's *display text* (what the driver's `select(id, value)`
    /// sends, and what the menu shows) back to an actor id. Returns `nil` for the
    /// "None" / "Admin (default)" sentinel rows (which clear the designation). The
    /// inverse of `vm.actorLabel(_:)` over the loaded actor page.
    private func actorIdForLabel(_ text: String) -> Data? {
        if text == L.admin.dns.catchAllNone || text == L.admin.dns.roleAddressAdminDefault {
            return nil
        }
        return vm.actors.first { vm.actorLabel($0.actorId) == text }?.actorId
    }

    private func roleAddressPickers(_ d: LocalDomainView) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.admin.dns.roleAddressLabel).font(.caption).foregroundStyle(.secondary)
            ForEach(roleAddressOptions(), id: \.key) { option in
                roleAddressPicker(d, option)
            }
        }
    }

    private func roleAddressPicker(_ d: LocalDomainView, _ option: RoleAddressOption) -> some View {
        let role = option.kind
        let key = option.key
        let currentId = d.roleAddressOverrides.first { $0.role == role }?.actorId
        let current = currentId.map { data_to_hex($0) } ?? ""
        return Picker("\(key)@", selection: Binding(
            get: { current },
            set: { hex in
                Task { await vm.setRoleAddress(domain: d.domain, role: role, actorId: hex.isEmpty ? nil : hex_to_data(hex)) }
            }
        )) {
            Text(L.admin.dns.roleAddressAdminDefault).tag("")
            ForEach(vm.actors, id: \.actorId) { a in
                Text(vm.actorLabel(a.actorId)).tag(data_to_hex(a.actorId))
            }
            if let id = currentId, !vm.actors.contains(where: { $0.actorId == id }) {
                Text(vm.actorLabel(id)).tag(current)
            }
        }
        .pickerStyle(.menu)
        .disabled(vm.isBusy)
        .accessibilityIdentifier("admin-dns-domain-role-address-\(key)-select")
        .automationSelect(
            "admin-dns-domain-role-address-\(key)-select",
            // Read the current selection's *display text* (the action layer's
            // `role_address_selected` get_text expects the label, not the hex tag).
            value: {
                let liveId = vm.localDomain(d.domain)?
                    .roleAddressOverrides.first { $0.role == role }?.actorId
                guard let liveId else { return L.admin.dns.roleAddressAdminDefault }
                return vm.actorLabel(liveId)
            },
            isEnabled: { !vm.isBusy }
        ) { displayText in
            let actorId = actorIdForLabel(displayText)
            Task { await vm.setRoleAddress(domain: d.domain, role: role, actorId: actorId) }
        }
        .faunaGate("fauna.bridges.set_role_address")
    }

    // MARK: - Cert lifecycle (status badge + issuance + delegation)

    @ViewBuilder
    private func certStatusBadge(_ name: String) -> some View {
        let cert = vm.certStatus(name)
        // One shared call decides the whole badge: the state word AND which of the
        // two mutually-exclusive sub-labels follows it. `nil` = still checking.
        let view = cert.map {
            FaunaFFISwift.certStatusView(
                state: certStateSerde($0.state), isFloor: $0.isFloor, notAfterUnix: $0.notAfterUnix)
        }
        HStack(spacing: 6) {
            Text(L.admin.dns.cert.label).font(.caption).foregroundStyle(.secondary)
            Text(view.map { renderLocalizedText($0.state) } ?? L.admin.dns.statusChecking)
                .font(.caption.weight(.medium))
                .foregroundStyle(certStateColor(cert))
            // The shared `cert_status_view` decides the sub-label — the floor's
            // "(self-signed)", or a trusted cert's expiry, NEVER both. Suppressing
            // the floor's reassuring-looking `not_after` on an untrusted cert is
            // now `show_self_signed`, not a per-app `else if` (value-formatting.md
            // § Cert status badge; the exclusion is the shared contract's).
            if let view {
                if view.showSelfSigned {
                    Text(L.admin.dns.cert.selfSigned).font(.caption2).foregroundStyle(.secondary)
                } else if let exp = view.expiresAtUnix {
                    Text(L.admin.dns.cert.expires(date: formatDate(exp)))
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
        }
        .accessibilityIdentifier(Ids.adminDnsCertStatus)
        // Composite label (label + state + badges); register the state text the
        // `cert_statuses` get_text reads. Re-read live so a refresh updates it.
        .automationValue(Ids.adminDnsCertStatus, text: { certStateWord(vm.certStatus(name)) })
    }

    // The state word is the shared `cert_status_label` (via `cert_status_view`);
    // apple only crosses the stringly serde boundary (`CertHealthState` -> its
    // variant name), so the state->key map lives once in `fauna_core`, not per
    // client. `nil` cert = still checking.
    private func certStateWord(_ cert: CertStatusRow?) -> String {
        guard let cert else { return L.admin.dns.statusChecking }
        return renderLocalizedText(
            FaunaFFISwift.certStatusView(
                state: certStateSerde(cert.state), isFloor: cert.isFloor,
                notAfterUnix: cert.notAfterUnix).state)
    }
    private func certStateSerde(_ state: CertHealthState) -> String {
        switch state {
        case .validTrusted: "ValidTrusted"
        case .onFloorRenewNeeded: "OnFloorRenewNeeded"
        case .expiring: "Expiring"
        }
    }
    private func certStateColor(_ cert: CertStatusRow?) -> Color {
        switch cert?.state {
        case .validTrusted: .green
        case .onFloorRenewNeeded, .expiring: .orange
        case .none: .secondary
        }
    }

    @ViewBuilder
    private func certIssuanceBlock(_ name: String) -> some View {
        let pending = vm.pendingCert(name)
        VStack(alignment: .leading, spacing: 8) {
            Button(L.admin.dns.cert.issue) {
                Task { await vm.issueCert(domain: name, managedOrDelegated: vm.isManagedOrDelegated(name)) }
            }
            .controlSize(.small)
            .disabled(vm.isBusy || pending != nil)
            .accessibilityIdentifier(Ids.adminDnsCertIssueButton)
            .automationActivate(
                Ids.adminDnsCertIssueButton,
                isEnabled: { !vm.isBusy && vm.pendingCert(name) == nil }
            ) {
                Task { await vm.issueCert(domain: name, managedOrDelegated: vm.isManagedOrDelegated(name)) }
            }
            // This button covers BOTH issuance paths and they have different
            // classes, so the kind is taken from the SAME discriminant the action
            // itself uses rather than guessed: managed/delegated runs the whole
            // order and ends by delivering the cert to the nest
            // (`fauna.tls.publish_cert`), while manual *phase 1* only opens the CA
            // order and stashes the breadcrumb in the admin's own config document
            // (`fauna.account.state.put`, OfflineSafe — so that path stays live, decided
            // by the shared table, never by a class test written here).
            .faunaGate(vm.isManagedOrDelegated(name)
                       ? "fauna.tls.publish_cert" : "fauna.account.state.put")

            if let pending {
                Text(L.admin.dns.cert.pasteInstructions).font(.caption).foregroundStyle(.secondary)
                ForEach(Array(pending.challenges.enumerated()), id: \.offset) { _, challenge in
                    recordCard(challenge)
                }
                HStack {
                    Button(L.admin.dns.cert.issueComplete) {
                        Task { await vm.completeManualIssue() }
                    }
                    .disabled(vm.isBusy)
                    .accessibilityIdentifier(Ids.adminDnsCertCompleteButton)
                    .automationActivate(
                        Ids.adminDnsCertCompleteButton,
                        isEnabled: { !vm.isBusy }
                    ) {
                        Task { await vm.completeManualIssue() }
                    }
                    // Manual phase 2 finishes the order and DELIVERS the cert to
                    // the nest; Cancel beside it only clears the local breadcrumb,
                    // so it stays live with no nest.
                    .faunaGate("fauna.tls.publish_cert")
                    Button(L.admin.dns.cert.issueCancel) {
                        Task { await vm.cancelManualIssue() }
                    }
                    .accessibilityIdentifier(Ids.adminDnsCertCancelButton)
                    .automationActivate(Ids.adminDnsCertCancelButton) {
                        Task { await vm.cancelManualIssue() }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func certDelegationBlock(_ name: String) -> some View {
        if let delegation = vm.delegation(name) {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(L.admin.dns.cert.renewalsAutomated)
                        .font(.caption).foregroundStyle(.green)
                    Spacer()
                    Button(L.admin.dns.cert.removeDelegation, role: .destructive) {
                        Task { await vm.removeDelegation(domain: name) }
                    }
                    .controlSize(.small)
                    .disabled(vm.isBusy)
                    .accessibilityIdentifier(Ids.adminDnsCertRemoveDelegationButton)
                    .automationActivate(
                        Ids.adminDnsCertRemoveDelegationButton,
                        isEnabled: { !vm.isBusy }
                    ) {
                        Task { await vm.removeDelegation(domain: name) }
                    }
                }
                recordCard(delegation.cname)
            }
        } else if delegatingDomain == name {
            HStack {
                Picker(L.admin.dns.cert.delegateZoneLabel, selection: $delegateZone) {
                    ForEach(vm.credZones, id: \.self) { zone in Text(zone).tag(zone) }
                }
                .pickerStyle(.menu)
                .accessibilityIdentifier(Ids.adminDnsCertDelegateZoneSelect)
                .automationSelect(
                    Ids.adminDnsCertDelegateZoneSelect,
                    value: { delegateZone },
                    isEnabled: { !vm.isBusy }
                ) { zone in delegateZone = zone }
                Button(L.admin.dns.cert.delegateSubmit) {
                    let zone = delegateZone
                    Task { await vm.delegateRenewal(domain: name, zone: zone); delegatingDomain = nil }
                }
                .disabled(vm.isBusy || delegateZone.isEmpty)
                .accessibilityIdentifier(Ids.adminDnsCertDelegateSubmitButton)
                .automationActivate(
                    Ids.adminDnsCertDelegateSubmitButton,
                    isEnabled: { !vm.isBusy && !delegateZone.isEmpty }
                ) {
                    let zone = delegateZone
                    Task { await vm.delegateRenewal(domain: name, zone: zone); delegatingDomain = nil }
                }
                Button(L.admin.dns.cert.delegateCancel) { delegatingDomain = nil }
                    .accessibilityIdentifier(Ids.adminDnsCertDelegateCancelButton)
                    .automationActivate(Ids.adminDnsCertDelegateCancelButton) {
                        delegatingDomain = nil
                    }
            }
        } else {
            Button(L.admin.dns.cert.delegate) { beginDelegate(name) }
            .controlSize(.small)
            .disabled(vm.isBusy || vm.credZones.isEmpty)
            .help(vm.credZones.isEmpty ? L.admin.dns.cert.delegateNoZones : "")
            .accessibilityIdentifier(Ids.adminDnsCertDelegateButton)
            .automationActivate(
                Ids.adminDnsCertDelegateButton,
                isEnabled: { !vm.isBusy && !vm.credZones.isEmpty }
            ) { beginDelegate(name) }
        }
    }

    /// Reveal the inline CNAME renewal-delegation form for `name`, defaulting the
    /// zone-picker to the first held-credential zone. Shared by the reveal Button
    /// and its `.automationActivate` so the two never diverge.
    private func beginDelegate(_ name: String) {
        delegateZone = vm.credZones.first ?? ""
        delegatingDomain = name
    }

    // MARK: - Primary-domain rename (mail-primary-domain-rename.md § UX surface)

    /// Deployment-wide in-flight/grace-window rename banner. A dumb render of
    /// `vm.activeRename` — the descriptive text (title/old→new/state/countdown)
    /// is plain, un-registered `Text` (ui.yaml's `admin-dns-rename-banner`
    /// component carries no dedicated state/countdown child id, only the
    /// container + the 6 action-button ids below), so re-reading it fresh on
    /// every body pass is sufficient; only the action affordances need
    /// automation registration.
    @ViewBuilder
    private var renameBanner: some View {
        if let rename = vm.activeRename {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text(L.admin.dns.rename.bannerTitle).font(.headline)
                    Text("\(rename.oldPrimaryDomain) → \(rename.newPrimaryDomain)")
                        .font(.callout.monospaced())
                }
                HStack(spacing: 12) {
                    Text("\(L.admin.dns.rename.stateLabel) \(rename.state)")
                    if rename.isPostFlipActive, let endsAt = rename.graceEndsAt {
                        Text("\(L.admin.dns.rename.graceEnds) \(graceRemaining(endsAt))")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                renameBannerActions
            }
            .padding(12)
            .background(Color.secondary.opacity(0.08))
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.adminDnsRenameBanner)
            // Presence entry so the flat in-process registry can `count`
            // the banner (a bare `.accessibilityIdentifier` is invisible to it).
            .automationValue(Ids.adminDnsRenameBanner, text: { "" })
        }
    }

    /// Complete / extend / abort, each gated on the rename view's `can*` flags
    /// (the nest owns validation; the client only shows/hides) and reveal-then-
    /// confirm for the two destructive-ish transitions. Every `isEnabled`/
    /// dispatch closure re-reads `vm.activeRename` live rather than closing
    /// over the `rename` snapshot bound above, so this stays correct across
    /// the rename's whole non-terminal lifetime without the banner itself
    /// remounting (the fixed-identity / live-changing-field hazard
    /// documented for `task-delegation-kind-runner`).
    @ViewBuilder
    private var renameBannerActions: some View {
        if let rename = vm.activeRename {
            HStack(spacing: 8) {
                if rename.canComplete || rename.canForceComplete {
                    if confirmingComplete {
                        if rename.canForceComplete {
                            Text(L.admin.dns.rename.completeForceWarning)
                                .font(.caption2)
                                .foregroundStyle(.orange)
                        }
                        Button(L.admin.dns.rename.completeConfirm) { completeRename() }
                            .accessibilityIdentifier(Ids.adminDnsRenameCompleteConfirmButton)
                            .automationActivate(Ids.adminDnsRenameCompleteConfirmButton) { completeRename() }
                            .faunaGate("fauna.bridges.complete_primary_domain_rename")
                    } else {
                        Button(L.admin.dns.rename.complete) { confirmingComplete = true }
                            .accessibilityIdentifier(Ids.adminDnsRenameCompleteButton)
                            .automationActivate(Ids.adminDnsRenameCompleteButton) {
                                confirmingComplete = true
                            }
                    }
                }
                if rename.canExtend {
                    TextField("", text: $extendDays)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 50)
                        .accessibilityIdentifier(Ids.adminDnsRenameExtendDaysInput)
                        .automationField(Ids.adminDnsRenameExtendDaysInput, text: $extendDays)
                    Button(L.admin.dns.rename.extend) { extendRename() }
                        .accessibilityIdentifier(Ids.adminDnsRenameExtendButton)
                        .automationActivate(Ids.adminDnsRenameExtendButton) { extendRename() }
                        .faunaGate("fauna.bridges.extend_primary_domain_rename_grace")
                }
                if rename.canAbort {
                    if confirmingAbort {
                        if !rename.isPreFlip {
                            Text(L.admin.dns.rename.abortPostflipWarning)
                                .font(.caption2)
                                .foregroundStyle(.orange)
                        }
                        Button(L.admin.dns.rename.abortConfirm, role: .destructive) { abortRename() }
                            .accessibilityIdentifier(Ids.adminDnsRenameAbortConfirmButton)
                            .automationActivate(Ids.adminDnsRenameAbortConfirmButton) { abortRename() }
                            .faunaGate("fauna.bridges.abort_primary_domain_rename")
                    } else {
                        Button(L.admin.dns.rename.abort, role: .destructive) { confirmingAbort = true }
                            .accessibilityIdentifier(Ids.adminDnsRenameAbortButton)
                            .automationActivate(Ids.adminDnsRenameAbortButton) {
                                confirmingAbort = true
                            }
                    }
                }
            }
            .controlSize(.small)
            .disabled(vm.isBusy)
        }
    }

    /// Start-a-rename wizard (inline reveal, single instance — opened from
    /// either the primary row's "Rename primary" or a non-primary row's
    /// "Promote to primary"). Picks the new primary from the existing active
    /// non-primary domains (the two-step rule — the wizard never adds a
    /// domain) plus an optional grace-days override, then dispatches
    /// `StartPrimaryRename`.
    @ViewBuilder
    private var renameSheetSection: some View {
        if renameSheetOpen {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.dns.rename.sheetTitle).font(.headline)
                renameTargetPicker
                TextField(L.admin.dns.rename.graceDaysLabel, text: $renameGraceDays)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.adminDnsRenameGraceDaysInput)
                    .automationField(Ids.adminDnsRenameGraceDaysInput, text: $renameGraceDays)
                HStack {
                    Button(L.admin.dns.rename.submit) { submitRename() }
                        .disabled(vm.isBusy || renameTarget.isEmpty)
                        .accessibilityIdentifier(Ids.adminDnsRenameSubmitButton)
                        .automationActivate(
                            Ids.adminDnsRenameSubmitButton,
                            isEnabled: { !vm.isBusy && !renameTarget.isEmpty }
                        ) { submitRename() }
                        // Opening this wizard and picking a target are local; only
                        // the submit starts the nest-side rename.
                        .faunaGate("fauna.bridges.start_primary_domain_rename")
                    Button(L.admin.dns.rename.cancel) { cancelRenameSheet() }
                        .accessibilityIdentifier(Ids.adminDnsRenameCancelButton)
                        .automationActivate(Ids.adminDnsRenameCancelButton) { cancelRenameSheet() }
                }
            }
            .padding(12)
            .background(Color.secondary.opacity(0.06))
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(Ids.adminDnsRenameSheet)
            .automationValue(Ids.adminDnsRenameSheet, text: { "" })
        }
    }

    /// The promotion-target picker (`admin-dns-rename-new-primary-select`) —
    /// active non-primary domains, re-derived from `vm` live on every render
    /// (never a captured snapshot) so it stays current for as long as the
    /// sheet happens to stay open.
    private var renameTargetPicker: some View {
        let options = vm.activeDomainNames.filter { !(vm.localDomain($0)?.isPrimary ?? false) }
        return Picker(L.admin.dns.rename.newPrimaryLabel, selection: $renameTarget) {
            ForEach(options, id: \.self) { d in Text(d).tag(d) }
        }
        .pickerStyle(.menu)
        .accessibilityIdentifier(Ids.adminDnsRenameNewPrimarySelect)
        .automationSelect(
            Ids.adminDnsRenameNewPrimarySelect,
            value: { renameTarget },
            isEnabled: { !vm.isBusy }
        ) { picked in renameTarget = picked }
    }

    /// Reveal the sheet, pre-targeting `target` when opened from a non-primary
    /// row's "Promote to primary" (mirrors web `openRenameSheet`,
    /// `admin/dns/+page.svelte`). An empty `target` (opened from the primary
    /// row's "Rename primary") defaults to the first active non-primary domain.
    private func openRenameSheet(target: String) {
        renameTarget = target.isEmpty
            ? (vm.activeDomainNames.first { !(vm.localDomain($0)?.isPrimary ?? false) } ?? "")
            : target
        renameGraceDays = ""
        renameSheetOpen = true
    }
    private func cancelRenameSheet() {
        renameSheetOpen = false
        renameTarget = ""
        renameGraceDays = ""
    }
    private func submitRename() {
        guard !renameTarget.isEmpty, let domainId = vm.localDomain(renameTarget)?.domainId else { return }
        let days = Int64(renameGraceDays.trimmed)
        renameSheetOpen = false
        Task { await vm.startPrimaryRename(newPrimaryDomainId: domainId, graceDays: days) }
    }
    private func completeRename() {
        guard let rename = vm.activeRename else { return }
        let force = rename.canForceComplete
        confirmingComplete = false
        Task { await vm.completePrimaryRename(renameId: rename.renameId, force: force) }
    }
    private func extendRename() {
        guard let rename = vm.activeRename, let days = Int64(extendDays.trimmed), days >= 1 else { return }
        Task { await vm.extendPrimaryRenameGrace(renameId: rename.renameId, additionalDays: days) }
    }
    private func abortRename() {
        guard let rename = vm.activeRename else { return }
        confirmingAbort = false
        Task { await vm.abortPrimaryRename(renameId: rename.renameId, reason: nil) }
    }
    /// Client-rendered countdown to `graceEndsAt` (the state is nest-
    /// authoritative; only the *display* is client-side). Shared formatting
    /// via `fauna_core::format::grace_countdown`.
    private func graceRemaining(_ endsAtMs: Int64) -> String {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        guard let lt = FaunaFFISwift.graceCountdown(deadlineMs: endsAtMs, nowMs: nowMs) else {
            return L.admin.dns.rename.graceElapsed
        }
        return renderLocalizedText(lt)
    }

    // MARK: - Record card

    private func recordCard(_ record: DnsRecordRow) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(L.admin.dns.fieldName).font(.caption2).foregroundStyle(.secondary)
                automationText(Ids.adminDnsRecordName, record.name)
                    .font(.caption.monospaced()).textSelection(.enabled)
            }
            HStack {
                Text(L.admin.dns.fieldType).font(.caption2).foregroundStyle(.secondary)
                automationText(Ids.adminDnsRecordType, record.recordType)
                    .font(.caption)
            }
            HStack(alignment: .top) {
                Text(L.admin.dns.fieldValue).font(.caption2).foregroundStyle(.secondary)
                automationText(Ids.adminDnsRecordValue, record.expected)
                    .font(.caption.monospaced()).textSelection(.enabled)
            }
            HStack {
                automationText(Ids.adminDnsRecordStatus, verdictText(record.verdict))
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(verdictColor(record.verdict))
                Spacer()
                Button(L.admin.dns.copy) { Pasteboard.copy(record.expected) }
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.adminDnsRecordCopyButton)
                    .automationActivate(Ids.adminDnsRecordCopyButton) { Pasteboard.copy(record.expected) }
            }
            // Reverse DNS is set at the admin's server/VPS provider, never
            // zone-published (dns-management.md § Records covered) — the one
            // record type that needs this advisory (rule-A approved 2026-08-15).
            if record.recordType == "PTR" {
                automationText(Ids.adminDnsRecordProviderNote, L.admin.dns.ptrProviderNote)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(8)
        .background(Color.secondary.opacity(0.06))
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminDnsRecord)
        // Per-row presence entry so the flat in-process registry can `count`
        // `admin-dns-record` rows (the test reads records by index).
        .automationValue(Ids.adminDnsRecord, text: { record.name })
    }

    // The verdict word is the shared `dns_verdict_label`; apple only crosses the
    // stringly serde boundary (verdict status -> its variant name) and hands over
    // what public DNS actually served, so a `Mismatch` reads "found 1.2.3.4"
    // instead of dead-ending. windows/android carry the identical leg.
    private func verdictText(_ v: RecordVerdict?) -> String {
        let serde: String
        switch v?.status {
        case .ok: serde = "Ok"
        case .missing: serde = "Missing"
        case .mismatch: serde = "Mismatch"
        case .checking, .none: serde = "Checking"
        }
        return renderLocalizedText(
            FaunaFFISwift.dnsVerdictLabel(status: serde, observed: v?.observed ?? [])
        )
    }
    private func verdictColor(_ v: RecordVerdict?) -> Color {
        switch v?.status {
        case .ok: .green
        case .missing, .mismatch: .red
        case .checking, .none: .secondary
        }
    }

    // MARK: - Removed domains + credentials

    @ViewBuilder
    private var removedDomainsSection: some View {
        if !vm.removedDomains.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.dns.removedTitle).font(.headline)
                Text(L.admin.dns.removedDesc).font(.caption).foregroundStyle(.secondary)
                ForEach(vm.removedDomains, id: \.domain) { d in
                    HStack {
                        automationText(Ids.adminDnsRemovedDomainName, d.domain)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button(L.admin.dns.restore) {
                            Task { await vm.restoreDomain(d.domain) }
                        }
                        .controlSize(.small)
                        .disabled(vm.isBusy)
                        .accessibilityIdentifier(Ids.adminDnsRemovedDomainRestoreButton)
                        .automationActivate(
                            Ids.adminDnsRemovedDomainRestoreButton,
                            isEnabled: { !vm.isBusy }
                        ) {
                            Task { await vm.restoreDomain(d.domain) }
                        }
                        .faunaGate("fauna.bridges.restore_local_domain")
                    }
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.adminDnsRemovedDomain)
                }
            }
        }
    }

    @ViewBuilder
    private var credentialsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.admin.dns.credentialsTitle).font(.headline)
            if vm.credentials.isEmpty {
                Text(L.admin.dns.credentialsEmpty)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier(Ids.adminDnsCredentialsList)
                    // Presence entry so the in-process `is_visible` check finds the
                    // section even when it holds no credential rows yet.
                    .automationValue(Ids.adminDnsCredentialsList, text: { "" })
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(Array(vm.credentials.enumerated()), id: \.offset) { idx, cred in
                        credentialItem(index: UInt32(idx), cred: cred)
                    }
                }
                .accessibilityIdentifier(Ids.adminDnsCredentialsList)
                .automationValue(Ids.adminDnsCredentialsList, text: { "" })
            }
            addCredentialForm
        }
    }

    @ViewBuilder
    private var addCredentialForm: some View {
        if showAddCredential {
            VStack(alignment: .leading, spacing: 8) {
                ProviderRow(
                    providers: Self.dnsProviders,
                    selectedId: selectedProvider,
                    kind: "admin-dns-add-credential",
                    isEnabled: { _ in !vm.isBusy },
                    onSelect: { pid in
                        selectedProvider = pid
                        credValues = [:]
                    }
                )
                if let provider = Self.dnsProviders.first(where: { $0.id == selectedProvider }) {
                    credentialFields(provider)
                }
                HStack {
                    Button(L.admin.dns.addCredentialSubmit) { submitAddCredential() }
                        .disabled(vm.isBusy || !canSubmitCredential)
                        .accessibilityIdentifier(Ids.adminDnsAddCredentialSubmitButton)
                        .automationActivate(
                            Ids.adminDnsAddCredentialSubmitButton,
                            isEnabled: { !vm.isBusy && canSubmitCredential }
                        ) { submitAddCredential() }
                    Button(L.common.cancel) { resetAddCredential() }
                        .accessibilityIdentifier(Ids.adminDnsAddCredentialCancelButton)
                        .automationActivate(Ids.adminDnsAddCredentialCancelButton) {
                            resetAddCredential()
                        }
                }
            }
        } else {
            Button(L.admin.dns.addCredential) { showAddCredential = true }
                .accessibilityIdentifier(Ids.adminDnsAddCredentialButton)
                .automationActivate(Ids.adminDnsAddCredentialButton) {
                    showAddCredential = true
                }
        }
    }

    /// The selected provider's DNS credential fields (each id'd by its bare
    /// `field.id`, matching linux `rebuild_credential_fields`).
    private func credentialFields(_ provider: ProviderMeta) -> some View {
        let fields = provider.fields.filter { $0.kinds.contains(.dns) }
        return VStack(alignment: .leading, spacing: 6) {
            ForEach(fields, id: \.id) { field in
                LabeledContent(L.lookup(field.labelKey)) {
                    if field.type == .secret {
                        SecureField("", text: credBinding(field.id))
                            .accessibilityIdentifier(field.id)
                            .automationField(field.id, text: credBinding(field.id))
                    } else {
                        TextField("", text: credBinding(field.id))
                            .accessibilityIdentifier(field.id)
                            .automationField(field.id, text: credBinding(field.id))
                    }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminDnsAddCredentialForm)
    }

    private func credBinding(_ id: String) -> Binding<String> {
        Binding(get: { credValues[id] ?? "" }, set: { credValues[id] = $0 })
    }

    private var canSubmitCredential: Bool {
        guard let provider = Self.dnsProviders.first(where: { $0.id == selectedProvider }) else { return false }
        let required = provider.fields.filter { $0.kinds.contains(.dns) && $0.required }
        return required.allSatisfy { !(credValues[$0.id] ?? "").trimmed.isEmpty }
    }

    private func submitAddCredential() {
        guard let provider = Self.dnsProviders.first(where: { $0.id == selectedProvider }) else { return }
        let fields = provider.fields
            .filter { $0.kinds.contains(.dns) }
            .map { DnsCredentialField(id: $0.id, value: credValues[$0.id] ?? "") }
        let label = L.lookup(provider.displayNameKey)
        Task {
            await vm.putCredentials(providerId: provider.id, fields: fields, label: label)
            if vm.errorMessage == nil { resetAddCredential() }
        }
    }

    private func resetAddCredential() {
        showAddCredential = false
        selectedProvider = nil
        credValues = [:]
    }

    private static let dnsProviders: [ProviderMeta] = PROVIDERS.filter { $0.capabilities.contains(.dns) }

    private func credentialItem(index: UInt32, cred: CredentialSummary) -> some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                automationText(
                    Ids.adminDnsCredentialItemProvider,
                    cred.label.isEmpty ? cred.providerId : cred.label
                )
                .font(.body)
                automationText(
                    Ids.adminDnsCredentialItemZones,
                    cred.zones.isEmpty ? L.admin.dns.credentialZones
                                       : cred.zones.joined(separator: ", ")
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()
            Button(L.admin.dns.remove, role: .destructive) {
                Task { await vm.clearCredentials(index: index) }
            }
            .controlSize(.small)
            .disabled(vm.isBusy)
            .accessibilityIdentifier(Ids.adminDnsCredentialItemClearButton)
            .automationActivate(
                Ids.adminDnsCredentialItemClearButton,
                isEnabled: { !vm.isBusy }
            ) {
                Task { await vm.clearCredentials(index: index) }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminDnsCredentialItem)
        // Per-row presence entry so the flat registry can `count` credential rows.
        .automationValue(Ids.adminDnsCredentialItem, text: { cred.label.isEmpty ? cred.providerId : cred.label })
    }

    // MARK: - Helpers

    private func formatDate(_ unix: Int64) -> String {
        ValueFormat.absoluteDate(epochMs: unix * 1000)
    }
}

private extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
