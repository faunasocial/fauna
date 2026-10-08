import SwiftUI

/// The admin **`admin-nest`** page (admin.md § N Nest, ratified 2026-06-04
/// per-page-services redesign), shared by macOS + iOS (one FaunaKit view, thin
/// per-target mount points). A dumb renderer of
/// `AdminNestVM`; no business logic here. Element IDs match
/// `tests/e2e-unified/ui.yaml` `admin-nest` exactly. Reference renderer: linux
/// (`apps/fauna-linux/src/views/admin.rs::build_nest_page`).
///
/// Nest-wide controls: the admin **pairing** toggle
/// (`admin-service-pairing-toggle` / `-pairing-status`), and the **Factory Reset**
/// danger zone (`admin-factory-reset-section` / `-button` + a transient confirm
/// alert `admin-factory-reset-confirm-button`).
///
/// **Factory Reset re-onboard.** Confirm → `fauna.admin.factory_reset(nil)` returns
/// the post-reset claim code (the human never sees it); the view hands it to the
/// platform shell via `onFactoryReset`, which drops the authed session keeping local
/// creds and re-seeds onboarding at claim-code with the code pre-filled, so the
/// just-reset nest is immediately re-claimable (mail-bridge-lifecycle.md § Factory
/// reset; `architecture/nest/common.md` § Client-state recoverability). The
/// re-seed is platform-app-level (macOS `FaunaMacApp.factoryResetReonboard`), so it
/// lives in the shell, not this shared view.
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation
/// stack (iOS), not this page.
public struct AdminNestView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminNestVM()
    @State private var showResetConfirm = false
    /// Edit buffer for the serving-port field — a `String` so it parses on Save with
    /// a u16-range guard (a stray edit never silently writes a bad port). Re-seeded
    /// from `vm.servingPort` whenever the hydrated value changes.
    @State private var servingPortText = ""
    /// Edit buffer for the region-code field — mirrors `servingPortText`'s
    /// shape. The draft mirrors the declaration, so a withdrawal empties the
    /// field rather than leaving the withdrawn code sitting in it looking
    /// declared (web `+page.svelte`'s `regionInput` doc).
    @State private var regionInput = ""
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0
    /// Platform re-onboard hook: handed the post-reset claim code after a confirmed
    /// `factory_reset`. Default no-op (iOS wires its own in Track 3 / standalone
    /// previews); macOS wires `FaunaMacApp.factoryResetReonboard`.
    var onFactoryReset: (String) -> Void = { _ in }

    public init(reloadToken: Int = 0, onFactoryReset: @escaping (String) -> Void = { _ in }) {
        self.reloadToken = reloadToken
        self.onFactoryReset = onFactoryReset
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.adminNestHeading, L.admin.nestPage.title)
                    .font(.title)
                Text(L.admin.nestPage.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                // ── Admin pairing toggle (moved off the removed Services page) ──
                GroupBox {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Toggle(L.admin.servicesPage.pairing, isOn: Binding(
                                get: { vm.pairingEnabled },
                                set: { on in Task { await vm.setPairing(on) } }
                            ))
                            .accessibilityIdentifier(Ids.adminServicePairingToggle)
                            // Toggle actuation: `activate` flips pairing (same
                            // `setPairing` the binding's setter calls); `value`
                            // exposes the on/off state the driver reads (SwiftUI
                            // ignores `.accessibilityValue` on a Toggle, so the
                            // registry carries it).
                            .automationActivate(Ids.adminServicePairingToggle,
                                                isEnabled: { !vm.isBusy },
                                                value: { vm.pairingEnabled ? "on" : "off" }) {
                                Task { await vm.setPairing(!vm.pairingEnabled) }
                            }
                            // The pairing policy is one `services.update` row.
                            .faunaGate("fauna.admin.services.update")
                            Spacer()
                            automationText(Ids.adminServicePairingStatus, vm.pairingStatus)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Text(L.admin.servicesPage.pairingDesc)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .disabled(vm.isBusy)

                // ── Admin-set client-facing API serving port (the symmetric twin
                //    of the admin-calendar CalDAV-port field) ──
                servingPortSection

                // ── NAT-mode control (fauna.setup.nat_mode via the shared
                //    AdminNatModeMachine) ──
                natModeSection

                // ── Read-only host-OS-maintenance status + "restart now" ──
                osMaintenanceSection

                // ── Declared region (admin.md § N Nest → Declared region, row 146) ──
                regionSection

                // ── Deployment-identity rotation ──
                seedRotateSection

                // ── Outside-app sign-in keys — directly after the deployment
                //    identity, the same class of deployment crypto (admin.md
                //    § N Nest; authorization-server.md § The issuer) ──
                oauthSection

                // ── Legal takedown — the legal-compulsion carve-out's admin
                //    trigger (moderation.md § Legal takedown → Invocation
                //    surface). Sits here, after the deployment crypto and
                //    before the danger zone, exactly as ui.yaml's admin-nest
                //    element order declares it ──
                takedownSection

                // ── Reports queue (moderation.md § User-initiated reporting →
                //    *Where it lands*) — directly after the takedown console it
                //    pre-fills, as ui.yaml's admin-nest element order declares ──
                reportsSection

                // ── Factory Reset danger zone (moved off Settings) ──
                factoryResetSection
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
        // Re-seed the edit buffer from the hydrated value (initial + after a save's
        // re-hydrate); a live user/driver edit changes the buffer, not vm.servingPort,
        // so this never clobbers an in-progress edit.
        .onChange(of: vm.servingPort, initial: true) { _, port in
            servingPortText = String(port)
        }
        // Same re-seed shape as servingPortText, off the hydrated declaration
        // rather than a live edit — a save/withdraw's re-hydrate clears the
        // field on withdraw or reflects the new code on declare.
        .onChange(of: vm.regionView?.declared, initial: true) { _, declared in
            regionInput = declared ?? ""
        }
    }

    /// Admin-set client-facing API serving port (`admin-nest-serving-port-*`;
    /// nest/common.md § Serving ports) — the symmetric twin of the admin-calendar
    /// CalDAV-port field. The chosen value applies on the nest's next restart but
    /// reads back at once; the buffer parses on Save with a u16-range guard.
    private var servingPortSection: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline) {
                    Text(L.admin.nestPage.servingPortLabel)
                    Spacer(minLength: 12)
                    TextField("", text: $servingPortText)
                        .multilineTextAlignment(.trailing)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 160)
                        .accessibilityIdentifier(Ids.adminNestServingPortInput)
                        .automationField(Ids.adminNestServingPortInput, text: $servingPortText,
                                         isEnabled: { vm.servingPortEditable })
                }
                Text(L.admin.nestPage.servingPortDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // Router-fronted nest: the external port is fixed by the deployment
                // (served on 443), so the field is read-only and we say why. Plain
                // untagged label — no new ui.yaml ID; the disabled-state contract is
                // observable on the existing `admin-nest-serving-port-input`.
                if vm.frontedByRouter {
                    Text(L.admin.nestPage.servingPortFrontedHint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                HStack {
                    Spacer()
                    Button(L.admin.nestPage.servingPortSave) { saveServingPort() }
                        .disabled(!vm.servingPortEditable)
                        .accessibilityIdentifier(Ids.adminNestServingPortSaveButton)
                        .automationActivate(Ids.adminNestServingPortSaveButton,
                                            isEnabled: { vm.servingPortEditable }) {
                            saveServingPort()
                        }
                        // Editing the buffer is local; committing the port is
                        // `fauna.admin.set_serving_port`, so only Save gates.
                        .faunaGate("fauna.admin.set_serving_port")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .disabled(!vm.servingPortEditable)
    }

    /// Parse the serving-port edit buffer as a u16 in [1, 65535]; an out-of-range /
    /// unparseable value surfaces `serving_port_invalid` on `error-message` and is
    /// NOT dispatched (mirrors the admin-calendar CalDAV-port validation).
    private func saveServingPort() {
        // Shared `fauna_core::format::parse_port` (UniFFI `parsePort`): trims,
        // parses u16, requires 1..=65535 (rejects 0) — one validator for every
        // app (value-formatting.md § Port validation), replacing the inline
        // `UInt16(...) + port >= 1` check.
        guard let port = parsePort(input: servingPortText) else {
            vm.errorMessage = L.admin.nestPage.servingPortInvalid
            return
        }
        Task { await vm.setServingPort(port) }
    }

    /// NAT-mode control (`admin-nest-nat-mode-*`, admin.md § Nest → NAT-mode
    /// control) — the post-onboarding change surface for the axis the wizard's
    /// `nat_mode_choice` page confirms once at claim. Driven by the shared
    /// `AdminNatModeMachine` (the exact seam + signed commit ceremony as the
    /// wizard page); radio labels reuse the `onboarding.natMode` strings
    /// (priority #1/#3 — same concept, same copy). No defer button — navigating
    /// away is the defer (admin.md), mirroring linux/web. Radio idiom mirrors the
    /// shared `PrivacySettingsView` inbox-mode-selector (a plain `Button` with a
    /// circle glyph, not a native Picker/segmented control).
    private var natModeSection: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 6) {
                Text(L.admin.nestPage.natModeLabel)
                    .fontWeight(.medium)
                natModeRadio(mode: .public, label: L.onboarding.natMode.publicLabel,
                             desc: L.onboarding.natMode.publicDesc)
                natModeRadio(mode: .private, label: L.onboarding.natMode.privateLabel,
                             desc: L.onboarding.natMode.privateDesc)
                HStack {
                    automationText(Ids.adminNestNatModeStatus, vm.natStatusText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button(L.admin.nestPage.natModeSave) { saveNatMode() }
                        .disabled(!vm.natSaveEnabled)
                        .accessibilityIdentifier(Ids.adminNestNatModeSaveButton)
                        .automationActivate(Ids.adminNestNatModeSaveButton,
                                            isEnabled: { vm.natSaveEnabled }) {
                            saveNatMode()
                        }
                        // Selecting a radio is local; the signed commit is
                        // `fauna.setup.nat_mode` (`AdminNatModeMachine::submit`).
                        .faunaGate("fauna.setup.nat_mode")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// One NAT-mode radio row (`admin-nest-nat-mode-{public,private}-radio`).
    private func natModeRadio(mode: NodeMode, label: String, desc: String) -> some View {
        let id = mode == .public ? "admin-nest-nat-mode-public-radio" : "admin-nest-nat-mode-private-radio"
        return Button(action: { vm.selectNatMode(mode) }) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: vm.natMode == mode ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(vm.natMode == mode ? Color.accentColor : Color.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(label).foregroundStyle(.primary)
                    Text(desc)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(vm.natInFlight)
        .accessibilityIdentifier(id)
        .automationActivate(id, isEnabled: { !vm.natInFlight }) {
            vm.selectNatMode(mode)
        }
    }

    /// Save action, shared by the Button and its `automationActivate` sibling
    /// (apple-e2e-automation.md § Resolved design point — same-symbol convention).
    private func saveNatMode() {
        Task { await vm.saveNatMode() }
    }

    /// Read-only host-OS-maintenance status (installers/vps.md § Host OS Maintenance
    /// § 4) — the patch/reboot state of an onboarded VPS's host Ubuntu box, read off
    /// the `os_*` fields on `fauna.setup.status`. The status line is always present;
    /// the raw count badge renders only when `os_security_updates_pending > 0`; the
    /// "restart now" button renders only when `os_reboot_pending` and expedites the
    /// nest-coordinated idle reboot via `fauna.admin.request_host_restart` (rejected
    /// `no_host` on a nest with no maintenance mount → `error-message`). Mirrors web
    /// `routes/admin/nest/+page.svelte`. No container `accessibilityIdentifier`
    /// (each leaf carries its own id — the bare-container-id-clobbers-children guard).
    private var osMaintenanceSection: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline) {
                    automationText(Ids.nestOsMaintenanceStatus, vm.osMaintenanceStatusText)
                        .font(.callout)
                    // Raw integer count, split out of the categorical status line so
                    // tests assert the number without parsing the localized sentence.
                    if vm.osSecurityUpdates > 0 {
                        automationText(Ids.nestOsUpdatesCount, String(vm.osSecurityUpdates))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    if vm.osRebootPending {
                        Button(L.admin.nestPage.osRestartNow) {
                            Task { await vm.restartNow() }
                        }
                        .accessibilityIdentifier(Ids.nestOsRestartNowButton)
                        .automationActivate(Ids.nestOsRestartNowButton,
                                            isEnabled: { !vm.isBusy }) {
                            Task { await vm.restartNow() }
                        }
                        // Expediting the host reboot is nest-coordinated.
                        .faunaGate("fauna.admin.request_host_restart")
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .disabled(vm.isBusy)
    }

    /// Declared region (`admin-nest-region-*`) — the deployment's legal
    /// situs, the region tier's one human choice (admin.md § N Nest →
    /// Declared region; region-blocking.md § Region determination). Every
    /// rendering decision is the shared `AdminRegionView` fold (tui
    /// `admin/nest.rs`, the reference leg) — this view paints exactly what
    /// it hands back and wires two buttons; it decides nothing about the
    /// plane. Mirrors linux `views/admin.rs`'s region block / web
    /// `+page.svelte`'s region section.
    ///
    /// ⚠ DECLARED, NEVER DETECTED (ratified 2026-08-10): no detect/prefill
    /// affordance may be added here — `adminParseRegionCode` deliberately
    /// does not even case-fold.
    private var regionSection: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 6) {
                Text(L.admin.nestPage.regionLabel).fontWeight(.medium)
                Text(L.admin.nestPage.regionDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                // A NORMAL state, never an error: a deployment that has never
                // declared is conforming. `nil` (pre-first-hydrate) falls
                // back to the same local string, matching web/android.
                automationText(
                    Ids.adminNestRegionStatus,
                    vm.regionView.map { renderLocalizedText($0.status) } ?? L.admin.nestPage.regionNone
                )
                .font(.callout)
                // Present only while a region is declared: before that there
                // is no authority channel to describe.
                if let authority = vm.regionView?.authority {
                    automationText(Ids.adminNestRegionAuthority, renderLocalizedText(authority))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                // The nest-reported "act when you can" warning, only when the
                // authority channel is unreached — the rules already
                // received stay in force, so this is a caveat, not an outage.
                if let staleness = vm.regionView?.staleness {
                    automationText(Ids.adminNestRegionStaleness, renderLocalizedText(staleness))
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                HStack {
                    TextField(L.admin.nestPage.regionPlaceholder, text: $regionInput)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 160)
                        .disabled(vm.isBusy)
                        .accessibilityIdentifier(Ids.adminNestRegionInput)
                        .automationField(Ids.adminNestRegionInput, text: $regionInput,
                                         isEnabled: { !vm.isBusy })
                    Button(L.admin.nestPage.regionSave) { saveRegion() }
                        .disabled(vm.isBusy)
                        .accessibilityIdentifier(Ids.adminNestRegionSaveButton)
                        .automationActivate(Ids.adminNestRegionSaveButton,
                                            isEnabled: { !vm.isBusy }) {
                            saveRegion()
                        }
                        // Editing the buffer is local; declaring is
                        // `fauna.admin.region.set`.
                        .faunaGate("fauna.admin.region.set")
                    // Shown only while a region is declared. Withdrawing also
                    // retires the previous region's feature-policy document
                    // nest-side.
                    if vm.regionView?.canWithdraw == true {
                        Button(L.admin.nestPage.regionWithdraw) {
                            Task { await vm.setRegion(nil) }
                        }
                        .disabled(vm.isBusy)
                        .accessibilityIdentifier(Ids.adminNestRegionWithdrawButton)
                        .automationActivate(Ids.adminNestRegionWithdrawButton,
                                            isEnabled: { !vm.isBusy }) {
                            Task { await vm.setRegion(nil) }
                        }
                        .faunaGate("fauna.admin.region.set")
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .disabled(vm.isBusy)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminNestRegionSection)
        .automationValue(Ids.adminNestRegionSection, text: { L.admin.nestPage.regionLabel })
    }

    /// Validate the region-code edit buffer client-side via the shared
    /// `adminParseRegionCode` (invalid → `error-message`, no dispatch — the
    /// serving-port shape) before declaring (`fauna.admin.region.set`). The
    /// `Err` this discards is the shared engine's own i18n KEY, not a display
    /// string (mirrors web's `saveRegion`), so this always renders its own
    /// local `region_invalid` message.
    private func saveRegion() {
        do {
            let code = try FaunaFFISwift.adminParseRegionCode(raw: regionInput)
            Task { await vm.setRegion(code) }
        } catch {
            vm.errorMessage = L.admin.nestPage.regionInvalid
        }
    }

    /// Deployment-identity rotation (`admin-nest-seed-rotate-*`, admin.md § N
    /// Nest → Deployment-identity rotation; apps row 245). The confirm reveals
    /// **in-page** rather than as a transient alert (unlike Factory Reset) —
    /// it has to carry indexed roster rows, mirrors linux's in-page `GroupBox`
    /// shape rather than the Factory Reset dialog's. Reference: tui
    /// (`apps/fauna-tui/src/admin/nest.rs`), linux (`views/admin.rs`).
    private var seedRotateSection: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.nestPage.rotateSeedLabel).font(.headline)
                Text(L.admin.nestPage.rotateSeedDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if vm.seedRotateConfirm == nil {
                    Button(L.admin.nestPage.rotateSeedButton, role: .destructive) {
                        Task { await vm.openSeedRotateConfirm() }
                    }
                    .accessibilityIdentifier(Ids.adminNestSeedRotateButton)
                    .automationActivate(Ids.adminNestSeedRotateButton,
                                        isEnabled: { !vm.isBusy }) {
                        Task { await vm.openSeedRotateConfirm() }
                    }
                } else {
                    seedRotateConfirmView
                }

                if let status = vm.seedRotateStatus {
                    automationText(Ids.adminNestSeedRotateStatus, status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminNestSeedRotateSection)
        .automationValue(Ids.adminNestSeedRotateSection,
                         text: { L.admin.nestPage.rotateSeedLabel })
    }

    /// The armed confirm's body — three honest renderings of the roster-read
    /// gap (never an empty list beside a live confirm): `.loading` disables
    /// the confirm with zero rows, `.failed` shows why the roster couldn't
    /// answer, `.ready` lists who inherits and gates the confirm on
    /// `view.canConfirm` (an empty roster is refused too, per the shared
    /// fold — see `blockedReason`).
    @ViewBuilder
    private var seedRotateConfirmView: some View {
        switch vm.seedRotateConfirm {
        case .loading:
            Text(L.admin.nestPage.rotateSeedRosterLoading)
                .font(.caption)
                .foregroundStyle(.secondary)
            seedRotateConfirmButtons(enabled: false)
        case .failed(let message):
            Text(message)
                .font(.caption)
                .foregroundStyle(.red)
            seedRotateConfirmButtons(enabled: false)
        case .ready(let view):
            Text(L.admin.nestPage.rotateSeedConfirmBody)
                .font(.caption)
                .foregroundStyle(.secondary)
            ForEach(Array(view.inheritors.enumerated()), id: \.offset) { index, inheritor in
                // Index baked directly into the registered id (not
                // `.automationScope`, which needs a caller-supplied `scope=`
                // query the cross-app test never sends) — matches windows'
                // `AutomationProperties.SetAutomationId(row,
                // $"…roster-item-{i}")` and linux's `set_test_id`, and the
                // literal `-{n}` shape `ui.yaml` documents for this id.
                automationText("\(Ids.adminNestSeedRotateRosterItem)-\(index)", inheritor.label)
                    .font(.caption)
            }
            if let reason = view.blockedReason {
                automationText(Ids.adminNestSeedRotateRosterReason, renderLocalizedText(reason))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            seedRotateConfirmButtons(enabled: view.canConfirm)
        case nil:
            EmptyView()
        }
    }

    private func seedRotateConfirmButtons(enabled: Bool) -> some View {
        HStack {
            Button(L.admin.nestPage.rotateSeedConfirmButton, role: .destructive) {
                Task { await vm.confirmSeedRotate() }
            }
            .disabled(!enabled)
            .accessibilityIdentifier(Ids.adminNestSeedRotateConfirmButton)
            .automationActivate(Ids.adminNestSeedRotateConfirmButton,
                                isEnabled: { enabled }) {
                Task { await vm.confirmSeedRotate() }
            }
            // Arming is local (the opener above carries no gate); the confirm
            // is the commit — `fauna.admin.deployment_seed.rotate`.
            .faunaGate("fauna.admin.deployment_seed.rotate")
            Button(L.admin.nestPage.rotateSeedCancelButton, role: .cancel) {
                vm.cancelSeedRotateConfirm()
            }
            .accessibilityIdentifier(Ids.adminNestSeedRotateCancelButton)
            .automationActivate(Ids.adminNestSeedRotateCancelButton) {
                vm.cancelSeedRotateConfirm()
            }
        }
    }

    /// Outside-app sign-in keys (`admin-nest-oauth-*`; authorization-server.md
    /// § The issuer → *Two rotation arms*) — the compromise response over the
    /// nest-held OAuth issuer key set and its refresh-token secret: the served
    /// keys, the ordinary rotation (no confirm — nothing breaks, so its cost
    /// is stated beside it), and the two forced arms behind ONE inline confirm
    /// that states the armed arm's cost before dispatch. Paint only: every
    /// sentence is a shared fold, and `OauthSectionState` refuses each gesture
    /// on the same test the `.disabled` flags read. The confirm reveals
    /// in-page, never as a `.sheet`/`.alert` (admin.md § N Nest — the journey
    /// asserts it synchronously after the press). Mirrors tui's
    /// `admin/nest.rs::oauth_elements` element for element.
    private var oauthSection: some View {
        let live = vm.oauth.controlsLive
        return GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.nestPage.oauthLabel).font(.headline)
                Text(L.admin.nestPage.oauthDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                oauthKeyRows

                // The ordinary arm states its cost beside itself: it has no confirm.
                if let view = vm.oauth.answeredView {
                    Text(renderLocalizedText(FaunaFFISwift.issuerKeyRotateCost(view: view)))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                HStack {
                    Button(L.admin.nestPage.oauthRotateButton) { vm.rotateIssuerKey() }
                        .disabled(!live)
                        .accessibilityIdentifier(Ids.adminNestOauthRotateButton)
                        .automationActivate(Ids.adminNestOauthRotateButton,
                                            isEnabled: { vm.oauth.controlsLive }) {
                            vm.rotateIssuerKey()
                        }
                        // The ordinary arm commits on the press, so it declares.
                        .faunaGate("fauna.oauth.rotate_issuer_key")
                    // The two arm buttons dispatch nothing (arming is local) and
                    // declare nothing — the confirm they open commits, and gates.
                    Button(L.admin.nestPage.oauthForceRotateButton, role: .destructive) {
                        vm.armOauthForced(.issuerKey)
                    }
                    .disabled(!live)
                    .accessibilityIdentifier(Ids.adminNestOauthForceRotateButton)
                    .automationActivate(Ids.adminNestOauthForceRotateButton,
                                        isEnabled: { vm.oauth.controlsLive }) {
                        vm.armOauthForced(.issuerKey)
                    }
                    Button(L.admin.nestPage.oauthSecretForceRotateButton, role: .destructive) {
                        vm.armOauthForced(.sessionSecret)
                    }
                    .disabled(!live)
                    .accessibilityIdentifier(Ids.adminNestOauthSecretForceRotateButton)
                    .automationActivate(Ids.adminNestOauthSecretForceRotateButton,
                                        isEnabled: { vm.oauth.controlsLive }) {
                        vm.armOauthForced(.sessionSecret)
                    }
                }

                if let armed = vm.oauth.armed {
                    oauthConfirm(armed)
                }

                // The verdict, its own element — never the page's `error-message`.
                if let status = vm.oauth.status {
                    automationText(Ids.adminNestOauthStatus, status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminNestOauthSection)
        .automationValue(Ids.adminNestOauthSection, text: { L.admin.nestPage.oauthLabel })
    }

    /// The key rows, painted ONLY from an answered read (the seed-rotate
    /// roster's rule) — signer first, in the nest's own order, never
    /// re-sorted; "not asked yet" and "couldn't find out" get the reason line
    /// instead. A replaced key's countdown is the point of its line, so it is
    /// counted against the clock at PAINT, and the timeline repaints it every
    /// 15 s while the page shows (linux's tick).
    @ViewBuilder
    private var oauthKeyRows: some View {
        switch vm.oauth.keys {
        case .ready(let view):
            TimelineView(.periodic(from: .now, by: 15)) { context in
                let nowSecs = Int64(context.date.timeIntervalSince1970)
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(view.keys.enumerated()), id: \.offset) { index, row in
                        // The literal `-{n}` id, not `.automationScope` (which
                        // needs a `scope=` query the cross-app test never
                        // sends) — the seed-rotate roster's shape above.
                        automationText("\(Ids.adminNestOauthKeyItem)-\(index)",
                                       renderLocalizedText(FaunaFFISwift.issuerKeyRowLabel(
                                           row: row, nowSecs: nowSecs)))
                            .font(.callout)
                    }
                }
            }
        case .unread:
            automationText(Ids.adminNestOauthKeyReason, L.admin.nestPage.oauthKeysLoading)
                .font(.caption)
                .foregroundStyle(.secondary)
        case .failed(let reason):
            automationText(Ids.adminNestOauthKeyReason, reason)
                .font(.caption)
                .foregroundStyle(.red)
        }
    }

    /// The armed forced confirm — the CAPTURED fold's cost and label, and a
    /// confirm carrying the arm it was rendered for (`OauthSectionState`
    /// refuses a mismatch).
    private func oauthConfirm(_ armed: OauthArmed) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            automationText(Ids.adminNestOauthConfirmSummary, renderLocalizedText(armed.confirm.summary))
                .font(.callout)
            HStack {
                Button(renderLocalizedText(armed.confirm.confirmLabel), role: .destructive) {
                    vm.confirmOauthForced(armed.arm)
                }
                .accessibilityIdentifier(Ids.adminNestOauthConfirmButton)
                .automationActivate(Ids.adminNestOauthConfirmButton) {
                    vm.confirmOauthForced(armed.arm)
                }
                // Exactly the armed arm's kind — both literals spelled out for
                // the offline-gate-kinds check, the arm choosing between them.
                .faunaGate(armed.arm == .issuerKey
                           ? "fauna.oauth.force_rotate_issuer_key"
                           : "fauna.oauth.force_rotate_session_secret")
                Button(L.admin.nestPage.oauthCancelButton, role: .cancel) {
                    vm.cancelOauthForced()
                }
                .accessibilityIdentifier(Ids.adminNestOauthCancelButton)
                .automationActivate(Ids.adminNestOauthCancelButton) {
                    vm.cancelOauthForced()
                }
            }
        }
        // No container id: ui.yaml declares no confirm container, and the
        // section's `.contain` already keeps these children queryable.
    }

    /// Legal takedown (`admin-nest-takedown-*`; moderation.md § Legal takedown
    /// → *Invocation surface*, ruled 2026-08-16 — the one nest-wide content
    /// removal and never a policy lever). The one-configuration-surface
    /// invariant makes the admin app UI the only sanctioned channel for this
    /// act, so the console is the same two-step shape the seed rotation and
    /// the forced issuer arms use: fill → the arm control the shared fold
    /// gates → an inline confirm that NAMES the content and the citation →
    /// dispatch. Paint only — every sentence, the citation guard and its
    /// restore asymmetry are `fauna_client_moderation::takedown` over UniFFI
    /// (`TakedownSectionState.fold`), so no app grows its own wording.
    /// Mirrors linux's `build_nest_page` takedown block and tui's
    /// `admin/nest.rs` element for element.
    private var takedownSection: some View {
        let view = vm.takedown.fold
        return GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.nestPage.takedownLabel).font(.headline)
                Text(L.admin.nestPage.takedownDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                HStack {
                    Text(L.admin.nestPage.takedownContentIdLabel)
                        .font(.caption)
                    TextField("", text: Binding(
                        get: { vm.takedown.contentId },
                        set: { vm.setTakedownContentId($0) }
                    ))
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.adminNestTakedownContentIdInput)
                    .automationField(Ids.adminNestTakedownContentIdInput,
                                     text: Binding(
                                        get: { vm.takedown.contentId },
                                        set: { vm.setTakedownContentId($0) }
                                     ),
                                     isEnabled: { !vm.takedown.inFlight })
                }

                takedownTypeRadio(conversation: false,
                                  id: Ids.adminNestTakedownTypePostRadio,
                                  label: L.admin.nestPage.takedownTypePost)
                takedownTypeRadio(conversation: true,
                                  id: Ids.adminNestTakedownTypeConversationRadio,
                                  label: L.admin.nestPage.takedownTypeConversation)

                HStack {
                    Text(L.admin.nestPage.takedownReferenceLabel)
                        .font(.caption)
                    TextField("", text: Binding(
                        get: { vm.takedown.reference },
                        set: { vm.setTakedownReference($0) }
                    ))
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.adminNestTakedownReferenceInput)
                    .automationField(Ids.adminNestTakedownReferenceInput,
                                     text: Binding(
                                        get: { vm.takedown.reference },
                                        set: { vm.setTakedownReference($0) }
                                     ),
                                     isEnabled: { !vm.takedown.inFlight })
                }

                // Plain-label Toggle: SwiftUI ignores `.accessibilityValue` on
                // a Toggle, so the on/off the driver reads rides the registry
                // (the pairing toggle's shape).
                Toggle(L.admin.nestPage.takedownRestoreLabel, isOn: Binding(
                    get: { vm.takedown.restore },
                    set: { vm.setTakedownRestore($0) }
                ))
                .accessibilityIdentifier(Ids.adminNestTakedownRestoreCheckbox)
                .automationActivate(Ids.adminNestTakedownRestoreCheckbox,
                                    isEnabled: { !vm.takedown.inFlight },
                                    value: { vm.takedown.restore ? "on" : "off" }) {
                    vm.setTakedownRestore(!vm.takedown.restore)
                }

                // The disabled arm control owes its reason. Decorative — no
                // test id: ui.yaml declares none for it, matching linux's
                // unregistered label and tui's `Element::chrome` twin.
                if let reason = view.blockedReason {
                    Text(renderLocalizedText(reason))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                // Arming is local and dispatches nothing, so it declares no
                // wire kind — the confirm it opens commits, and gates.
                Button(renderLocalizedText(view.armLabel), role: .destructive) {
                    vm.armTakedown()
                }
                .disabled(!vm.takedown.canArm)
                .accessibilityIdentifier(Ids.adminNestTakedownButton)
                .automationActivate(Ids.adminNestTakedownButton,
                                    isEnabled: { vm.takedown.canArm }) {
                    vm.armTakedown()
                }

                if let armed = vm.takedown.armed {
                    takedownConfirm(armed)
                }

                // The verdict, its own element — never the page's
                // `error-message`: a refusal here must not read as "nothing
                // changed", and a success has consequences worth words.
                if let status = vm.takedown.status {
                    automationText(Ids.adminNestTakedownStatus, status)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminNestTakedownSection)
        .automationValue(Ids.adminNestTakedownSection, text: { L.admin.nestPage.takedownLabel })
    }

    /// The open abuse reports (`admin-nest-reports-section`): one flat
    /// `admin-nest-report-item` per row with its three levers. *Open takedown*
    /// (posts and messages only — the shared `can_open_takedown`) pre-fills the
    /// console above with NO citation; *acted* / *dismiss* only RECORD the
    /// outcome (the reporter is told nothing more), over
    /// `fauna.moderation.abuse_report.resolve` — OnlineOnly, so each declares it.
    private var reportsSection: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.nestPage.reportsLabel).font(.headline)
                Text(L.admin.nestPage.reportsDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if !vm.reportsLoaded {
                    Text(L.admin.nestPage.reportsLoading).font(.caption).foregroundStyle(.secondary)
                } else if vm.reports.isEmpty {
                    Text(L.admin.nestPage.reportsEmpty).font(.caption).foregroundStyle(.secondary)
                }
                ForEach(vm.reports, id: \.reportId) { row in
                    reportRow(row)
                }
                if let status = vm.reportsStatus {
                    Text(status).font(.caption).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminNestReportsSection)
        .automationValue(Ids.adminNestReportsSection, text: { L.admin.nestPage.reportsLabel })
    }

    private func reportRow(_ row: FfiReportQueueRow) -> some View {
        let line = AdminNestVM.reportLine(row)
        return VStack(alignment: .leading, spacing: 4) {
            Text(line)
                .font(.caption)
                .accessibilityIdentifier(Ids.adminNestReportItem)
                .automationValue(Ids.adminNestReportItem, text: { line })
            HStack(spacing: 8) {
                if row.canOpenTakedown {
                    Button(L.admin.nestPage.reportsOpenTakedown) { vm.openTakedown(for: row) }
                        .controlSize(.small)
                        .accessibilityIdentifier(Ids.adminNestReportOpenTakedownButton)
                        .automationActivate(Ids.adminNestReportOpenTakedownButton) {
                            vm.openTakedown(for: row)
                        }
                }
                Button(L.admin.nestPage.reportsActed) {
                    Task { await vm.resolveReport(row, acted: true) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminNestReportActedButton)
                .automationActivate(Ids.adminNestReportActedButton) {
                    Task { await vm.resolveReport(row, acted: true) }
                }
                .faunaGate("fauna.moderation.abuse_report.resolve")
                Button(L.admin.nestPage.reportsDismiss) {
                    Task { await vm.resolveReport(row, acted: false) }
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminNestReportDismissButton)
                .automationActivate(Ids.adminNestReportDismissButton) {
                    Task { await vm.resolveReport(row, acted: false) }
                }
                .faunaGate("fauna.moderation.abuse_report.resolve")
            }
        }
    }

    /// One content-kind radio. `post` is the default (the serve-withhold half);
    /// `conversation` selects the MLS relay-withhold kind. Same hand-painted
    /// shape as `natModeRadio` — SwiftUI has no radio primitive, and a Picker
    /// would not give each choice its own ui.yaml id.
    private func takedownTypeRadio(conversation: Bool, id: String, label: String) -> some View {
        let selected = vm.takedown.conversation == conversation
        return Button(action: { vm.setTakedownConversation(conversation) }) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(selected ? Color.accentColor : Color.secondary)
                Text(label).foregroundStyle(.primary)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(vm.takedown.inFlight)
        .accessibilityIdentifier(id)
        .automationActivate(id,
                            isEnabled: { !vm.takedown.inFlight },
                            value: { vm.takedown.conversation == conversation ? "on" : "off" }) {
            vm.setTakedownConversation(conversation)
        }
    }

    /// The armed confirm — the decision surface, rendering the fold CAPTURED
    /// at arm time (`TakedownArmed`), so a field the admin keeps typing after
    /// arming cannot change what the confirm already named. Reveals in-page,
    /// never as a `.sheet`/`.alert`: the journey asserts it synchronously
    /// after the press.
    private func takedownConfirm(_ armed: TakedownArmed) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.adminNestTakedownConfirmSummary, armed.summary)
                .font(.caption)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Button(armed.confirmLabel, role: .destructive) { vm.confirmTakedown() }
                    .accessibilityIdentifier(Ids.adminNestTakedownConfirmButton)
                    .automationActivate(Ids.adminNestTakedownConfirmButton) {
                        vm.confirmTakedown()
                    }
                    // The confirm is the commit — the compulsory act itself.
                    .faunaGate("fauna.moderation.legal_takedown")
                Button(L.admin.nestPage.takedownCancelButton, role: .cancel) {
                    vm.cancelTakedown()
                }
                .accessibilityIdentifier(Ids.adminNestTakedownCancelButton)
                .automationActivate(Ids.adminNestTakedownCancelButton) {
                    vm.cancelTakedown()
                }
            }
        }
        // No container id: ui.yaml declares no confirm container, and the
        // section's `.contain` already keeps these children queryable.
    }

    private var factoryResetSection: some View {
        GroupBox(L.admin.settingsPage.factoryResetSection) {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.settingsPage.factoryResetTitle)
                    .font(.headline)
                Text(L.admin.settingsPage.factoryResetDesc)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button(L.admin.settingsPage.factoryResetButton, role: .destructive) {
                    showResetConfirm = true
                }
                .accessibilityIdentifier(Ids.adminFactoryResetButton)
                .automationActivate(Ids.adminFactoryResetButton,
                                    isEnabled: { !vm.isBusy }) {
                    showResetConfirm = true
                }
                factoryResetConfirm
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminFactoryResetSection)
        // Group anchor read so the driver can locate the danger-zone section.
        .automationValue(Ids.adminFactoryResetSection,
                         text: { L.admin.settingsPage.factoryResetSection })
        .disabled(vm.isBusy)
    }

    /// The destructive factory-reset confirm, as an **inline overlay** rather
    /// than the system `.alert` this section used to raise.
    ///
    /// The alert had the ids on it, so this is not about reach for the driver —
    /// it is about the gate. `.faunaGate` reads `FaunaClient` from the
    /// environment, and an `.alert`'s content builder is a separate presentation
    /// context that does not reliably carry it; a missing client is read as an
    /// UNKNOWN connection word, which ruling 3 answers *available*. So the
    /// declaration sat there looking correct, satisfied the dev-fleet
    /// offline-gate-kinds checker, and could silently never desensitize
    /// anything — a false declaration is worse than none, because it also
    /// stops anyone looking. Inline, the confirm is an ordinary descendant
    /// of the page and the environment simply reaches it.
    ///
    /// Two things follow from the move, both of them the point: the declared
    /// `admin-factory-reset-cancel-button` (ui.yaml:1450) now exists on apple —
    /// it had no id at all inside the alert — and the shape matches web's own
    /// inline confirm (`apps/fauna-web/src/routes/admin/nest/+page.svelte`).
    /// `mail-settings-disable-confirm` is the FaunaKit precedent.
    @ViewBuilder private var factoryResetConfirm: some View {
        if showResetConfirm {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.admin.settingsPage.factoryResetConfirmTitle)
                    .font(.headline)
                Text(L.admin.settingsPage.factoryResetConfirmBody)
                    .font(.callout)
                HStack {
                    Button(L.admin.settingsPage.factoryResetConfirmButton, role: .destructive) {
                        confirmFactoryReset()
                    }
                    .accessibilityIdentifier(Ids.adminFactoryResetConfirmButton)
                    .automationActivate(Ids.adminFactoryResetConfirmButton) {
                        confirmFactoryReset()
                    }
                    // Arming stays local (the opener above is live with no nest);
                    // the reset itself is `fauna.admin.factory_reset`, OnlineOnly.
                    .faunaGate("fauna.admin.factory_reset")
                    Button(L.admin.settingsPage.factoryResetCancel, role: .cancel) {
                        showResetConfirm = false
                    }
                    .accessibilityIdentifier(Ids.adminFactoryResetCancelButton)
                    .automationActivate(Ids.adminFactoryResetCancelButton) {
                        showResetConfirm = false
                    }
                }
            }
            // No container id on this stack on purpose: ui.yaml declares the
            // confirm and cancel buttons but no confirm *container*, and the
            // enclosing `admin-factory-reset-section` GroupBox already carries
            // the `.contain` that keeps these children queryable.
        }
    }

    /// Factory-reset confirm action — shared by the overlay `Button` and
    /// `automationActivate`. Runs the real `factory_reset` and hands the
    /// post-reset claim code to the platform re-onboard hook.
    private func confirmFactoryReset() {
        Task {
            if let code = await vm.factoryReset() { onFactoryReset(code) }
        }
    }
}
