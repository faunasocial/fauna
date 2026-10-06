import SwiftUI

/// The shared **Bluesky** settings sub-page (macOS + iOS, one FaunaKit view) —
/// `docs/goal/ui/atproto.md`. One page answers one question: *how deep is this
/// user's Bluesky integration?* The spine is the four-rung integration-depth
/// selector; every other control reveals below it as a sub-setting of the level
/// that makes it meaningful:
///
/// - **selector** (`atproto-depth-*`) — one ordered choice; selecting a
///   *different* level stages the transition card, never mutates the level
///   directly (the one exception, Off → Linked, is effect-free and applies on
///   select). Hosted rungs grey-with-reason on a non-public domain.
/// - **transition card** (`atproto-depth-confirm-card`) — the composed effect
///   lines, rendered *verbatim* from the machine's `pendingTransition.lines`
///   (the single `TransitionPlan` the nest also executes), plus the
///   history-backfill opt-in on a minting move and confirm/cancel.
/// - **Linked panel** — the shared `BridgeCardContent` (`bridge-card`/
///   `bridge-link-form`), fed the `"bluesky"` `BridgeInfo` row directly (that
///   row is otherwise excluded from the unified Bridges page —
///   `is_unified_bridges_page_bridge`).
/// - **hosted panel** — pre-mint: the DID-method radio + the either-way handle
///   line. The **identity summary** (`atproto-hosted-handle`) is a sibling
///   section gated on the identity itself, not the level — it renders
///   whenever `snapshot.identity` is set, including a deactivated identity at
///   Off/Linked (`ui/atproto.md` § Errors & edge cases).
/// - **full-PDS panel** — the F1 login-plane surface (app credentials, the
///   external-apps kill-switch and the D10 authoring-delegation row), gated on
///   level = `hosted_full`. The OAuth consent card and the connected-apps roster
///   it used to carry render on the Connected apps page now
///   (`connected-apps.md`; ``ConnectedAppsView``).
/// - **delete presence** (`atproto-delete-presence`) — visible whenever a
///   hosted identity exists (active or deactivated); opens its own confirm
///   card (`atproto-delete-confirm-card`, distinct from the depth selector's)
///   whose one commit is `fauna.bridges.atproto.delete_presence`
///   (`atproto-pds-bridge.md` § Disable & revocation layer 2).
///
/// Backed by the shared `AtprotoSettingsMachine` (`AtprotoSettingsVM`) — mirrors
/// `LabelerCatalogView`'s observer + `@Observable` VM + snapshot render pattern.
/// Reference implementation: `apps/fauna-tui/src/settings/atproto.rs` (the lead
/// app for the delete ceremony + these two rendering rules).
public struct AtprotoSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AtprotoSettingsVM()
    @State private var linkedVM = BridgeManagerVM()
    /// The Linked-account panel reuses `BridgeCardContent`, so it carries the same
    /// ward feed-source ask (`family-safety.md` § Feed-source approvals).
    @Environment(FamilyStatusStore.self) private var familyStatus: FamilyStatusStore?

    public init() {}

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                automationText(Ids.pageHeading, L.atprotoSettings.title)
                    .font(.title2)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                // The recovery-fork contest card leads the page, above the
                // depth selector — an identity under active attack outranks
                // every settings row below it (atproto-identity-custody.md
                // § The 72 h recovery-fork contest).
                if let contest = snapshot?.contest {
                    contestCardSection(contest)
                }
                if let confirm = snapshot?.contestConfirm {
                    contestConfirmSection(confirm)
                }

                depthSelectorSection

                if let card = snapshot?.pendingTransition {
                    transitionCardSection(card)
                }

                if showLinkedPanel {
                    linkedPanelSection
                }

                if showHostedPanel {
                    hostedPanelSection
                }

                if let identity = snapshot?.identity {
                    identitySummarySection(identity)
                }

                if snapshot?.showDeletePresence == true {
                    deletePresenceSection
                }

                if let confirm = snapshot?.deleteConfirm {
                    deleteConfirmSection(confirm)
                }

                if isFullPds {
                    fullPdsSection
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier(Ids.atprotoPage)
        .automationValue(Ids.atprotoPage, text: { "" })
        .pageTitle(L.atprotoSettings.title)
        .task {
            guard let client else { return }
            await vm.configure(api: client.api)
            linkedVM.singleBridgeId = "bluesky"
            linkedVM.configure(api: client.api, familyStatus: familyStatus)
            await linkedVM.refresh()
        }
    }

    private var snapshot: AtprotoSettingsSnapshot? { vm.snapshot }
    private var level: String { snapshot?.level ?? "off" }
    private var showLinkedPanel: Bool { level == "linked" }
    private var showHostedPanel: Bool {
        level.hasPrefix("hosted")
            || (snapshot?.pendingTransition?.targetLevel.hasPrefix("hosted") ?? false)
    }
    private var isFullPds: Bool { level == "hosted_full" }

    // MARK: - Depth selector

    /// The four rungs' `(wire level, element id, title, description, hosted)`
    /// rows, from the one shared owner — `depthLevelOptions()` in the settings
    /// machine's own UniFFI namespace, no `fauna-ffi` hop
    /// (`atproto.md` § Where logic lives → *The rung catalog is part of "all of
    /// it"*). `let`, not a computed property: it is a UniFFI call, so a body
    /// re-eval must not re-cross the boundary.
    private static let depthLevels = depthLevelOptions()

    private var depthSelectorSection: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(L.atprotoSettings.depthHeading).font(.headline)
            ForEach(Self.depthLevels, id: \.level) { rung in
                depthRow(rung)
            }
            // Copy comprehensibility rule 5 — a gated row must always carry a
            // reason, including the pre-fetch window before `configure`
            // resolves: `vm.snapshot` is seeded from the Rust-ratified
            // `atprotoSettingsPrefetchSnapshot()` default there, so
            // `hostedGateReason` is never nil while the gate is closed — no
            // local stand-in needed (mirrors web/android reading the seam
            // directly).
            if snapshot?.hostedAllowed != true, let reason = snapshot?.hostedGateReason {
                Text(renderLocalizedText(reason))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityIdentifier(Ids.atprotoDepthSelector)
        .automationValue(Ids.atprotoDepthSelector, value: { level })
    }

    @ViewBuilder
    private func depthRow(_ rung: DepthLevelOption) -> some View {
        // `rung.hosted` is the catalog's own fact, not a `hasPrefix("hosted")`
        // sniff over the wire spelling — a string-prefix read of a closed
        // vocabulary is exactly what would mis-gate a future `hosted_*`-but-
        // ungated rung on six surfaces at once (`atproto.md` § Where logic
        // lives). The `!gated || active` rule below stays the app's.
        let gated = rung.hosted && snapshot?.hostedAllowed != true
        // A hosted rung the user is *already at* stays selectable so a step-down
        // is reachable even if the domain later stops being public — the gate
        // only ever blocks *entering* a hosted level from a lower one (mirrors
        // linux `render`).
        let enabled = !gated || level == rung.level
        Button {
            Task { await vm.selectLevel(rung.level) }
        } label: {
            HStack {
                Image(systemName: level == rung.level ? "largecircle.fill.circle" : "circle")
                VStack(alignment: .leading, spacing: 2) {
                    Text(renderLocalizedText(rung.title))
                    Text(renderLocalizedText(rung.description))
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        .disabled(!enabled)
        .buttonStyle(.plain)
        .accessibilityIdentifier(rung.uiId)
        .automationActivate(rung.uiId, isEnabled: { enabled }, value: { gated ? "gated" : "ok" }) {
            Task { await vm.selectLevel(rung.level) }
        }
        // A rung is not purely staging: `select_level` applies an effect-free
        // move (Off → Linked) IMMEDIATELY, card-less, and that is a nest write
        // (`machine.rs::select_level` — "still a nest write … just card-less").
        // So the rung is a dispatch-on-change control and declares, exactly as
        // the confirm below does for the carded moves.
        .faunaGate("fauna.bridges.atproto.set_integration_level")
    }

    // MARK: - Recovery-fork contest

    /// `atproto-contest-card` — decision 2: `atproto-contest` renders ONLY
    /// when `card.showContest` is true, never inferred from `state` (the
    /// `not-contestable` state covers two distinct un-buttonable reasons —
    /// genesis and unauthenticated — so a button gated on "not window-closed"
    /// would dead-end on both).
    private func contestCardSection(_ card: ContestCardRow) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.atprotoSettings.contestCardHeading).font(.headline)
            automationText(Ids.atprotoContestDetail, renderLocalizedText(card.detail))
            if let deadline = card.deadline {
                automationText(Ids.atprotoContestDeadline, renderLocalizedText(deadline))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if card.showContest {
                Button(L.atprotoSettings.contestButton) { vm.openContestConfirm() }
                    .accessibilityIdentifier(Ids.atprotoContest)
                    .automationActivate(Ids.atprotoContest) { vm.openContestConfirm() }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.atprotoContestCard)
        .automationValue(Ids.atprotoContestCard, value: { card.state })
    }

    /// `atproto-contest-confirm-card` — the ceremony's confirm, present only
    /// while `contestConfirm` is non-nil. Only `requestContest` (the confirm)
    /// dispatches; `openContestConfirm`/`cancelContest` are local and touch
    /// nothing on the wire, so neither carries a `.faunaGate`, unlike the
    /// depth-transition confirm above.
    private func contestConfirmSection(_ confirm: ContestConfirmCardModel) -> some View {
        let lines = confirm.lines.map(renderLocalizedText).joined(separator: "\n")
        return VStack(alignment: .leading, spacing: 8) {
            Text(lines)
            HStack {
                Button(L.atprotoSettings.contestCancelButton) { vm.cancelContest() }
                    .disabled(confirm.inProgress)
                    .accessibilityIdentifier(Ids.atprotoContestCancel)
                    .automationActivate(Ids.atprotoContestCancel, isEnabled: { !confirm.inProgress }) {
                        vm.cancelContest()
                    }
                Button(L.atprotoSettings.contestConfirmButton, role: .destructive) {
                    Task { await vm.requestContest() }
                }
                .disabled(confirm.inProgress)
                .accessibilityIdentifier(Ids.atprotoContestConfirm)
                .automationActivate(Ids.atprotoContestConfirm, isEnabled: { !confirm.inProgress }) {
                    Task { await vm.requestContest() }
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.atprotoContestConfirmCard)
        .automationValue(Ids.atprotoContestConfirmCard, text: { lines })
    }

    // MARK: - Transition card

    private func transitionCardSection(_ card: TransitionCardModel) -> some View {
        let lines = card.lines.map(renderLocalizedText).joined(separator: "\n")
        return VStack(alignment: .leading, spacing: 8) {
            Text(L.atprotoSettings.depthCardHeading).font(.headline)
            Text(lines)
            if card.showHistoryBackfill {
                Toggle(L.atprotoSettings.historyBackfillLabel, isOn: Binding(
                    get: { snapshot?.historyBackfill ?? false },
                    set: { vm.setHistoryBackfill($0) }
                ))
                .accessibilityIdentifier(Ids.atprotoHistoryBackfill)
                .automationActivate(
                    Ids.atprotoHistoryBackfill,
                    value: { (snapshot?.historyBackfill ?? false) ? "on" : "off" }
                ) {
                    vm.setHistoryBackfill(!(snapshot?.historyBackfill ?? false))
                }
            }
            HStack {
                Button(L.atprotoSettings.depthCancelButton) { vm.cancelTransition() }
                    .disabled(card.inProgress)
                    .accessibilityIdentifier(Ids.atprotoDepthCancel)
                    .automationActivate(Ids.atprotoDepthCancel, isEnabled: { !card.inProgress }) {
                        vm.cancelTransition()
                    }
                Button(L.atprotoSettings.depthConfirmButton) { Task { await vm.confirmTransition() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(card.inProgress)
                    .accessibilityIdentifier(Ids.atprotoDepthConfirm)
                    .automationActivate(Ids.atprotoDepthConfirm, isEnabled: { !card.inProgress }) {
                        Task { await vm.confirmTransition() }
                    }
                    // The card's own cancel, the DID-method radio and the
                    // history-backfill toggle above are all buffer
                    // (`set_did_method`/`set_history_backfill` touch local state
                    // only) — the confirm is the commit, so only it declares.
                    .faunaGate("fauna.bridges.atproto.set_integration_level")
            }
        }
        .padding()
        .background(Color.secondary.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .accessibilityIdentifier(Ids.atprotoDepthConfirmCard)
        .automationValue(Ids.atprotoDepthConfirmCard, text: { lines })
    }

    // MARK: - Linked panel (`bridge-card` / `bridge-link-form`, reused verbatim)

    private var linkedPanelSection: some View {
        Group {
            if let bridge = linkedVM.bridges.first(where: { $0.id == "bluesky" }) {
                BridgeCardContent(bridge: bridge, vm: linkedVM, isDesktop: isDesktopPlatform, compact: isDesktopPlatform, index: 0)
            }
        }
    }

    // MARK: - Hosted panel

    private var hostedPanelSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            if snapshot?.showDidMethodRadio == true {
                didMethodSection
            }
        }
    }

    // The identity summary: gated on the IDENTITY, not on the level
    // (`ui/atproto.md` § Errors & edge cases — "A deactivated identity at
    // level Off/Linked: the identity summary renders... so the user can see
    // what re-enabling restores"). Was inside `hostedPanelSection` above,
    // which is exactly the one place that rule can never hold; tui led the
    // fix (`settings/atproto.rs`'s own doc comment), apple follows here.
    private func identitySummarySection(_ identity: IdentitySummaryRow) -> some View {
        // The one shared reading (`identity_status_label`, UniFFI
        // `identityStatusLabel`); an unrecognized status renders its wire word
        // via `renderLocalizedText`'s missing-key fallback.
        let status = renderLocalizedText(identityStatusLabel(status: identity.status))
        return automationText(
            Ids.atprotoHostedHandle,
            "\(L.atprotoSettings.hostedHandlePrefix(handle: identity.handle)) · "
                + "\(L.atprotoSettings.hostedMethodPrefix(method: identity.method)) · \(status)"
        )
    }

    private var didMethodSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.atprotoSettings.didMethodHeading).font(.headline)
            didMethodRow(id: "atproto-did-method-plc", wire: "plc",
                         title: L.atprotoSettings.didMethodPlcTitle, desc: L.atprotoSettings.didMethodPlcDesc)
            didMethodRow(id: "atproto-did-method-web", wire: "web",
                         title: L.atprotoSettings.didMethodWebTitle, desc: L.atprotoSettings.didMethodWebDesc)
            if !(snapshot?.handlePreview.isEmpty ?? true) {
                Text(L.atprotoSettings.handleEitherWay(handle: snapshot?.handlePreview ?? ""))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityIdentifier(Ids.atprotoDidMethod)
        .automationValue(Ids.atprotoDidMethod, text: { "" })
    }

    private func didMethodRow(id: String, wire: String, title: String, desc: String) -> some View {
        let selected = (snapshot?.didMethod ?? "plc") == wire
        return Button {
            vm.setDidMethod(wire)
        } label: {
            HStack {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle")
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                    Text(desc).font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(id)
        .automationActivate(id, value: { selected ? "selected" : "unselected" }) {
            vm.setDidMethod(wire)
        }
    }

    // MARK: - Delete presence (`ui/atproto.md` § User actions row 4;
    // `atproto-pds-bridge.md` § Disable & revocation layer 2)

    private var deletePresenceSection: some View {
        Button(L.atprotoSettings.deletePresenceButton) { vm.openDeleteConfirm() }
            .foregroundStyle(.red)
            .accessibilityIdentifier(Ids.atprotoDeletePresence)
            .automationActivate(Ids.atprotoDeletePresence) { vm.openDeleteConfirm() }
    }

    /// Its own confirm card — never the depth selector's (distinct destructive
    /// flow, `ui/atproto.md` § User actions row 4). The copy is the machine's,
    /// rendered verbatim: the ceremony's promises about what survives are not
    /// this file's to word. `openDeleteConfirm`/`cancelDelete` are synchronous
    /// local mutations; `confirmDelete` is the one network round trip
    /// (`fauna.bridges.atproto.delete_presence`), so only its button gates.
    private func deleteConfirmSection(_ confirm: DeleteConfirmCardModel) -> some View {
        let lines = confirm.lines.map(renderLocalizedText).joined(separator: "\n")
        return VStack(alignment: .leading, spacing: 8) {
            Text(lines)
            retireIdentityRow(confirm.retireIdentity, inProgress: confirm.inProgress)
            HStack {
                Button(L.atprotoSettings.deleteCancelButton) { vm.cancelDelete() }
                    .disabled(confirm.inProgress)
                    .accessibilityIdentifier(Ids.atprotoDeleteCancel)
                    .automationActivate(Ids.atprotoDeleteCancel, isEnabled: { !confirm.inProgress }) {
                        vm.cancelDelete()
                    }
                Button(L.atprotoSettings.deleteConfirmButton, role: .destructive) {
                    Task { await vm.confirmDelete() }
                }
                .disabled(confirm.inProgress)
                .accessibilityIdentifier(Ids.atprotoDeleteConfirm)
                .automationActivate(Ids.atprotoDeleteConfirm, isEnabled: { !confirm.inProgress }) {
                    Task { await vm.confirmDelete() }
                }
                .faunaGate("fauna.bridges.atproto.delete_presence")
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.atprotoDeleteConfirmCard)
        .automationValue(Ids.atprotoDeleteConfirmCard, text: { lines })
    }

    /// The card's terminal opt-in, `atproto-delete-tombstone` — "also
    /// permanently retire this identity" (`ui/atproto.md` § User actions row 4).
    /// **Always on the card and never pre-ticked**: where the identity cannot
    /// be retired (did:web, or a did:plc not yet published) the row renders
    /// greyed with the machine's reason beside it rather than hidden, because a
    /// live control there could only error on press. The tick is card state on
    /// the machine, not a gesture of its own — nothing is sent until the
    /// confirm, and ticking swaps the card's identity-kept line for the
    /// cannot-be-undone one machine-side, so this row derives nothing. `state`
    /// is `on` / `off` / `unavailable` (the greyed row is assertable apart from
    /// its reason's wording).
    private func retireIdentityRow(_ optIn: RetireIdentityOptIn, inProgress: Bool) -> some View {
        let live = optIn.available && !inProgress
        let state = !optIn.available ? "unavailable" : (optIn.selected ? "on" : "off")
        return VStack(alignment: .leading, spacing: 4) {
            Toggle(L.atprotoSettings.deleteRetireIdentityLabel, isOn: Binding(
                get: { optIn.selected },
                set: { vm.setDeleteRetireIdentity($0) }
            ))
            .disabled(!live)
            .accessibilityIdentifier(Ids.atprotoDeleteTombstone)
            .automationActivate(
                Ids.atprotoDeleteTombstone,
                isEnabled: { live },
                value: { state }
            ) {
                vm.setDeleteRetireIdentity(!optIn.selected)
            }
            if let reason = optIn.unavailableReason {
                Text(renderLocalizedText(reason))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    // MARK: - Full-PDS panel (gated on level = hosted_full)

    private var fullPdsSection: some View {
        // The OAuth consent card and the connected-apps roster that used to lead
        // and follow this panel moved to Settings → Connected apps
        // (`connected-apps.md` § Architectural rules): a row moves, it is never
        // shown twice.
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text(L.atprotoSettings.appCredentialsHeading).font(.headline)
                    Spacer()
                    Button(L.atprotoSettings.mintButton) { Task { await vm.mint() } }
                        .accessibilityIdentifier(Ids.atprotoAppCredentialMint)
                        .automationActivate(Ids.atprotoAppCredentialMint) {
                            Task { await vm.mint() }
                        }
                        .faunaGate("fauna.bridges.atproto.provision_app_credential")
                }
                let credentials = snapshot?.credentials ?? []
                if credentials.isEmpty {
                    Text(L.atprotoSettings.appCredentialsEmpty)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(Array(credentials.enumerated()), id: \.element.credentialId) { index, cred in
                        credentialRow(cred, index: index)
                    }
                }
            }
            .accessibilityIdentifier(Ids.atprotoAppCredentialsList)
            .automationValue(Ids.atprotoAppCredentialsList, text: { "\(snapshot?.credentials.count ?? 0)" })

            Toggle(L.atprotoSettings.externalAppsToggle, isOn: Binding(
                get: { snapshot?.externalAppsEnabled ?? true },
                set: { newVal in Task { await vm.setExternalAppsEnabled(newVal) } }
            ))
            .accessibilityIdentifier(Ids.atprotoExternalAppsEnable)
            .automationActivate(
                Ids.atprotoExternalAppsEnable,
                value: { (snapshot?.externalAppsEnabled ?? true) ? "on" : "off" }
            ) {
                Task { await vm.setExternalAppsEnabled(!(snapshot?.externalAppsEnabled ?? true)) }
            }
            // A dispatch-on-change toggle: flipping it IS the nest write.
            .faunaGate("fauna.bridges.atproto.set_external_apps_enabled")

            delegationSection
        }
    }

    // MARK: - D10 authoring-delegation row (`atproto-delegation-*`)

    /// What authorizes an external ATProto app to *post* as this account, as
    /// opposed to merely signing in (the credentials + connected-apps groups
    /// above govern that). `atproto-pds-full.md` § Problem 1 → D10; leaf
    /// decomposition mirrors the shipped `nest-trust-grant-*` rows
    /// (`LinkedNestsView.TrustGrantItemView`), the named interaction template.
    ///
    /// Two states, one always-present control:
    ///
    /// - **nil** — no delegation, or one whose stored cert FAILED the
    ///   client-side verify under the account's own identity key. The row and
    ///   its leaves are **withheld**, never rendered as a grant the user cannot
    ///   be shown to have made (the mismatch surfaces on `error-message`, which
    ///   the machine has already set). Only `-authorize` renders.
    /// - **some** — the leaves render. `-authorize` STAYS rendered, because
    ///   re-authorizing IS the renewal gesture: provisioning overwrites the
    ///   cert, so a lapsed grant recovers in one gesture with no revoke first.
    ///   Hiding it once authorized would force the revoke-then-re-mint flow
    ///   that churns the signing sub-key for what is only an expiry refresh.
    ///
    /// Mirrors tui's `settings/atproto.rs::delegation_elements` (lead app).
    @ViewBuilder
    private var delegationSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.atprotoSettings.delegationHeading).font(.headline)
            if let row = snapshot?.delegation {
                delegationRowView(row)
            } else {
                Text(L.atprotoSettings.delegationEmpty)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            HStack {
                Button(snapshot?.delegation == nil
                       ? L.atprotoSettings.delegationAuthorizeButton
                       : L.atprotoSettings.delegationReauthorizeButton) {
                    Task { await vm.authorizeDelegation() }
                }
                .accessibilityIdentifier(Ids.atprotoDelegationAuthorize)
                .automationActivate(Ids.atprotoDelegationAuthorize) {
                    Task { await vm.authorizeDelegation() }
                }
                .faunaGate("fauna.bridges.atproto.provision_authoring_delegation")
                if snapshot?.delegation != nil {
                    Button(L.atprotoSettings.delegationRevokeButton, role: .destructive) {
                        Task { await vm.revokeDelegation() }
                    }
                    .accessibilityIdentifier(Ids.atprotoDelegationRevoke)
                    .automationActivate(Ids.atprotoDelegationRevoke) {
                        Task { await vm.revokeDelegation() }
                    }
                    .faunaGate("fauna.bridges.atproto.revoke_authoring_delegation")
                }
            }
        }
    }

    private func delegationRowView(_ row: DelegationRow) -> some View {
        // ⚠ MICROseconds on this row — these come from the SIGNED CERT, not the
        // wire, unlike the credential/session rows above which carry
        // milliseconds. `formattedDate` takes millis, hence the /1000; a
        // millis-vs-micros slip here would date every grant ~1000x into its own
        // future (the exact bug that made the nest's expiry check dead until
        // 2026-07-29).
        let authorized = formattedDate(millisSince1970: Int64(row.authorizedAtMicros / 1000))
        let lastsUntil = row.expiresAtMicros.map {
            L.atprotoSettings.delegationLastsUntil(
                authorized: authorized,
                expires: formattedDate(millisSince1970: Int64($0 / 1000))
            )
        } ?? L.atprotoSettings.delegationLastsUntilNoExpiry(authorized: authorized)
        return VStack(alignment: .leading, spacing: 2) {
            automationText(
                Ids.atprotoDelegationScope,
                L.atprotoSettings.delegationScopePrefix(
                    capabilities: delegationCapabilitiesLine(row.capabilities)
                )
            )
            .font(.subheadline)
            .fontWeight(.medium)

            automationText(Ids.atprotoDelegationLastsUntil, lastsUntil)
                .font(.caption)
                .foregroundStyle(.secondary)

            // The liveness WIRE SPELLING rides the automation `value` slot, which
            // is what `/element/attr?attr=state` reads — so the e2e asserts the
            // STATE, not its localized prose. An unrecognized spelling from a
            // newer nest still renders its own wire form (degrade, never fail).
            Text(delegationStatusLabel(row.liveness))
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.atprotoDelegationStatus)
                .automationValue(
                    Ids.atprotoDelegationStatus,
                    text: { delegationStatusLabel(row.liveness) },
                    value: { row.liveness }
                )

            // The ADVISORY last-use hint (D10 § Audit). Every leaf above is
            // derived from the signed cert, verified client-side under the
            // account's own identity key. This one is NOT: the nest simply
            // asserts it, with nothing signing it. So the wording hedges
            // deliberately and the leaf carries `advisory=true` — an absent
            // stamp means no use was REPORTED, never that no app posted as the
            // user, because a nest that under-reports is exactly what this
            // value cannot detect. Do not re-word it more confident than it is.
            Text(delegationLastUsedLabel(row.lastUsedAtMillis))
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.atprotoDelegationLastUsed)
                .automationValue(
                    Ids.atprotoDelegationLastUsed,
                    text: { delegationLastUsedLabel(row.lastUsedAtMillis) },
                    value: { "true" }
                )
            // Points the user at the surface that IS verified, rather than
            // leaving them to trust the number above. Chrome — no id.
            Text(L.atprotoSettings.delegationLastUsedHint)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.atprotoDelegationRow)
        // Presence entry so the in-process registry resolves
        // `is_visible("atproto-delegation-row")` — a bare
        // `.accessibilityIdentifier` never registers (the documented
        // container pattern: quoted-post / nest-trust-grant-item).
        .automationValue(Ids.atprotoDelegationRow, text: { row.liveness })
    }

    private func credentialRow(_ cred: AppCredentialRow, index: Int) -> some View {
        let revealed = vm.revealedSecrets[cred.credentialId]
        let subtitle: String = {
            let created = L.atprotoSettings.credentialCreatedPrefix(date: formattedDate(millisSince1970: cred.createdAtMillis))
            if let lastUsed = cred.lastUsedAtMillis {
                return "\(created) · \(L.atprotoSettings.credentialLastUsedPrefix(date: formattedDate(millisSince1970: lastUsed)))"
            }
            return "\(created) · \(L.atprotoSettings.credentialNeverUsed)"
        }()
        return HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                Text(cred.label)
                Text(subtitle).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if cred.revealable || revealed != nil {
                Button(revealed ?? L.atprotoSettings.revealButton) {
                    Task { await vm.revealSecret(credentialId: cred.credentialId) }
                }
                .disabled(revealed != nil)
                .accessibilityIdentifier(Ids.atprotoAppCredentialReveal)
                .automationActivate(
                    Ids.atprotoAppCredentialReveal,
                    isEnabled: { revealed == nil },
                    value: { revealed ?? L.atprotoSettings.revealButton }
                ) {
                    Task { await vm.revealSecret(credentialId: cred.credentialId) }
                }
                // Rule 2 of `security.md` § On-screen secret exposure. This
                // control is always on screen — its own label BECOMES the app
                // password once revealed — so it holds only while `revealed`
                // is populated, rather than for the lifetime of the row.
                .suppressScreenCapture(isActive: revealed != nil)
            }
            Button(L.atprotoSettings.revokeButton, role: .destructive) {
                Task { await vm.revoke(credentialId: cred.credentialId) }
            }
            .accessibilityIdentifier(Ids.atprotoAppCredentialRevoke)
            .automationActivate(Ids.atprotoAppCredentialRevoke) {
                Task { await vm.revoke(credentialId: cred.credentialId) }
            }
            // The reveal beside it is a LOCAL read of the stored secret
            // (the `fauna.state.atproto` rows, never the nest), so it
            // deliberately carries no gate and stays usable offline.
            .faunaGate("fauna.bridges.atproto.revoke_app_credential")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.atprotoAppCredentialItem)
        .automationValue(Ids.atprotoAppCredentialItem, text: { cred.credentialId })
        .automationScope(Ids.atprotoAppCredentialItem, index: index)
    }
}

/// The granted capabilities in user voice. `DelegationRow.capabilities` carries
/// the cert's WIRE spellings; an unrecognized one degrades to its wire form
/// rather than vanishing, so the row stays honest about what was actually
/// granted. Same shape as `LinkedNestsView`'s `scopeLine` — the grant row this
/// one mirrors — and as tui's `capabilities_text`. Thin wrapper over the
/// shared `fauna_atproto_settings_machine::delegation_capability_label`
/// (UniFFI `delegationCapabilityLabel`) — was a hand-rolled duplicate of that
/// same match until this pass.
private func delegationCapabilitiesLine(_ capabilities: [String]) -> String {
    capabilities
        .map { renderLocalizedText(delegationCapabilityLabel(capability: $0)) }
        .joined(separator: ", ")
}

/// Liveness in user voice. Mirrors `LinkedNestsView`'s `statusLabel`; an
/// unrecognized spelling from a newer nest renders its own wire form rather
/// than an empty cell (degrade, never fail to decode) — `renderLocalizedText`'s
/// missing-key fallback gives the same behavior as the shared
/// `delegation_liveness_label`'s own `other => other` arm. Thin wrapper over
/// the shared door (UniFFI `delegationLivenessLabel`) — was a hand-rolled
/// duplicate of that same match until this pass.
private func delegationStatusLabel(_ liveness: String) -> String {
    renderLocalizedText(delegationLivenessLabel(liveness: liveness))
}

/// The advisory last-use line. `nil` is a MEANINGFUL state — "no use reported" —
/// never a loading gap and never a claim that nothing happened.
private func delegationLastUsedLabel(_ lastUsedAtMillis: Int64?) -> String {
    guard let millis = lastUsedAtMillis else {
        return L.atprotoSettings.delegationLastUsedNever
    }
    return L.atprotoSettings.delegationLastUsed(when: formattedDate(millisSince1970: millis))
}

private func formattedDate(millisSince1970 millis: Int64) -> String {
    ValueFormat.absoluteDate(epochMs: millis, withTime: true)
}

private var isDesktopPlatform: Bool {
    #if os(macOS)
    true
    #else
    false
    #endif
}
