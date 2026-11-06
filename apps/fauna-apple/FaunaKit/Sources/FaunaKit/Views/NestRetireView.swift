#if !FAUNA_KIDS
import SwiftUI

/// nest_retire — retire an app-provisioned nest
/// (`docs/goal/behavior/nest-retirement.md`; ui.yaml `onboarding.nest_retire`).
///
/// Shared by macOS and iOS (priority #2). A paint shell over the shared
/// `NestRetireMachine`: credentials → list → confirm → running → done, every state
/// the machine's. The credential state reuses `vps_config`'s controls by id
/// (`ProviderRow`, `CredentialsForm`, the verify button); the running and done
/// states reuse the `provisioning-step-row` glyphs. The lift of tui's
/// `wizard/nest_retire.rs` and android's `NestRetireScreen`.
///
/// `onExit` is told whether the session was dropped (this session's own nest was
/// retired from `admin-nest`): the host then signs out keeping credentials and
/// re-enters launch routing; otherwise it shows the entry again.
public struct NestRetireView: View {
    let vm: NestRetireVM
    let onExit: (_ sessionDropped: Bool) -> Void

    public init(vm: NestRetireVM, onExit: @escaping (_ sessionDropped: Bool) -> Void) {
        self.vm = vm
        self.onExit = onExit
    }

    public var body: some View {
        // The machine's snapshot getters are untracked: read the tick first so
        // every machine call repaints the page.
        let _ = vm.tick
        if let m = vm.machine {
            let snap = m.snapshot()
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(L.onboarding.retire.title).font(.title2)
                    Text(L.onboarding.retire.subtitle)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)

                    switch snap.phase {
                    case .credentials, .listing: credentials(m, snap)
                    case .list: list(m, snap)
                    case .confirm: confirm(m, snap)
                    case .running: running(m, snap)
                    case .done: done(m, snap)
                    }

                    if let error = snap.error, !error.isEmpty {
                        ErrorBanner(message: error)
                    }
                }
                .padding()
            }
        }
    }

    private func exit() { onExit(vm.leave()) }

    private func backButton() -> some View {
        Button(L.common.back) { if vm.back() { exit() } }
            .accessibilityIdentifier(Ids.retireBackButton)
            .automationActivate(Ids.retireBackButton) { if vm.back() { exit() } }
    }

    // MARK: credentials

    /// The `vps_config` provider row, credential form and verify button, by the
    /// same ids — no location or server-type controls — then Back. Every
    /// VPS-capable provider is offered: a box bought through a provider no longer
    /// on the curated offer list must still be retirable.
    @ViewBuilder
    private func credentials(_ m: NestRetireMachine, _ snap: RetireSnapshot) -> some View {
        let listing = snap.phase == .listing
        let providers = PROVIDERS.filter { $0.capabilities.contains(.vps) }
        let selectedId = m.selectedProvider()

        ProviderRow(
            providers: providers,
            selectedId: selectedId,
            kind: "vps",
            isEnabled: { _ in !listing },
            onSelect: { vm.selectProvider($0) }
        )

        if let selectedId, let provider = providers.first(where: { $0.id == selectedId }) {
            VStack(alignment: .leading, spacing: 8) {
                vpsProviderLinkAndHelp(provider: provider)

                CredentialsForm(
                    fields: m.visibleFields(),
                    kind: "vps",
                    getCred: { vm.creds[$0] ?? "" },
                    setCred: { vm.setCred($0, $1) },
                    hostedAuthCanBegin: { m.hostedAuthCanBegin(fieldId: $0) },
                    hostedAuthState: { m.hostedAuthState(fieldId: $0) },
                    hostedAuthBegin: { await vm.hostedAuthBegin($0) },
                    hostedAuthWait: { await vm.hostedAuthWait($0) }
                )
                .disabled(listing)

                HStack {
                    Button(L.provisioning.verifyCredentials) { vm.verify() }
                        .buttonStyle(.bordered)
                        .disabled(!m.canVerify() || listing)
                        .accessibilityIdentifier(Ids.vpsVerifyButton)
                        .automationActivate(
                            Ids.vpsVerifyButton,
                            isEnabled: { m.canVerify() && !listing }
                        ) { vm.verify() }
                    if listing { ProgressView().controlSize(.small) }
                }
            }
        }
        backButton()
    }

    // MARK: list

    /// One row per listed server, its members scoped inside `retire-server-item[n]`
    /// (the scoped-descendant idiom, e2e convention 1).
    @ViewBuilder
    private func list(_ m: NestRetireMachine, _ snap: RetireSnapshot) -> some View {
        if snap.servers.isEmpty {
            automationText(Ids.retireServerEmptyMessage, L.onboarding.retire.emptyMessage)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            Text(L.onboarding.retire.listLabel).foregroundStyle(.secondary)
        }
        VStack(spacing: 8) {
            ForEach(Array(snap.servers.enumerated()), id: \.element.serverId) { index, row in
                serverRow(m, row, index: index, selected: snap.selected == row.serverId)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.retireServerList)
        .automationValue(Ids.retireServerList, text: { "\(snap.servers.count) servers" })

        Button(L.onboarding.retire.deleteButton) { vm.beginConfirm() }
            .buttonStyle(.borderedProminent)
            .disabled(snap.selected == nil)
            .accessibilityIdentifier(Ids.retireDeleteButton)
            .automationActivate(
                Ids.retireDeleteButton,
                isEnabled: { m.snapshot().selected != nil }
            ) { vm.beginConfirm() }
        backButton()
    }

    @ViewBuilder
    private func serverRow(
        _ m: NestRetireMachine, _ row: ManagedServerRow, index: Int, selected: Bool
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                    .foregroundStyle(selected ? Color.accentColor : Color.secondary)
                automationText(Ids.retireServerName, row.name).font(.headline)
            }
            VStack(alignment: .leading, spacing: 2) {
                if let domain = row.domain {
                    fact(Ids.retireServerDomain, L.onboarding.retire.domainLabel, domain)
                }
                if !row.secondaryDomains.isEmpty {
                    fact(
                        Ids.retireServerSecondaryDomains,
                        L.onboarding.retire.secondaryDomainsLabel,
                        row.secondaryDomains.joined(separator: ", "))
                }
                if let ipv4 = row.ipv4 {
                    fact(Ids.retireServerAddress, L.onboarding.retire.addressLabel, ipv4)
                }
                if row.current {
                    automationText(Ids.retireServerCurrentBadge, L.onboarding.retire.currentBadge)
                        .font(.caption.bold())
                        .foregroundStyle(Color.accentColor)
                }
                if !row.marked {
                    automationText(Ids.retireServerUnmarkedNote, L.onboarding.retire.unmarkedNote)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.leading, 28)
            if selected { transferCode(m, row).padding(.leading, 28) }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(selected ? Color.accentColor.opacity(0.12) : Color.clear)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .contentShape(Rectangle())
        .onTapGesture { vm.select(row.serverId) }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.retireServerItem)
        .automationActivate(
            Ids.retireServerItem,
            value: { selected ? "on" : "off" }
        ) { vm.select(row.serverId) }
        // OUTERMOST, so the members register INSIDE the scope (the
        // `account-switcher-item` idiom) and `retire-server-item[n]` addresses
        // this row by its declared position.
        .automationScope(Ids.retireServerItem, index: index)
    }

    private func fact(_ id: String, _ label: String, _ value: String) -> some View {
        HStack(spacing: 4) {
            Text("\(label):").font(.caption).foregroundStyle(.secondary)
            automationText(id, value).font(.caption)
        }
    }

    /// The transfer-code affordance on the selected row — independent of deleting.
    @ViewBuilder
    private func transferCode(_ m: NestRetireMachine, _ row: ManagedServerRow) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            switch row.transferCode {
            case .idle:
                Button(L.onboarding.retire.transferCodeButton) { vm.fetchTransferCode() }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier(Ids.retireTransferCodeButton)
                    .automationActivate(Ids.retireTransferCodeButton) { vm.fetchTransferCode() }
            case .code(let code):
                HStack {
                    Text("\(L.onboarding.retire.transferCodeLabel):").font(.caption)
                    automationText(Ids.retireTransferCodeValue, code)
                        .font(.system(.body, design: .monospaced))
                    CopyButton(Ids.retireTransferCodeCopyButton, text: code)
                }
            case .unsupported, .fetching, .availableAfter:
                EmptyView()
            }
            if let note = m.transferCodeNote(serverId: row.serverId) {
                automationText(Ids.retireTransferCodeStatus, renderLocalizedText(note))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    // MARK: confirm

    /// The typed-name gate (`nest-retirement.md` § Confirm shape).
    @ViewBuilder
    private func confirm(_ m: NestRetireMachine, _ snap: RetireSnapshot) -> some View {
        // The summary's provider rides as its display KEY, so it is resolved nested.
        let summary = m.confirmSummary().map(renderLocalizedTextNested).joined(separator: "\n")
        automationText(Ids.retireConfirmSummary, summary)
            .fixedSize(horizontal: false, vertical: true)

        let typed = Binding(
            get: { vm.confirmTyped },
            set: { vm.setConfirmName($0) }
        )
        LabeledContent(L.onboarding.retire.confirmNameLabel) {
            TextField("", text: typed)
                .accessibilityIdentifier(Ids.retireConfirmNameInput)
                .automationField(Ids.retireConfirmNameInput, text: typed)
        }

        Button(L.onboarding.retire.confirmButton, role: .destructive) { vm.confirm() }
            .buttonStyle(.borderedProminent)
            .disabled(!snap.confirmEnabled)
            .accessibilityIdentifier(Ids.retireConfirmButton)
            .automationActivate(
                Ids.retireConfirmButton,
                isEnabled: { m.snapshot().confirmEnabled }
            ) { vm.confirm() }
        Button(L.onboarding.retire.cancelButton) { vm.cancelConfirm() }
            .accessibilityIdentifier(Ids.retireCancelButton)
            .automationActivate(Ids.retireCancelButton) { vm.cancelConfirm() }
    }

    // MARK: running / done

    /// The two step rows (DNS = 0, Server = 1), reusing `provisioning-step-row`'s
    /// ids and the glyphs the provisioning page paints for the shared status.
    private func stepRows(_ m: NestRetireMachine, _ snap: RetireSnapshot) -> some View {
        VStack(spacing: 8) {
            ForEach(Array(snap.steps.enumerated()), id: \.offset) { index, row in
                stepRow(m, row).automationScope(Ids.provisioningStepRow, index: index)
            }
        }
    }

    private func stepRow(_ m: NestRetireMachine, _ row: RetireStepRow) -> some View {
        let label = renderLocalizedText(m.stepLabel(step: row.step))
        return HStack(alignment: .top, spacing: 8) {
            automationText(
                Ids.provisioningStepCheckbox,
                provisioningStatusGlyph(status: retireStepStatus(state: row.state))
            )
            .font(.system(.body, design: .monospaced))
            .frame(width: 18, alignment: .center)

            VStack(alignment: .leading, spacing: 2) {
                automationText(Ids.provisioningStepLabel, label)
                if let note = m.stepNote(state: row.state) {
                    automationText(Ids.provisioningSubstep, renderLocalizedText(note))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if case .failed(let cause) = row.state {
                    automationText(Ids.provisioningStepError, cause)
                        .font(.caption)
                        .foregroundColor(.red)
                }
            }
            Spacer()
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.provisioningStepRow)
        .automationValue(Ids.provisioningStepRow, text: { label })
    }

    @ViewBuilder
    private func running(_ m: NestRetireMachine, _ snap: RetireSnapshot) -> some View {
        stepRows(m, snap)
        let failed = snap.steps.contains { if case .failed = $0.state { true } else { false } }
        if failed {
            Button(L.onboarding.retire.retryButton) { vm.retry() }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier(Ids.retireRetryButton)
                .automationActivate(Ids.retireRetryButton) { vm.retry() }
        }
        if snap.forceServerOffered {
            // The dangling-record warning, repeated where the choice is made.
            automationText(Ids.retireForceServerWarning, L.onboarding.retire.forceServerWarning)
                .font(.caption)
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
            Button(L.onboarding.retire.forceServerButton, role: .destructive) { vm.forceServer() }
                .accessibilityIdentifier(Ids.retireForceServerButton)
                .automationActivate(Ids.retireForceServerButton) { vm.forceServer() }
        }
        // Leaving is offered only once the run has stopped on a failure — never
        // while a step is still in flight.
        if failed { backButton() }
    }

    @ViewBuilder
    private func done(_ m: NestRetireMachine, _ snap: RetireSnapshot) -> some View {
        if let retired = snap.retired {
            Text(L.onboarding.retire.doneDeleted(name: retired.name)).font(.headline)
        }
        stepRows(m, snap)
        let leftover = m.leftoverLines().map(renderLocalizedText).joined(separator: "\n")
        automationText(Ids.retireDnsLeftoverText, leftover)
            .fixedSize(horizontal: false, vertical: true)
        HStack {
            Text(L.onboarding.retire.leftoverCopy).font(.caption).foregroundStyle(.secondary)
            CopyButton(Ids.retireDnsLeftoverCopyButton, text: leftover)
        }
        Button(L.onboarding.retire.doneButton) { exit() }
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier(Ids.retireDoneButton)
            .automationActivate(Ids.retireDoneButton) { exit() }
    }
}
#endif
