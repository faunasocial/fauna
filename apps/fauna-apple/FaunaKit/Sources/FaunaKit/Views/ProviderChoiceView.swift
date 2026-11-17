import SwiftUI
import FaunaFFISwift

/// Stage 4d: choose providers (`docs/goal/behavior/onboarding-provisioning.md`
/// § 3d) — one page between the handle check and `nest_provisioning`, in place
/// of `dns_config` → `vps_config` on the set-up path.
///
/// ui.yaml `provider_choice`: the goal lines `provider-goal-row[domain|dns|vps]`
/// (with `provider-goal-select[...]` only when more than one connected provider
/// can cover the goal), the provider rows `provider-row[<provider-id>|manual]`,
/// `dns-buy-domain-checkbox` and `dns-no-provider-message` (moved from
/// `dns_config`), the status line and Back / Continue — and, while a provider
/// row is open, its `provider-dialog` (link, open-in-browser, help,
/// `provider-credentials-form-{field}`, Connect / Disconnect, the registrar note
/// or contact form, a status line, Close), painted directly under its row as
/// tui does.
///
/// **Every rule is the machine's** (§ 3d *Where logic lives*): which goals and
/// who covers them (`goalRows()`), what each provider can do and why it cannot
/// cover a goal (`providerRows()`), the buy box (`canOfferBuyDomain()`), a
/// dialog's fields (`visibleProviderFields`), Connect (`canConnectProvider`),
/// the registrar note / contact form, and why Continue is dead
/// (`providersContinueBlockedReason()`). The view keeps two pieces of state, both
/// presentation: which dialog is open, and the typed credentials it paints (the
/// machine stores them as secrets and hands nothing back — tui's `inputs`
/// buffer; every keystroke is still pushed through `setProviderCred`).
///
/// Shared by macOS and iOS (priority #2). Reference: tui
/// `apps/fauna-tui/src/wizard/provider_choice.rs`, the lead app.
public struct ProviderChoiceView: View {
    @Bindable var vm: OnboardingVM
    /// The open dialog's provider id — presentation only, like tui's
    /// `Wizard::provider_dialog`.
    @State private var openDialog: String?
    /// Typed credentials, keyed `"<provider>/<field>"`, for paint only.
    @State private var typed: [String: String] = [:]

    /// The manual row's key in `provider-row[...]` and `provider-goal-select[dns]`.
    static let manual = "manual"

    public init(vm: OnboardingVM) {
        self.vm = vm
    }

    public var body: some View {
        // The tracked read: `errorMessage` reads the VM's observation tick, so
        // every machine change (a connect landing, a cover moving) re-renders
        // this body and its untracked `vm.machine.<getter>()` reads.
        let error = vm.errorMessage
        let m = vm.machine
        let goals = m.goalRows()
        let blocked = m.providersContinueBlockedReason()
        let continuePossible: Possibility = blocked.map { .blocked(reason: $0) } ?? .possible

        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                automationText(Ids.pageHeading, L.onboarding.providerChoice.title)
                    .font(.title2)
                Text(L.onboarding.providerChoice.description)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                if let error {
                    ErrorBanner(message: error)
                }

                goalSection(goals)

                // The buy box, offered only for a domain that can be bought.
                if m.canOfferBuyDomain() {
                    Toggle(L.onboarding.dnsConfig.buyDomainCheckbox, isOn: Binding(
                        get: { vm.machine.dnsConfig().buyDomain },
                        set: { vm.machine.toggleBuyDomain(on: $0) }
                    ))
                    .accessibilityIdentifier(Ids.dnsBuyDomainCheckbox)
                    .automationActivate(
                        Ids.dnsBuyDomainCheckbox,
                        value: { vm.machine.dnsConfig().buyDomain ? "on" : "off" }
                    ) {
                        vm.machine.toggleBuyDomain(on: !vm.machine.dnsConfig().buyDomain)
                    }
                }
                if m.shouldShowNoProviderMessage() {
                    automationText(
                        Ids.dnsNoProviderMessage,
                        L.onboarding.dnsConfig.noProviderCarriesTld(
                            tld: handleTld(handle: vm.machine.currentHandle()) ?? ""))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }

                providerSection(goals)

                automationText(
                    Ids.providerChoiceStatusText,
                    blocked.map { renderLocalizedText($0) } ?? "")
                    .font(.callout)
                    .foregroundStyle(.secondary)

                HStack {
                    Button(L.common.back) { vm.machine.back() }
                        .accessibilityIdentifier(Ids.providerChoiceBackButton)
                        .automationActivate(Ids.providerChoiceBackButton) {
                            vm.machine.back()
                        }
                    Spacer()
                    Button(L.common.continue) { continueFromProviders() }
                        .keyboardShortcut(.defaultAction)
                        .accessibilityIdentifier(Ids.providerChoiceContinueButton)
                        .automationActivate(Ids.providerChoiceContinueButton) {
                            continueFromProviders()
                        }
                        // Enabled is the machine's answer (`ui/README.md`
                        // § Offer only what is possible, rule 6).
                        .faunaPossible(continuePossible)
                }
            }
            .padding()
        }
    }

    private func continueFromProviders() {
        openDialog = nil
        try? vm.machine.continueFromProviders()
    }

    // ── Goals ──────────────────────────────────────────────────────────────

    @ViewBuilder
    private func goalSection(_ goals: [GoalRow]) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(goals.enumerated()), id: \.offset) { _, row in
                goalRow(row)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.providerGoalRow)
        .automationValue(Ids.providerGoalRow, text: { "" })
    }

    @ViewBuilder
    private func goalRow(_ row: GoalRow) -> some View {
        let g = Self.goalKey(row.goal)
        let id = "\(Ids.providerGoalRow)[\(g)]"
        let line = goalLine(row)
        VStack(alignment: .leading, spacing: 2) {
            Text(line)
                .accessibilityIdentifier(id)
                .automationValue(id, text: { line }, attributes: {
                    var attrs = ["state": row.cover == nil ? "open" : "covered"]
                    if let c = row.cover { attrs["provider"] = Self.coverKey(c) }
                    return attrs
                })
            // The goal's un-id'd note — DNS covered at a provider whose zone the
            // domain does not point at yet (§ 3d, *A zone the domain does not
            // point at*).
            if let note = row.note {
                Text(renderLocalizedText(note))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // The choice, only where there is one to make.
            if row.candidates.count > 1 {
                goalSelect(row, goal: g)
            }
        }
        // A goal line changes text and attributes in place as covers move; key
        // the identity on both so the registration is re-read.
        .id("\(id):\(line):\(row.cover.map(Self.coverKey) ?? "")")
    }

    @ViewBuilder
    private func goalSelect(_ row: GoalRow, goal g: String) -> some View {
        let id = "\(Ids.providerGoalSelect)[\(g)]"
        let selected = row.cover.map(Self.coverKey) ?? ""
        Picker(L.onboarding.providerChoice.goalSelect, selection: Binding(
            get: { selected },
            set: { select(goal: row.goal, key: $0) }
        )) {
            ForEach(row.candidates, id: \.self) { c in
                Text(Self.providerName(c)).tag(c)
            }
        }
        .accessibilityIdentifier(id)
        .automationSelect(
            id,
            value: { selected },
            options: { row.candidates },
            set: { select(goal: row.goal, key: $0) }
        )
    }

    private func select(goal: ProviderGoal, key: String) {
        let cover: GoalCover = key == Self.manual ? .manual : .provider(providerId: key)
        try? vm.machine.selectGoalCover(goal: goal, cover: cover)
    }

    private func goalLine(_ row: GoalRow) -> String {
        let d = vm.machine.providerGoalDomain()
        let name: String
        switch row.goal {
        case .domain: name = L.onboarding.providerChoice.goalDomain(domain: d)
        case .dns: name = L.onboarding.providerChoice.goalDns(domain: d)
        case .vps: name = L.onboarding.providerChoice.goalVps
        }
        let status = row.cover.map {
            L.onboarding.providerChoice.goalCoveredBy(provider: Self.providerName(Self.coverKey($0)))
        } ?? L.onboarding.providerChoice.goalOpen
        var line = "\(name) — \(status)"
        if let price = row.priceCents {
            let cur = row.currency ?? "USD"
            let first = formatPrice(cents: price, currency: cur)
            let priced = row.renewalCents.map {
                L.onboarding.providerChoice.goalPrice(
                    price: first, renewal: formatPrice(cents: $0, currency: cur))
            } ?? L.onboarding.providerChoice.goalPriceFirstYear(price: first)
            line += " · \(priced)"
        }
        return line
    }

    // ── Providers ──────────────────────────────────────────────────────────

    @ViewBuilder
    private func providerSection(_ goals: [GoalRow]) -> some View {
        let dnsCover = goals.first { $0.goal == .dns }?.cover
        VStack(alignment: .leading, spacing: 8) {
            Text(L.onboarding.providerChoice.providersHeading).font(.headline)
            ForEach(vm.machine.providerRows(), id: \.providerId) { row in
                providerRow(row, dnsCover: dnsCover)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.providerRow)
        .automationValue(Ids.providerRow, text: { L.onboarding.providerChoice.providersHeading })
    }

    @ViewBuilder
    private func providerRow(_ row: FaunaFFISwift.ProviderRow, dnsCover: GoalCover?) -> some View {
        let pid = row.providerId
        let id = "\(Ids.providerRow)[\(pid)]"
        let manual = pid == Self.manual
        let text = manual
            ? "\(L.onboarding.providerChoice.manualRow) — \(L.onboarding.providerChoice.manualRowHelp)"
            : Self.rowText(row)
        let connected = manual ? dnsCover == .manual : row.connected
        // The manual row covers DNS only; while it cannot, it is dead and says
        // why beneath.
        let enabled = manual ? row.dnsReason == nil : true
        let tap = {
            if manual {
                select(goal: .dns, key: Self.manual)
            } else {
                openDialog = openDialog == pid ? nil : pid
            }
        }
        VStack(alignment: .leading, spacing: 4) {
            Button(action: tap) {
                Text(text).frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.bordered)
            .tint(connected ? .accentColor : .secondary)
            .disabled(!enabled)
            .accessibilityIdentifier(id)
            .automationActivate(
                id,
                isEnabled: { enabled },
                text: { text },
                attributes: { ["state": connected ? "connected" : "available"] },
                perform: tap
            )
            .id("\(id):\(text):\(connected)")
            if row.held {
                Text(L.onboarding.providerChoice.heldCredential)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // The manual row's server and registration reasons restate what the
            // row is (DNS only); only its DNS reason explains anything.
            if let reason = manual ? row.dnsReason.map(renderLocalizedText) : Self.reasonLine(row) {
                Text(reason)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // The open dialog paints directly under its own row.
            if openDialog == pid {
                dialog(row)
            }
        }
    }

    // ── The dialog ─────────────────────────────────────────────────────────

    @ViewBuilder
    private func dialog(_ row: FaunaFFISwift.ProviderRow) -> some View {
        let pid = row.providerId
        let m = vm.machine
        VStack(alignment: .leading, spacing: 8) {
            Text(Self.providerName(pid)).font(.headline)
            if let p = PROVIDERS.first(where: { $0.id == pid }) {
                if let url = URL(string: p.signupUrl) {
                    Link(p.signupUrl, destination: url)
                        .accessibilityIdentifier(Ids.providerDialogLink)
                        .automationValue(Ids.providerDialogLink, text: { p.signupUrl })
                    Button(L.onboarding.dnsConfig.openInBrowser) { OpenURL.open(url) }
                        .accessibilityIdentifier(Ids.providerDialogOpenBrowserButton)
                        .automationActivate(Ids.providerDialogOpenBrowserButton) { OpenURL.open(url) }
                }
                automationText(Ids.providerDialogHelpText, L.lookup(p.helpKey))
                    .fixedSize(horizontal: false, vertical: true)
                if let notes = m.providerRegistrarNotesKey(providerId: pid) {
                    DnsRegistrarNotes(notesKey: notes)
                }

                CredentialsForm(
                    fields: m.visibleProviderFields(providerId: pid),
                    kind: "provider",
                    getCred: { typed["\(pid)/\($0)"] ?? "" },
                    setCred: { field, value in
                        typed["\(pid)/\(field)"] = value
                        vm.machine.setProviderCred(providerId: pid, fieldId: field, value: value)
                    },
                    hostedAuthCanBegin: { vm.machine.providerHostedAuthCanBegin(providerId: pid, fieldId: $0) },
                    hostedAuthState: { vm.machine.providerHostedAuthState(providerId: pid, fieldId: $0) },
                    hostedAuthBegin: { try? await vm.machine.providerHostedAuthBegin(providerId: pid, fieldId: $0) },
                    hostedAuthWait: { _ = try? await vm.machine.providerHostedAuthWait(providerId: pid, fieldId: $0) }
                )

                if row.connected {
                    Button(L.onboarding.providerChoice.disconnect) {
                        vm.machine.disconnectProvider(providerId: pid)
                    }
                    .accessibilityIdentifier(Ids.providerDisconnectButton)
                    .automationActivate(Ids.providerDisconnectButton) {
                        vm.machine.disconnectProvider(providerId: pid)
                    }
                } else {
                    let canConnect = m.canConnectProvider(providerId: pid)
                    Button(L.onboarding.providerChoice.connect) { connect(pid) }
                        .buttonStyle(.borderedProminent)
                        .disabled(!canConnect)
                        .accessibilityIdentifier(Ids.providerConnectButton)
                        .automationActivate(
                            Ids.providerConnectButton,
                            isEnabled: { vm.machine.canConnectProvider(providerId: pid) }
                        ) { connect(pid) }
                }

                // The WHOIS contact form of a registrar that requires one.
                if m.providerShowsContactForm(providerId: pid) {
                    DnsContactForm(
                        contact: m.providerContact(providerId: pid),
                        onSet: { vm.machine.setProviderContact(providerId: pid, contact: $0) }
                    )
                }
            }

            let status = row.connecting
                ? L.onboarding.providerChoice.dialogConnecting
                : row.error ?? (row.connected ? L.onboarding.providerChoice.dialogConnected : "")
            automationText(Ids.providerDialogStatusText, status)
                .font(.callout)
                .foregroundStyle(row.error == nil ? Color.secondary : Color.red)

            Button(L.onboarding.providerChoice.close) { openDialog = nil }
                .accessibilityIdentifier(Ids.providerDialogCloseButton)
                .automationActivate(Ids.providerDialogCloseButton) { openDialog = nil }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 8).stroke(Color.secondary.opacity(0.4)))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.providerDialog)
        .automationValue(Ids.providerDialog, text: { Self.providerName(pid) })
    }

    private func connect(_ pid: String) {
        Task { try? await vm.machine.connectProvider(providerId: pid) }
    }

    // ── Labels ─────────────────────────────────────────────────────────────

    static func goalKey(_ goal: ProviderGoal) -> String {
        switch goal {
        case .domain: return "domain"
        case .dns: return "dns"
        case .vps: return "vps"
        }
    }

    static func coverKey(_ cover: GoalCover) -> String {
        switch cover {
        case .provider(let providerId): return providerId
        case .manual: return manual
        }
    }

    /// A provider's display name, or the id for one the registry does not know.
    static func providerName(_ id: String) -> String {
        if id == manual { return L.onboarding.providerChoice.manualRow }
        return PROVIDERS.first { $0.id == id }.map { L.lookup($0.displayNameKey) } ?? id
    }

    /// What a capability lets the provider do. `zone` is a field kind only,
    /// never a provider capability, so it names nothing.
    static func capabilityLabel(_ c: CapabilityPlain) -> String? {
        switch c {
        case .registrar: return L.onboarding.providerChoice.capabilityRegistrar
        case .dns: return L.onboarding.providerChoice.capabilityDns
        case .vps: return L.onboarding.providerChoice.capabilityVps
        case .zone: return nil
        }
    }

    /// A provider row's text: its name, what it can do, whether it is connected.
    static func rowText(_ row: FaunaFFISwift.ProviderRow) -> String {
        let caps = row.capabilities.compactMap(capabilityLabel).joined(separator: ", ")
        let state = row.connecting
            ? L.onboarding.providerChoice.stateConnecting
            : row.connected
                ? L.onboarding.providerChoice.stateConnected
                : L.onboarding.providerChoice.stateAvailable
        return "\(providerName(row.providerId)) — \(caps) · \(state)"
    }

    /// The un-id'd line beneath a row: why it cannot cover the goals it cannot
    /// (rule 5), each distinct reason once.
    static func reasonLine(_ row: FaunaFFISwift.ProviderRow) -> String? {
        var seen: [String] = []
        for r in [row.domainReason, row.dnsReason, row.vpsReason].compactMap({ $0 }) {
            let text = renderLocalizedText(r)
            if !seen.contains(text) { seen.append(text) }
        }
        return seen.isEmpty ? nil : seen.joined(separator: " ")
    }
}
