import SwiftUI

/// The standalone **Nostr** settings page (`docs/goal/ui/nostr.md` § Layout & flow;
/// page structure ratified 2026-06-13 (user): Nostr keeps its own dedicated page —
/// the same treatment as mail — and is **not** folded into the unified Bridges page
/// (`bridges.md` § Scope)). Shared by **both** Apple targets, one FaunaKit view with
/// thin per-target mount points (priority #1/#2): macOS renders it as a Settings
/// rail sub-page (`SettingsShellView` → `SettingsPage.nostr`), iOS as a Settings
/// sub-page push (`SettingsView.settingsDestination(.nostr)`). A dumb renderer over
/// `NostrVM`; all Nostr logic is shared Rust (`libs/fauna-bridge-nostr`) reached via
/// the unified `fauna.bridges.*` control plane that the `APIClient` Nostr methods
/// adapt (`nostr.md` § WS-RPC migration contract — `bridge_id:"nostr"`). Element IDs
/// match `tests/e2e-unified/ui.yaml` `nostr` exactly. Reference render: linux
/// (`apps/fauna-linux/src/settings/nostr_tab.rs`), web.
///
/// Scope covers account linking (link mode + nsec/bunker import, link/unlink,
/// pubkey copy), the 5 content-publishing toggles, **relay management**
/// (`nostr-relay-*` — `NostrVM.addRelay`/`removeRelay` read-modify-write the
/// `relay_list` setting, a JSON array of relay URLs, over `fauna.bridges.set_settings`;
/// the nest republishes NIP-65), and follows. (This is what was previously rendered
/// inside `BridgesView` with non-canonical `bridge-*` ids; lifted here with the
/// canonical `nostr-*` ids.) DMs are NOT on this page — a Nostr DM is a bridged
/// room on the unified Conversations surface.
public struct NostrSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = NostrVM()

    public init() {}

    public var body: some View {
        // Eager `ScrollView { VStack }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules): an iOS `Form` lazily
        // realizes AND POOLS its rows, so a relay/follow row removed from
        // `vm.relays`/`vm.follows` (`test_relay_add_remove_round_trip` —
        // `wait_for_relay_count(0)` never resolves) or the linked-account sections
        // swapped out for the unlinked form on `unlink` (`is_linked()` stays True in
        // `test_link_generate_and_unlink_round_trip`) can linger past their real
        // removal — the same delete-zombie class rule 6 fixed for iOS Events. Cost:
        // rows lose the grouped `Form` styling (accepted rule-6 production-UI tradeoff).
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let status = vm.status {
                    if !status.registered {
                        // The bridge is absent from `fauna.bridges.list` entirely — a nest
                        // built without the `nostr` cargo feature. Genuinely, permanently
                        // unavailable — unlike `available` (the S8.9 nsec-deposit bootstrap
                        // gate), which must NOT hide the link form: on a fresh box with zero
                        // deposits so far, linking is exactly what bootstraps the first one.
                        Text(L.nostr.unavailable).foregroundStyle(.secondary)
                    } else if status.linked {
                        linkedAccountSection(status)
                        contentSettingsSection
                        relaysSection
                        followsSection
                        #if !FAUNA_EXCISE_PAYMENTS
                        zapSignersSection
                        #endif
                        if vm.showConnectedApps {
                            connectedAppsSection
                        }
                    } else {
                        unlinkedAccountSection
                    }
                } else if vm.isLoading {
                    ProgressView()
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.nostr.title)
        .task {
            guard let client else { return }
            vm.configure(api: client.api)
            await vm.refresh()
        }
    }

    // MARK: - Account (linked)

    @ViewBuilder
    private func linkedAccountSection(_ status: NostrStatus) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let pubkey = status.pubkey {
                LabeledContent(L.nostr.account.publicKey) {
                    HStack {
                        Text(pubkey)
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Button {
                            Pasteboard.copy(pubkey)
                        } label: {
                            Image(systemName: "doc.on.doc")
                        }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier(Ids.nostrPubkeyCopyBtn)
                        .automationActivate(Ids.nostrPubkeyCopyBtn) {
                            Pasteboard.copy(pubkey)
                        }
                    }
                }
            }
            if let mode = status.mode {
                LabeledContent(L.nostr.account.signingMode) {
                    Text(renderLocalizedText(nostrKeySourceLabel(mode: mode)))
                }
            }
            Button(L.nostr.account.unlink, role: .destructive) {
                Task { await vm.unlink() }
            }
            .accessibilityIdentifier(Ids.nostrUnlinkButton)
            .automationActivate(Ids.nostrUnlinkButton) {
                Task { await vm.unlink() }
            }
            .faunaGate("fauna.bridges.unlink")

            if vm.npubConfirmationOwed {
                npubConfirmBanner(npub: status.pubkey ?? "—")
            }
        }
    }

    /// Succession-aftermath npub confirm (leg 3 — `nostr.md` § Key succession
    /// and rotation). Dismissible, never a blocking modal — nostr.md's own
    /// Gotcha: the user may reach this page long after the succession, and
    /// what is owed is a *deliberate* confirmation surface, not a lock.
    /// Reference: tui's `nostr.rs` (the `npub_confirmation_owed` block).
    @ViewBuilder
    private func npubConfirmBanner(npub: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            automationText(Ids.nostrNpubConfirmBanner, L.nostr.npubConfirm.banner(npub: npub))
                .font(.caption)
            HStack {
                Button(L.nostr.npubConfirm.yesButton) {
                    Task { await vm.confirmNpub() }
                }
                .accessibilityIdentifier(Ids.nostrNpubConfirmYesButton)
                .automationActivate(Ids.nostrNpubConfirmYesButton) {
                    Task { await vm.confirmNpub() }
                }
                // No gate: `fauna.account.state.put` is OfflineSafe, so there is
                // nothing to desensitize (offline-gate-check ruling 1).
                Button(L.nostr.npubConfirm.noButton, role: .destructive) {
                    Task { await vm.dismissNpubToNewKey() }
                }
                .accessibilityIdentifier(Ids.nostrNpubConfirmNoButton)
                .automationActivate(Ids.nostrNpubConfirmNoButton) {
                    Task { await vm.dismissNpubToNewKey() }
                }
                // "No / nothing is linked" IS an unlink — the existing new-key
                // path is reached through the existing unlink+relink machinery,
                // not a bespoke one (`nostr.md`:75).
                .faunaGate("fauna.bridges.unlink")
            }
        }
        .padding(8)
        .background(.yellow.opacity(0.15))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    // MARK: - Account (unlinked)

    @ViewBuilder
    private var unlinkedAccountSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.nostr.linkAccount.title).font(.headline)
            // The link-mode select. Native targets offer generate / import nsec /
            // NIP-46 bunker (the web `nip07` browser-extension mode is
            // web-only — nostr.md § Architectural rules #4).
            Picker(L.nostr.linkAccount.modeLabel, selection: $vm.linkMode) {
                ForEach(NostrVM.LinkMode.allCases) { mode in
                    Text(renderLocalizedText(nostrLinkModeLabel(mode: mode.rawValue))).tag(mode)
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier(Ids.nostrLinkMode)
            // A Picker carries NO automation entry from `.accessibilityIdentifier`
            // alone — the in-process registry reads ONLY `automation*` modifiers, so a
            // bare a11y id is invisible to `is_visible`/`select` (that is why
            // `test_nostr_page_renders` could not see the link-mode select). Register it
            // as a value-set control: read the live selection, map a wire option back to
            // the case.
            .automationSelect(
                Ids.nostrLinkMode,
                value: { vm.linkMode.rawValue },
                set: { wire in
                    if let mode = NostrVM.LinkMode(rawValue: wire) { vm.linkMode = mode }
                }
            )

            if vm.linkMode == .import {
                SecureField(L.nostr.linkAccount.nsecPlaceholder, text: $vm.importNsec)
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    #endif
                    .accessibilityIdentifier(Ids.nostrNsecInput)
                    .automationField(Ids.nostrNsecInput, text: $vm.importNsec)
            }
            if vm.linkMode == .remote {
                // NIP-46 bunker URL — no ui.yaml id (native-only field).
                TextField("bunker://pubkey?relay=wss://...", text: $vm.bunkerUrl)
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    #endif
            }

            Button(L.nostr.linkAccount.linkButton) {
                Task { await vm.link() }
            }
            .buttonStyle(.borderedProminent)
            .disabled(vm.isLoading)
            .accessibilityIdentifier(Ids.nostrLinkButton)
            .automationActivate(Ids.nostrLinkButton, isEnabled: { !vm.isLoading }) {
                Task { await vm.link() }
            }
            // All three link modes commit through `linkBridge` → the one kind;
            // the mode picker and the nsec/bunker fields above are buffer, and
            // stay live (the commit gates, not the buffer).
            .faunaGate("fauna.bridges.link")
        }
    }

    // MARK: - Content settings

    /// The five content-publishing toggles' `(wire key, element id, default,
    /// title, subtitle)` rows, from the one shared owner
    /// (`fauna_ffi::bridges::nostr_content_toggle_options` — `nostr.md`
    /// § Where logic lives → *The content-toggle catalog*). `let`, not a
    /// computed property: the catalog is a UniFFI call, not a constant, and a
    /// SwiftUI body re-eval must not re-cross the boundary (android's
    /// `remember` for the same reason).
    private static let contentToggles = nostrContentToggleOptions()

    @ViewBuilder
    private var contentSettingsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.nostr.settings.title).font(.headline)
            ForEach(Self.contentToggles, id: \.key) { toggle in
                settingToggle(toggle)
            }
        }
    }

    /// One catalog row. Everything the row *says* — element id, wire key,
    /// default, title, subtitle — is read off `toggle`; nothing is re-spelled
    /// here (`nostr.md` § Where logic lives → *The content-toggle catalog*).
    ///
    /// The label is now the catalog's **title + subtitle pair**, the shape
    /// linux/tui already rendered and web/android adopted — a strict superset
    /// of the single long line apple used to show (priority #4). Carrying two
    /// lines means the visible label can no longer be the `Toggle`'s own
    /// (a custom-view label attaches the `accessibilityIdentifier` to a wrapper
    /// whose AX value is nil, which is what the apple-bridge reads for the
    /// "on"/"off" `state` contract). So the text moves out into a sibling
    /// `VStack` and the `Toggle` keeps a **plain string label, hidden** —
    /// `.labelsHidden()` drops it from the layout while VoiceOver keeps it, so
    /// the id still lands on the bare Switch. Same two-column shape android
    /// renders (`Row { Column { title; subtitle }; Switch }`).
    ///
    /// `isOn` is a **getter closure**, not a captured `Bool`: the in-process registry
    /// captures an `Entry`'s read closure ONCE on `.onAppear` and never re-registers on a
    /// body re-eval, so a closure over a value-type `Bool` snapshot freezes at the initial
    /// state and never round-trips to "on" after a flip + `refresh` (that is why
    /// `test_content_toggles_round_trip` failed). Reading the live `vm.status` field
    /// through the `@Observable` reference keeps the read current — the same way this
    /// view's own `isEnabled:` closures read `vm.isLoading`/`vm.relayInput` live, and the
    /// read analogue of `AdminMailView.policyToggle` reading through a `Binding`.
    ///
    /// The fallback for an unreported key is the catalog's `defaultOn` (the
    /// nest's own default), not a client-side `false` — a never-configured
    /// account now paints what the nest would actually do.
    private func settingToggle(_ toggle: FfiBridgeToggleOption) -> some View {
        let title = renderLocalizedText(toggle.label)
        let isOn: () -> Bool = { [vm] in
            vm.status?.flag(toggle.key, default: toggle.defaultOn) ?? toggle.defaultOn
        }
        let set: (Bool) async -> Void = { [vm] value in
            await vm.updateSetting(key: toggle.key, value: value)
        }
        return HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                // The catalog's second line, where a row has one — the same
                // subtitle linux renders under its SwitchRow. Three of the five
                // rows have none; those render exactly as before.
                if let subtitle = toggle.subtitle {
                    Text(renderLocalizedText(subtitle))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            Toggle(title, isOn: Binding(
                get: { isOn() },
                set: { v in Task { await set(v) } }
            ))
            .labelsHidden()
            .accessibilityIdentifier(toggle.uiId)
            // Toggle is actuatable AND readable → ONE registry entry: read the live "on"/"off"
            // (re-read via the getter, NOT a captured snapshot), flip via the same `set` the
            // binding's setter runs (the in-process analogue of the apple-bridge's native-Switch
            // mapping).
            .automationActivate(toggle.uiId, value: { isOn() ? "on" : "off" }) {
                Task { await set(!isOn()) }
            }
        }
    }

    // MARK: - Relays

    @ViewBuilder
    private var relaysSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.nostr.relays.title).font(.headline)
            HStack {
                TextField(L.nostr.relays.placeholder, text: $vm.relayInput)
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    #endif
                    .accessibilityIdentifier(Ids.nostrRelayInput)
                    .automationField(Ids.nostrRelayInput, text: $vm.relayInput)
                Button(L.nostr.relays.add) {
                    Task { await vm.addRelay() }
                }
                .disabled(vm.relayInput.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier(Ids.nostrAddRelay)
                .automationActivate(
                    Ids.nostrAddRelay,
                    isEnabled: { !vm.relayInput.trimmingCharacters(in: .whitespaces).isEmpty }
                ) {
                    Task { await vm.addRelay() }
                }
            }

            if vm.relays.isEmpty {
                Text(L.nostr.relays.none).foregroundStyle(.secondary)
            } else {
                ForEach(vm.relays, id: \.self) { relay in
                    relayRow(relay)
                }
            }
        }
    }

    @ViewBuilder
    private func relayRow(_ relay: String) -> some View {
        HStack {
            Text(relay)
                .font(.caption.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer()
            Button(role: .destructive) {
                Task { await vm.removeRelay(url: relay) }
            } label: {
                Image(systemName: "minus.circle")
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.nostrRemoveRelay)
            .automationActivate(Ids.nostrRemoveRelay) {
                Task { await vm.removeRelay(url: relay) }
            }
        }
        // Keep BOTH the row id AND the child remove-button id queryable (a bare
        // container id would clobber children — memory note / MailListMembersView).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.nostrRelayItem)
        // Per-row presence entry so the flat in-process registry can `count`
        // (and `get_text` by index) `nostr-relay-item` rows — one entry per row,
        // its text the relay URL.
        .automationValue(Ids.nostrRelayItem, text: { relay })
    }

    // MARK: - Follows

    @ViewBuilder
    private var followsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.nostr.follows.title).font(.headline)
            HStack {
                TextField(L.nostr.follows.pubkeyPlaceholder, text: $vm.followPubkey)
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    #endif
                    .accessibilityIdentifier(Ids.nostrFollowPubkeyInput)
                    .automationField(Ids.nostrFollowPubkeyInput, text: $vm.followPubkey)
                TextField(L.nostr.follows.petnamePlaceholder, text: $vm.followPetname)
                    .accessibilityIdentifier(Ids.nostrFollowPetnameInput)
                    .automationField(Ids.nostrFollowPetnameInput, text: $vm.followPetname)
                Button(L.nostr.follows.add) {
                    Task { await vm.addFollow() }
                }
                .disabled(vm.followPubkey.trimmingCharacters(in: .whitespaces).isEmpty)
                .accessibilityIdentifier(Ids.nostrAddFollow)
                .automationActivate(
                    Ids.nostrAddFollow,
                    isEnabled: { !vm.followPubkey.trimmingCharacters(in: .whitespaces).isEmpty }
                ) {
                    Task { await vm.addFollow() }
                }
            }

            if vm.follows.isEmpty {
                Text(L.nostr.follows.none).foregroundStyle(.secondary)
            } else {
                ForEach(vm.follows) { follow in
                    followRow(follow)
                }
            }
        }
    }

    @ViewBuilder
    private func followRow(_ follow: NostrFollow) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(follow.pubkey)
                    .font(.caption.monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let petname = follow.petname, !petname.isEmpty {
                    Text(petname)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            Button(role: .destructive) {
                Task { await vm.removeFollow(npub: follow.pubkey) }
            } label: {
                Image(systemName: "person.badge.minus")
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.nostrRemoveFollow)
            .automationActivate(Ids.nostrRemoveFollow) {
                Task { await vm.removeFollow(npub: follow.pubkey) }
            }
        }
        // Keep BOTH the row id AND the child remove-button id queryable (a bare
        // container id would clobber children — memory note / MailListMembersView).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.nostrFollowItem)
        // Per-row presence entry so the flat in-process registry can `count`
        // `nostr-follow-item` rows — one entry per row, its text the follow pubkey.
        .automationValue(Ids.nostrFollowItem, text: { follow.pubkey })
    }

    // MARK: - Zap signers (NIP-57 trust root)

    // EXCISED BY `FAUNA_EXCISE_PAYMENTS` — the RENDER half of the condition
    // APIClient's zap-signers section carries (`zaps` is a subset member of
    // `payments`, and the apple family has one condition for the plane). The
    // toolchain would find the glue on its own; it would NOT find these
    // `Ids.nostrZapSigner*` paints, which is exactly the hazard
    // dynamic-features.md § Platform-family surface excision names.
    #if !FAUNA_EXCISE_PAYMENTS

    /// The *Zap signers* section (`nostr-zap-signer-*`, `nostr.md` § Layout &
    /// flow item 7; `monetization.md` § Zap receipts — the trust model).
    /// Rendered for ANY linked account, unlike Connected apps below —
    /// designating who may speak for your money is orthogonal to where your
    /// key lives. Reference: linux (`settings/nostr_tab.rs`'s zap-signers
    /// group), tui (`nostr.rs::zap_signers_elements`, the lead app).
    @ViewBuilder
    private var zapSignersSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.nostr.zapSigners.title).font(.headline)
            Text(L.nostr.zapSigners.description)
                .font(.caption)
                .foregroundStyle(.secondary)

            HStack {
                TextField(L.nostr.zapSigners.pubkeyPlaceholder, text: $vm.zapSignerPubkeyInput)
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    #endif
                    .accessibilityIdentifier(Ids.nostrZapSignerPubkeyInput)
                    .automationField(Ids.nostrZapSignerPubkeyInput, text: $vm.zapSignerPubkeyInput)
                TextField(L.nostr.zapSigners.labelPlaceholder, text: $vm.zapSignerLabelInput)
                    .accessibilityIdentifier(Ids.nostrZapSignerLabelInput)
                    .automationField(Ids.nostrZapSignerLabelInput, text: $vm.zapSignerLabelInput)
                Button(L.nostr.zapSigners.add) {
                    Task { await vm.addZapSigner() }
                }
                .disabled(
                    vm.zapSignerPubkeyInput.trimmingCharacters(in: .whitespaces).isEmpty
                        || vm.zapSignerAddGateReason != nil
                )
                .accessibilityIdentifier(Ids.nostrZapSignerAddBtn)
                .automationActivate(
                    Ids.nostrZapSignerAddBtn,
                    isEnabled: {
                        !vm.zapSignerPubkeyInput.trimmingCharacters(in: .whitespaces).isEmpty
                            && vm.zapSignerAddGateReason == nil
                    }
                ) {
                    Task { await vm.addZapSigner() }
                }
                .faunaGate("fauna.nostr.zap_signers.add")
            }
            if let reason = vm.zapSignerAddGateReason {
                Text(reason).font(.caption).foregroundStyle(.secondary)
            }

            if vm.zapSigners.isEmpty {
                automationText(Ids.nostrZapSignerEmpty, L.nostr.zapSigners.none)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(vm.zapSigners, id: \.id) { signer in
                    zapSignerRow(signer)
                }
            }
        }
    }

    /// A roster row's text: the user's label (or a placeholder) and the
    /// signer's short pubkey — the shared `shortId(hex:)` formatter, the one
    /// 12-char shape every app's long-hex display uses.
    ///
    /// ⚠ Renders `signer.signerPubkey`, the STORED form, never the typed
    /// input — the nest validates 64-hex and normalizes to lowercase on
    /// write, and only that form ever matches a real receipt.
    @ViewBuilder
    private func zapSignerRow(_ signer: FfiZapSignerEntry) -> some View {
        HStack {
            let label = signer.label.trimmingCharacters(in: .whitespaces).isEmpty
                ? L.nostr.zapSigners.unnamed
                : signer.label
            Text("\(label) — \(shortId(hex: signer.signerPubkey))")
                .font(.callout)
            Spacer()
            Button(L.nostr.zapSigners.remove, role: .destructive) {
                Task { await vm.removeZapSigner(pubkey: signer.signerPubkey) }
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.nostrZapSignerRemove)
            .automationActivate(Ids.nostrZapSignerRemove) {
                Task { await vm.removeZapSigner(pubkey: signer.signerPubkey) }
            }
            .faunaGate("fauna.nostr.zap_signers.remove")
        }
        // Keep BOTH the row id AND the child remove-button id queryable (a bare
        // container id would clobber children — memory note / MailListMembersView).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.nostrZapSignerItem)
        .automationValue(Ids.nostrZapSignerItem, text: { signer.signerPubkey })
    }

    #endif

    // MARK: - Connected apps (NIP-46 bunker)

    /// The *Connected apps* invite start (`nostr-bunker-*`, `nostr.md` § The nest
    /// as the user's NIP-46 signer) — rendered only for a linked account in a
    /// custodial signing mode (`vm.showConnectedApps`). Mint an invite → the
    /// one-time `bunker://…` connect string reveals (string + QR + copy). The
    /// pending/active roster and its per-row revoke moved to Settings →
    /// Connected apps (`connected-apps.md` § Architectural rules; ``ConnectedAppsView``):
    /// this page keeps only the start, so a connection is listed in exactly one
    /// place. Reference: linux (`settings/nostr_tab.rs`'s `connected_apps_group`),
    /// web (`NostrSettingsSection.svelte`).
    @ViewBuilder
    private var connectedAppsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.nostr.connectedApps.title).font(.headline)
            Text(L.nostr.connectedApps.description)
                .font(.caption)
                .foregroundStyle(.secondary)

            Button(L.nostr.connectedApps.connectButton) {
                Task { await vm.connectApp() }
            }
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier(Ids.nostrBunkerConnectBtn)
            .automationActivate(Ids.nostrBunkerConnectBtn) {
                Task { await vm.connectApp() }
            }
            .faunaGate("fauna.nostr.bunker.create_invite")

            if let invite = vm.bunkerInvite {
                bunkerRevealBox(invite)
            }
        }
    }

    /// The one-time reveal — shown once, right after minting; the nest never
    /// re-shows a connect string (the mail-credentials one-time-reveal
    /// precedent). QR painted through the shared `qrMatrix` + `QrCodeView`
    /// (the `IdentityExportSection` pattern) — no client links a platform QR
    /// library (priorities #1/#2).
    @ViewBuilder
    private func bunkerRevealBox(_ invite: FfiCreateBunkerInviteReply) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.nostr.connectedApps.revealTitle).font(.subheadline.bold())
            if let matrix = try? qrMatrix(data: invite.connectString) {
                QrCodeView(matrix: matrix)
                    .frame(width: 200, height: 200)
                    .accessibilityIdentifier(Ids.nostrBunkerConnectQr)
                    .accessibilityLabel(L.nostr.connectedApps.qrAlt)
                    .automationValue(Ids.nostrBunkerConnectQr, text: { "\(matrix.size)" })
            }
            automationText(Ids.nostrBunkerConnectString, invite.connectString)
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .lineLimit(3)
                .truncationMode(.middle)
                // Rule 2 of `security.md` § On-screen secret exposure. The
                // one-time bunker connect string is a minted, revocable
                // credential, and this view exists only while it is shown — so
                // the hold's lifetime is the reveal's with no flag needed.
                .suppressScreenCapture()
            CopyButton(Ids.nostrBunkerConnectCopyBtn, text: invite.connectString)
            Text(L.nostr.connectedApps.revealHint)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.top, 4)
    }
}
