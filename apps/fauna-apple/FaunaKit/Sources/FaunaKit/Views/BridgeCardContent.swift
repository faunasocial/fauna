import SwiftUI

/// One bridge's card content (identity + settings + unlink, or the metadata-
/// driven link form) — extracted from the near-duplicate logic that used to
/// live separately in iOS `BridgesView` and macOS `BridgesSettingsView`
/// (mirrors linux's own `build_bridge_detail_content` extraction).
/// Shared by all THREE consumers: the two platforms' unified Bridges page, and
/// the AT Protocol settings page's Linked-account panel
/// (`docs/goal/ui/atproto.md` § Layout & flow — "the existing `bridge-link-form`
/// / `bridge-card` components, reused verbatim"). No new element IDs; `used_in`
/// on those ui.yaml components gains `bluesky`.
///
/// `isDesktop` controls the one real behavioral divergence (macOS's link-mode
/// filter also accepts `platform == "desktop"` modes, matching the existing
/// `BridgesSettingsView` behavior unchanged); `compact` controls purely
/// cosmetic control sizing macOS used and iOS didn't.
public struct BridgeCardContent: View {
    let bridge: BridgeInfo
    let vm: BridgeManagerVM
    let isDesktop: Bool
    let compact: Bool
    let index: Int

    public init(bridge: BridgeInfo, vm: BridgeManagerVM, isDesktop: Bool, compact: Bool, index: Int) {
        self.bridge = bridge
        self.vm = vm
        self.isDesktop = isDesktop
        self.compact = compact
        self.index = index
    }

    public var body: some View {
        Group {
            if !bridge.available {
                Text(L.bridges.notAvailable(name: bridge.name))
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    if bridge.linked {
                        linkedFields
                    } else {
                        unlinkFields
                    }
                    // A SINGLE, always-present button (never conditionally created/
                    // destroyed) carrying `bridge-action-button` — the same id for
                    // both the link AND unlink action on every other app
                    // (bridges.md § User actions table). Splitting this into two
                    // separate conditionally-mounted buttons (one per branch above,
                    // both tagged with the same literal id) was tried first and hit a
                    // real in-process-registry staleness bug: clicking right after a
                    // linked->unlinked (or reverse) transition could still fire the
                    // OLD branch's stale registration (observed as a spurious
                    // `fauna.bridges.already_linked` from an intended unlink click,
                    // `test_bridges.py --client ios`). One stable view whose
                    // label/action/role read `bridge.linked` fresh each render has no
                    // such window — mirrors the `mail-settings-enabled-toggle`
                    // idiom (one persistent control, not two swapped ones).
                    actionButton
                    if bridge.linked && bridge.supportsFollows {
                        followsContent
                    }
                    sourceAskRows
                }
            }
        }
        // Self-registering `bridge-card` container (ui.yaml: type view, indexed
        // true) — mirrors the `device-card` idiom (`DevicesContent.swift`):
        // `.accessibilityElement(children: .contain)` keeps the container id AND
        // its members' ids both queryable, and `.automationScope` pushes
        // `(bridge-card, index)` so the members above resolve as descendants of
        // `scope="bridge-card[i]"` (e2e-conventions.md convention 1;
        // apple-e2e-automation.md rule 5). Before this the card's comments cited
        // `bridge-card` as its ui.yaml component but registered no element for
        // it — only its members.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.bridgeCard)
        .automationValue(Ids.bridgeCard, text: { bridge.name })
        .automationScope(Ids.bridgeCard, index: index)
    }

    // MARK: - Linked bridge (`bridge-card`)

    @ViewBuilder
    private var linkedFields: some View {
        if let identity = bridge.identity {
            LabeledContent(identity.label) {
                Text(identity.display).font(.caption.monospaced()).textSelection(.enabled)
            }
        }

        ForEach(bridge.settings) { setting in
            if setting.type == "bool" {
                Toggle(setting.label, isOn: Binding(
                    get: { setting.value.boolValue ?? false },
                    set: { newVal in
                        Task { await vm.updateSetting(bridge: bridge, key: setting.key, value: newVal) }
                    }
                ))
            } else if setting.type == "text" {
                LabeledContent(setting.label) {
                    Text(setting.value.stringValue ?? "").font(.caption)
                }
            } else if setting.type == "number" {
                BridgeNumberSettingRow(setting: setting, vm: vm, bridge: bridge)
            }
        }
    }

    // MARK: - Unlinked bridge (`bridge-link-form`)

    /// The modes that apply on this platform — the input to the shared
    /// link-block rule, and the source of the link form's fields.
    ///
    /// The platform string-match is the SHARED rule (lifted 2026-08-15 from the
    /// seven per-app copies — `fauna_client_bridges::mode_applies`); apple
    /// supplies only its canonical name, picked at runtime because FaunaKit
    /// builds both apps. ⚠ This retired the old `"desktop"` alias this filter
    /// used to match on macOS: no provider has ever emitted it (the tree's one
    /// scoped mode is Nostr's `platform: "web"`), and matching it left iOS
    /// answering to NOTHING — a hypothetical `platform: "ios"` mode would have
    /// been dropped by the very app it targets.
    private var applicableModes: [BridgeLinkMode] {
        bridge.linkModes?.filter {
            bridgeModeApplies(modePlatform: $0.platform, platform: isDesktop ? "macos" : "ios")
        } ?? []
    }

    /// Why linking is unavailable (`nil` = it IS), from the shared Rust rule so
    /// apple cannot drift from the other six apps on what a mode-less bridge
    /// means (`bridges.md` § Errors & edge cases).
    private var linkBlock: FfiLinkBlock? {
        bridgeLinkBlock(
            linked: bridge.linked,
            error: bridge.error,
            applicableModes: UInt32(applicableModes.count)
        )
    }

    @ViewBuilder
    private var unlinkFields: some View {
        let modes = applicableModes
        if let block = linkBlock {
            // The nest's own explanation when it sent one — verbatim, never
            // swapped for the localized generic, since it is the only account of
            // what actually went wrong. `automationText` (not a bare
            // `.accessibilityIdentifier`) is what makes this readable by the
            // in-process driver — a bare a11y id is invisible to
            // `AutomationRegistry`, the same class of gap `bridge-link-field-*`
            // above already avoids via `.automationField` (confirmed via
            // `check_apple_automation_registration.py` that this id was
            // registration-baselined, not actually registered, so
            // `wait_for("bridge-link-blocked-reason")` timed out at count=0
            // on both apple targets even though the nest-side override worked).
            automationText(Ids.bridgeLinkBlockedReason, {
                switch block {
                case .providerError(let message): return message
                case .noApplicableMode: return L.bridges.noLinkMethod
                }
            }())
                .foregroundStyle(.secondary)
        } else {
            if modes.count > 1 {
                Picker(L.bridges.linkMethod, selection: Binding(
                    get: { vm.selectedMode[bridge.id] ?? modes.first?.mode ?? "" },
                    set: { vm.selectedMode[bridge.id] = $0 }
                )) {
                    ForEach(modes) { mode in
                        Text(mode.label).tag(mode.mode)
                    }
                }
            }

            let currentMode = modes.first(where: { $0.mode == vm.selectedMode[bridge.id] }) ?? modes.first
            if let mode = currentMode {
                // Metadata-driven link form: each provider-declared field renders
                // generically (no per-bridge hardcode) and registers as
                // `bridge-link-field-{key}` for the in-process driver. A bare
                // `.accessibilityIdentifier` is invisible to the driver, so the
                // typeable `.automationField` (write path) is what makes the field
                // addressable (bridges.md § Element IDs; apple-e2e-automation.md).
                ForEach(mode.fields) { field in
                    let fieldId = "bridge-link-field-\(field.key)"
                    if field.type == "password" {
                        SecureField(field.placeholder ?? field.label,
                                    text: fieldBinding(bridge: bridge.id, key: field.key))
                            .modifier(DesktopTextFieldStyle(isDesktop: isDesktop))
                            .accessibilityIdentifier(fieldId)
                            .automationField(fieldId,
                                             text: fieldBinding(bridge: bridge.id, key: field.key))
                    } else {
                        TextField(field.placeholder ?? field.label,
                                  text: fieldBinding(bridge: bridge.id, key: field.key))
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .modifier(DesktopTextFieldStyle(isDesktop: isDesktop))
                            .accessibilityIdentifier(fieldId)
                            .automationField(fieldId,
                                             text: fieldBinding(bridge: bridge.id, key: field.key))
                    }
                }
            }
        }
    }

    // MARK: - Action button (`bridge-action-button` — one stable view, dual role)

    private var actionButton: some View {
        Button(
            bridge.linked ? L.bridges.unlink(name: bridge.name) : L.bridges.link(name: bridge.name),
            role: bridge.linked ? .destructive : nil,
            action: performAction
        )
        .buttonStyle(.borderedProminent)
        .modifier(CompactControlSize(compact: compact))
        // Unlink is always valid; Link is disabled when no mode applies, so the
        // control is never live-but-inert (it used to swallow the tap in
        // `BridgeManagerVM.link`'s `guard ... else { return }`).
        .disabled(!bridge.linked && linkBlock != nil)
        .accessibilityIdentifier(Ids.bridgeActionButton)
        .automationActivate(
            Ids.bridgeActionButton,
            // Mirrors the `.disabled` predicate above — without it,
            // `/element/enabled` falls back to "exists ⇒ enabled" (the button's
            // real SwiftUI-level `.disabled` sits INSIDE this automation
            // modifier, so `AutomationRegistry`'s ancestors-only folding never
            // sees it), so a blocked Link read back as enabled on both apple
            // targets — worse than a stale test, since it also means
            // the in-process driver's own strict-actuation gate would have let a
            // click through on a live-but-inert control, exactly what
            // `bridges.md` § Errors & edge cases rule 3 forbids.
            isEnabled: { bridge.linked || linkBlock == nil },
            value: { bridge.linked ? "linked" : "unlinked" },
            perform: performAction
        )
        // One view, two kinds — so it passes the SAME discriminant the action
        // uses (`performAction` below), never a class test of its own, and the
        // shared table decides. Both arms are OnlineOnly today; declaring both
        // keeps that a fact the table owns rather than one this view assumes.
        // The link *fields* above are buffer and stay live.
        .faunaGate(bridge.linked ? "fauna.bridges.unlink" : "fauna.bridges.link")
    }

    private func performAction() {
        Task {
            if bridge.linked {
                await vm.unlink(bridge: bridge)
            } else {
                await vm.link(bridge: bridge)
            }
        }
    }

    // MARK: - Follows

    @ViewBuilder
    private var followsContent: some View {
        let bridgeFollows = vm.follows[bridge.id] ?? []

        DisclosureGroup(L.bridges.follows) {
            HStack {
                TextField(L.bridges.idToFollow, text: Binding(get: { vm.followId }, set: { vm.followId = $0 }))
                    .modifier(DesktopTextFieldStyle(isDesktop: isDesktop))
                TextField(L.bridges.petname, text: Binding(get: { vm.followPetname }, set: { vm.followPetname = $0 }))
                    .modifier(DesktopTextFieldStyle(isDesktop: isDesktop))
                    .frame(width: 120)
                Button(L.bridges.addFollow) {
                    Task { await vm.addFollow(bridgeId: bridge.id) }
                }
                .modifier(CompactControlSize(compact: compact))
                .disabled(vm.followId.isEmpty)
                .accessibilityIdentifier(Ids.bridgeAddFollowButton)
                .automationActivate(Ids.bridgeAddFollowButton,
                                    isEnabled: { !vm.followId.isEmpty }) {
                    Task { await vm.addFollow(bridgeId: bridge.id) }
                }
            }

            ForEach(bridgeFollows) { follow in
                HStack {
                    Text(shortId(hex: follow.id))
                        .font(.caption.monospaced())
                    if let petname = follow.petname {
                        Text("(\(petname))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button(L.bridges.removeFollow, role: .destructive) {
                        Task { await vm.removeFollow(bridgeId: bridge.id, followId: follow.id) }
                    }
                    .modifier(MiniControlSize(compact: compact))
                    .accessibilityIdentifier(Ids.bridgeFollowRemove)
                    .automationActivate(Ids.bridgeFollowRemove) {
                        Task { await vm.removeFollow(bridgeId: bridge.id, followId: follow.id) }
                    }
                }
                .accessibilityIdentifier(Ids.bridgeFollowItem)
                .automationValue(Ids.bridgeFollowItem, text: { follow.id })
            }
        }
        .accessibilityIdentifier(Ids.bridgeFollowsList)
        .automationValue(Ids.bridgeFollowsList, text: { "" })
    }

    // MARK: - The ward's feed-source ask (`family-safety.md` § Feed-source approvals)

    /// The ward's ask for a source their `feed_sources = "block"` policy just refused
    /// — under the card it refused, never on a page-level banner (the refusal itself
    /// stays on `error-message`). Pushes nothing at all in the common case, which is
    /// the whole design: it paints only where the guardian gate has actually bitten.
    ///
    /// Durable rows first (the store's list survives navigation and a restart, and is
    /// what makes the state honest on a fresh session that never saw the refusal),
    /// then the session's refusals that have no answer yet. An APPROVED ask paints
    /// "Approved — try again": a prompt to retry, never a retry — the grant is
    /// single-use, so an auto-retry would spend it on a render the user did not ask
    /// for, and a lapsed grant would then read as a silent failure.
    @ViewBuilder
    private var sourceAskRows: some View {
        ForEach(Array(vm.sourceAskStates(bridgeId: bridge.id).enumerated()), id: \.offset) { _, state in
            automationText(
                Ids.bridgeSourceRequestState,
                state == .approved ? L.bridges.sourceRequestApproved : L.bridges.sourceRequestPending
            )
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        ForEach(vm.sourceAskOffers(bridgeId: bridge.id), id: \.self) { source in
            Button(L.bridges.sourceRequestButton) {
                Task { await vm.requestSource(source, label: bridge.name) }
            }
            .modifier(CompactControlSize(compact: compact))
            .accessibilityIdentifier(Ids.bridgeSourceRequestButton)
            .automationActivate(Ids.bridgeSourceRequestButton) {
                Task { await vm.requestSource(source, label: bridge.name) }
            }
        }
    }

    private func fieldBinding(bridge: String, key: String) -> Binding<String> {
        Binding(
            get: { vm.linkFields[bridge]?[key] ?? "" },
            set: {
                if vm.linkFields[bridge] == nil { vm.linkFields[bridge] = [:] }
                vm.linkFields[bridge]?[key] = $0
            }
        )
    }
}

/// macOS's explicit `.roundedBorder` text-field style (iOS keeps the system
/// default it always rendered with — preserves the pre-extraction look exactly).
private struct DesktopTextFieldStyle: ViewModifier {
    let isDesktop: Bool
    func body(content: Content) -> some View {
        if isDesktop {
            content.textFieldStyle(.roundedBorder)
        } else {
            content
        }
    }
}

private struct CompactControlSize: ViewModifier {
    let compact: Bool
    func body(content: Content) -> some View {
        if compact {
            content.controlSize(.small)
        } else {
            content
        }
    }
}

private struct MiniControlSize: ViewModifier {
    let compact: Bool
    func body(content: Content) -> some View {
        if compact {
            content.controlSize(.mini)
        } else {
            content
        }
    }
}

/// A `number` `BridgeSetting` row (bridge search-policy settings'
/// `limit_posts_in_search`, the first `number`-typed row): type-then-commit,
/// matching tui's `folder-member-cap-input` idiom and android's
/// `OutlinedTextField` — a native keyboard has no keystroke-debounce concept,
/// so committing on submit IS this control's auto-save analogue, same
/// non-optimistic shape as every other setting row (the buffer clears on
/// commit; the post-refresh repaint owns the display). `parseCountI64` is the
/// shared `fauna_core::format` parser android already uses for this same field.
private struct BridgeNumberSettingRow: View {
    let setting: BridgeSetting
    let vm: BridgeManagerVM
    let bridge: BridgeInfo
    @State private var text = ""

    var body: some View {
        LabeledContent(setting.label) {
            TextField("", text: $text)
                #if os(iOS)
                .keyboardType(.numberPad)
                #endif
                .multilineTextAlignment(.trailing)
                .onSubmit(commit)
        }
        // Re-syncs the buffer only when the COMMITTED value changes (post-
        // refresh) — `.task(id:)`, not `.onAppear`, so in-progress typed text
        // survives an unrelated re-render (AnyCodable/BridgeSetting aren't
        // Equatable, so `.onChange(of:)` isn't available here).
        .task(id: setting.value.intValue) {
            text = setting.value.intValue.map(String.init) ?? ""
        }
    }

    private func commit() {
        guard let parsed = parseCountI64(input: text) else { return }
        Task { await vm.updateSetting(bridge: bridge, key: setting.key, value: parsed) }
    }
}
