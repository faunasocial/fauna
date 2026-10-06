import SwiftUI

/// The provider-specific expansion of the onboarding **VPS-config** step
/// (`docs/goal/behavior/onboarding.md` §5): signup link + open-in-browser,
/// provider help text, the generated credentials sub-form, a verify button,
/// the mail-mode toggle, and the conditionally-shown server-type / location
/// pickers. macOS (`MacVpsConfigView`) and iOS (`VpsConfigView`) rendered this
/// near-identically; lifted 2026-09-02 to one shared FaunaKit definition, mirroring `dnsProviderSection`'s
/// existing lift of the same shape.
///
/// **Free `@ViewBuilder` function, not a `struct … : View`** — same reason as
/// `dnsProviderSection` (see its doc comment): the body reads machine
/// snapshots via `vm.machine.<getter>()`, untracked `let` accesses `@Observable`
/// does NOT subscribe to. Inlined into the caller's `body` (which already
/// subscribes via a tracked read such as `vm.errorMessage`), it re-evaluates on
/// every machine change for free.
///
/// The server-type radio and location picker keep genuinely divergent
/// per-platform controls (SwiftUI has no native macOS RadioGroup, so macOS
/// uses a checkbox-styled `Toggle` set where iOS uses tappable rows; macOS
/// uses a native `Picker` where iOS uses a `Menu`) — real platform-idiom
/// differences, not accidental duplication.
@ViewBuilder
public func vpsProviderSection(vm: OnboardingVM, provider: ProviderMeta) -> some View {
    VStack(alignment: .leading, spacing: 8) {
        if let url = URL(string: provider.signupUrl) {
            Link(provider.signupUrl, destination: url)
                .accessibilityIdentifier(Ids.vpsProviderLink)
                .automationValue(Ids.vpsProviderLink, text: { provider.signupUrl })
            Button(L.onboarding.dnsConfig.openInBrowser) { OpenURL.open(url) }
                .accessibilityIdentifier(Ids.vpsProviderOpenBrowserButton)
                .automationActivate(Ids.vpsProviderOpenBrowserButton) { OpenURL.open(url) }
        }
        automationText(Ids.vpsProviderHelpText, L.lookup(provider.helpKey))

        CredentialsForm(
            fields: provider.visibleVpsFields,
            kind: "vps",
            getCred: { vm.machine.vpsConfig().creds[$0] ?? "" },
            setCred: { vm.machine.setVpsCred(fieldId: $0, value: $1) },
            machine: vm.machine,
            form: .vps
        )

        Button(L.provisioning.verifyCredentials) {
            Task { try? await vm.machine.verifyVps() }
        }
        .buttonStyle(.bordered)
        .disabled(!vm.machine.canVerifyVps())
        .accessibilityIdentifier(Ids.vpsVerifyButton)
        .automationActivate(Ids.vpsVerifyButton,
                            isEnabled: { vm.machine.canVerifyVps() }) {
            Task { try? await vm.machine.verifyVps() }
        }

        let cfg = vm.machine.vpsConfig()
        if !cfg.serverTypes.isEmpty {
            vpsMailModeToggle(vm: vm)
            vpsServerTypePicker(vm: vm, cfg: cfg)
        }
        if !cfg.locations.isEmpty {
            vpsLocationPicker(vm: vm, cfg: cfg)
        }
    }
}

/// Mail-vs-social mode (onboarding.md §5). Default-checked from the shared
/// machine (`provisionMailModeEnabled()` = mail ON iff the handle targets a
/// real domain); turning it OFF unlocks the 1 GB tier in the radio above.
/// `setProvisionMailMode` clears a now-too-small selection in shared Rust.
@ViewBuilder
private func vpsMailModeToggle(vm: OnboardingVM) -> some View {
    vpsCheckboxOption(
        id: "vps-config-mail-mode-toggle",
        checked: vm.machine.provisionMailModeEnabled(),
        label: L.onboarding.vpsConfig.mailModeLabel,
        detail: L.onboarding.vpsConfig.mailModeDesc,
        enabled: true
    ) { vm.machine.setProvisionMailMode(enabled: !vm.machine.provisionMailModeEnabled()) }
}

/// A deployment-toggle checkbox row (a tappable label + caption with a
/// leading square glyph).
@ViewBuilder
private func vpsCheckboxOption(
    id: String,
    checked: Bool,
    label: String,
    detail: String,
    enabled: Bool,
    action: @escaping () -> Void
) -> some View {
    Button(action: action) {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: checked ? "checkmark.square.fill" : "square")
                .foregroundStyle(checked ? Color.accentColor : Color.secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(label).foregroundStyle(.primary)
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
        }
        .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .disabled(!enabled)
    .accessibilityIdentifier(id)
    .automationActivate(id) { action() }
}

@ViewBuilder
private func vpsServerTypePicker(vm: OnboardingVM, cfg: VpsConfigState) -> some View {
    VStack(alignment: .leading) {
        Text(L.onboarding.vpsConfig.serverTypeRadioLegend).font(.headline)
        // Filter THEN index so `vps-server-type-radio[i]` stays 0-based over
        // the *shown* set (mail ON ⇒ mem_gb ≥ 2; the gate is shared Rust).
        // Key by `\.offset` (the dominant apple ForEach convention) AND pin
        // each row's identity to `[idx]:server-type` via `.id(...)`. The mail
        // toggle *prepends* the 1 GB tier, shifting every row's positional id;
        // since `_AutomationRegister` only (un)registers on `.onAppear`/
        // `.onDisappear` (view-identity changes), a row kept mounted across the
        // re-filter would otherwise keep its STALE `[0]` id — so
        // `vps-server-type-radio[1]` never registers in-process. Pinning
        // identity to (position, server-type) forces a teardown+rebuild
        // whenever either changes, re-registering the correct positional id
        // and a fresh `select(st.id)` closure.
        ForEach(Array(cfg.serverTypes.filter { serverTypeAllowedForMail(st: $0, enableMail: vm.machine.provisionMailModeEnabled()) }.prefix(5).enumerated()), id: \.offset) { idx, st in
            #if os(iOS)
            HStack {
                Image(systemName: cfg.selectedServerTypeId == st.id ? "circle.inset.filled" : "circle")
                Text(serverTypeLabel(st: st))
                Spacer()
            }
            .contentShape(Rectangle())
            .onTapGesture { vm.machine.selectVpsServerType(id: st.id) }
            .accessibilityIdentifier("vps-server-type-radio[\(idx)]")
            .accessibilityAddTraits(cfg.selectedServerTypeId == st.id ? .isSelected : [])
            // A radio "click" selects this server type (single-select set).
            .automationActivate("vps-server-type-radio[\(idx)]") {
                vm.machine.selectVpsServerType(id: st.id)
            }
            .id("vps-server-type-radio[\(idx)]:\(st.id)")
            #else
            Toggle(isOn: Binding(
                get: { cfg.selectedServerTypeId == st.id },
                set: { _ in vm.machine.selectVpsServerType(id: st.id) }
            )) {
                Text(serverTypeLabel(st: st))
            }
            // SwiftUI macOS lacks a native RadioGroup; toggleStyle .checkbox
            // + manual single-select is the conventional substitute. Each
            // row keeps its indexed identifier so the test layer treats
            // the group as a radio set.
            .toggleStyle(.checkbox)
            .accessibilityIdentifier("vps-server-type-radio[\(idx)]")
            // A radio "click" selects this server type (single-select set).
            .automationActivate("vps-server-type-radio[\(idx)]") {
                vm.machine.selectVpsServerType(id: st.id)
            }
            .id("vps-server-type-radio[\(idx)]:\(st.id)")
            #endif
        }
    }
}

@ViewBuilder
private func vpsLocationPicker(vm: OnboardingVM, cfg: VpsConfigState) -> some View {
    // The Menu's per-item Buttons don't register in-process (system menu);
    // the select routes through the same machine setter the items call.
    // Read/write the location's DISPLAY NAME, not its id — the cross-app
    // `vps-location-picker` contract is the visible name (linux's gtk::DropDown),
    // and it's the label shown above. The machine still works in ids; map both
    // directions here (both branches share this exact `automationSelect` call —
    // duplicated per-branch, not chained after `#endif`, because a leading-dot
    // continuation across a preprocessor boundary loses its concrete-type
    // context inside a `@ViewBuilder` free function).
    #if os(iOS)
    let currentLocationName = cfg.locations.first(where: { $0.id == cfg.selectedLocationId })?.name
        ?? cfg.locations.first?.name
        ?? ""
    Menu {
        ForEach(cfg.locations, id: \.id) { loc in
            Button(loc.name) { vm.machine.selectVpsLocation(id: loc.id) }
                .accessibilityIdentifier("vps-location-picker[\(loc.id)]")
        }
    } label: {
        Text(currentLocationName)
    }
    .accessibilityIdentifier(Ids.vpsLocationPicker)
    .automationSelect(Ids.vpsLocationPicker,
                      value: {
                          let cfg = vm.machine.vpsConfig()
                          return cfg.locations.first { $0.id == cfg.selectedLocationId }?.name
                      },
                      set: { wire in
                          let cfg = vm.machine.vpsConfig()
                          guard let id = cfg.locations.first(where: { $0.name == wire })?.id
                          else { return }
                          vm.machine.selectVpsLocation(id: id)
                      })
    #else
    Picker("", selection: Binding(
        get: { cfg.selectedLocationId ?? "" },
        set: { vm.machine.selectVpsLocation(id: $0) }
    )) {
        ForEach(cfg.locations, id: \.id) { loc in
            Text(loc.name)
                .tag(loc.id)
                .accessibilityIdentifier("vps-location-picker[\(loc.id)]")
        }
    }
    .accessibilityIdentifier(Ids.vpsLocationPicker)
    .automationSelect(Ids.vpsLocationPicker,
                      value: {
                          let cfg = vm.machine.vpsConfig()
                          return cfg.locations.first { $0.id == cfg.selectedLocationId }?.name
                      },
                      set: { wire in
                          let cfg = vm.machine.vpsConfig()
                          guard let id = cfg.locations.first(where: { $0.name == wire })?.id
                          else { return }
                          vm.machine.selectVpsLocation(id: id)
                      })
    #endif
}
