import SwiftUI

/// Shared (macOS + iOS) renderer for the **Settings → Devices** page — the device
/// **roster only**, read off `DevicesMachineVM.snapshot.devices` with no
/// client-side roster logic. Page errors surface in
/// `error-message`. Target state: `docs/goal/ui/devices.md`.
///
/// **2026-06-28 sync/folder UI unification:** the former combined "Peers" /
/// Devices page split — the folder list + creation wizard + conflict surface
/// moved to **Settings → Folders** (`FoldersContent`, folders.md); this page
/// is the roster alone (the same `DevicesMachine` drives both, each rendering its
/// slice — devices.md § State & data shape). The shared `DevicesView` page shell
/// (`Views/DevicesView.swift`, one struct for both apple targets) wraps this in the host's navigation chrome
/// and owns the page refresh-on-appear.
public struct DevicesContent: View {
    let vm: DevicesMachineVM
    /// This device's roster row — `DevicesMachineVM.thisDeviceRow`, the shared
    /// rule's answer (the enrolled row, else this app's own id), or `nil`
    /// pre-registration —
    /// drives `device-this-mark-badge` (`devices.md` § This-device marker).
    let localDeviceId: String?
    /// This client's own actor id hex, for the page-level
    /// `peer-actor-id-copy-btn` (`devices.md` § Layout & flow point 2) — a
    /// single copy of THIS client's identity, handed to a new device being
    /// paired, never a per-row device id.
    let actorId: String?

    public init(vm: DevicesMachineVM, localDeviceId: String? = nil, actorId: String? = nil) {
        self.vm = vm
        self.localDeviceId = localDeviceId
        self.actorId = actorId
    }

    /// Which `device-member-card` has its removal ARMED, by the card's fleet id
    /// (`FleetMemberSummary.deviceId`) — the confirm/cancel pair paints only on
    /// the card carrying it, mirroring `AdminCustodyHostingView`'s page-local
    /// `armedKey` idiom (arming a new card silently retargets) and tui's
    /// `DevicesState::member_remove_pending`. The key, not a list position, is
    /// what travels to the confirm, so a refresh that reshapes the list
    /// between arm and confirm cannot retarget the removal. A fresh page visit
    /// (a new `DevicesContent` identity) disarms for free — `@State` never
    /// survives that.
    @State private var memberRemoveArmedKey: String?

    // Rendered as an eager `ScrollView { VStack }` rather than a lazy `List`
    // (apple-e2e-automation.md registration rule 6, extended to macOS `List`
    // 2026-07-30): a macOS `List` is an NSTableView that realizes rows lazily,
    // so a `device-card` appended to `vm.snapshot.devices` can go unbuilt
    // indefinitely — no view, no `.onAppear`, no registration (measured on the
    // Events agenda; `test_device_cards.py`/`test_family.py` count and remove
    // `device-card` rows the same way). Shared with iOS — both apple targets
    // render this eagerly (rule 6's own statement: "on BOTH apple apps").
    // Cost: rows lose native List section-separator styling, the accepted
    // rule-6 tradeoff every other converted page already pays.
    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                deviceSection
                memberSection
                custodySection
                heldSection
                offerSection
                CustodyMintFlow(vm: vm)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .safeAreaInset(edge: .bottom) {
            if let error = vm.errorMessage {
                ErrorBanner(message: error)
                    .padding()
            }
        }
    }

    // MARK: - Devices

    @ViewBuilder
    private var deviceSection: some View {
        let devices = vm.snapshot?.devices ?? []
        Text(L.devices.myDevices)
            .font(.headline)
        HStack(spacing: 4) {
            Text(shortId(hex: actorId ?? ""))
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            Button {
                copyActorId()
            } label: {
                Image(systemName: "doc.on.doc").font(.caption)
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.peerActorIdCopyBtn)
            // In-process e2e driver actuation: fires the same `copyActorId()`
            // the Button does, exposing the copied id as the read value (one
            // Entry). Env-gated no-op in production. Page-level — renders even
            // with an empty roster, since pairing the FIRST device is exactly
            // when handing out this client's own actor id is needed.
            .automationActivate(Ids.peerActorIdCopyBtn, value: { actorId ?? "" }) { copyActorId() }
        }
        // Self-scoped (rule 5), deliberately: `peer-actor-id-copy-btn` has
        // exactly one instance and is never nested in a `device-card`, but
        // with NO ancestor path at all it has no scope path, so
        // `AutomationRegistry.hasScopePath` is false and a scoped query falls
        // to the legacy flat-index heuristic — `count(id) > scope.index`
        // resolves `scope="device-card[0]"` to true purely because 1 > 0,
        // even though this button is not inside any device-card's subtree.
        // Pushing a real (if trivial) ancestor path makes the id
        // scope-participating, so a `device-card[N]` prefix-match correctly
        // fails for every N. `test_device_cards.py`'s negative assert
        // (`not is_visible("peer-actor-id-copy-btn", scope="device-card[i]")`)
        // is what caught this.
        .automationScope(Ids.peerActorIdCopyBtn, index: 0)
        if devices.isEmpty {
            Text(L.devices.noDevices)
                .font(.caption)
                .foregroundStyle(.secondary)
        } else {
            ForEach(Array(devices.enumerated()), id: \.offset) { index, device in
                DeviceCard(vm: vm, device: device, index: index, localDeviceId: localDeviceId)
            }
        }
    }

    private func copyActorId() {
        Pasteboard.copy(actorId ?? "")
    }

    // MARK: - Signed-in devices without a matching entry (devices.md § Members
    // without a matching entry; account-data-taxonomy.md § The generation
    // machinery → Fleet-scope reclamation, clause (4))

    /// A separately-labelled group below the roster and before the custody
    /// facet, never intermixed with `device-card` rows: one
    /// `device-member-card` per verified fleet member no roster row accounts
    /// for (`DevicesSnapshot.members`, the shared derivation and fingerprint
    /// render). Renders NOTHING with no members — a settled honest fleet lists
    /// nobody, so the title and note never paint over an empty group
    /// (mirrors tui `member_elements`' own early return).
    @ViewBuilder
    private var memberSection: some View {
        let members = vm.snapshot?.members ?? []
        if !members.isEmpty {
            Text(L.devices.membersTitle)
                .font(.headline)
            automationText(Ids.deviceMemberNote, L.devices.memberNote)
                .font(.caption)
                .foregroundStyle(.secondary)
            ForEach(Array(members.enumerated()), id: \.element.deviceId) { index, member in
                MemberCard(vm: vm, member: member, index: index,
                           armedKey: $memberRemoveArmedKey)
            }
        }
    }

    // MARK: - T16 custody facet, owner side (devices.md § Custody facet, piece 2)

    /// The custodian-device rows — "who holds my data". A custody whose accept
    /// bound the host's NEST belongs to the Nests page's `nest-trust-custody-*`
    /// family instead: one custody never renders in both places, and
    /// `custodianNestUrl` is the marker that says which (the nest-custodian
    /// identity fact, ruled 2026-08-17). Hidden entirely when empty — an
    /// account with no custodians has nothing to say here, and a titled-but-
    /// empty section reads as a feature that failed to load (mirrors linux's
    /// `build_custody_section` / android's `CustodyHolderCard` gate).
    @ViewBuilder
    private var custodySection: some View {
        let custodyDeviceRows = vm.custodyRows.filter { $0.custodianNestUrl == nil }
        if !custodyDeviceRows.isEmpty {
            Text(L.devices.custodyHolderSection)
                .font(.headline)
            ForEach(Array(custodyDeviceRows.enumerated()), id: \.offset) { index, row in
                CustodyHolderCard(vm: vm, row: row, index: index)
            }
        }
    }

    // MARK: - T16 custody facet, host side (devices.md § Custody facet, piece 3)

    /// "What I hold for others" — one `custody-held-card` per custody this
    /// device holds for another account. Hidden until it holds something
    /// (linux's `build_held_section`). Cards are identified by grant id, so a
    /// re-fold that re-orders rows keeps each card's typed budget draft with
    /// its own custody.
    @ViewBuilder
    private var heldSection: some View {
        let held = vm.custodyHeld
        if !held.isEmpty {
            Text(L.devices.custodyHeldSection)
                .font(.headline)
            ForEach(Array(held.enumerated()), id: \.element.grantId) { index, row in
                CustodyHeldCard(vm: vm, row: row, index: index)
            }
        }
    }

    /// The consent cards — one `custody-offer-card` per offer waiting on this
    /// account. No section title: each card carries its own ("‹owner› asks
    /// this device…"), as on linux.
    @ViewBuilder
    private var offerSection: some View {
        ForEach(Array(vm.custodyOffers.enumerated()), id: \.element.grantId) { index, offer in
            CustodyOfferCard(vm: vm, offer: offer, index: index,
                             showsTarget: vm.custodyOfferTargets.contains(offer.grantId))
        }
    }
}

/// The counterpart account, abbreviated through the SAME shared `shortId`
/// every app uses, so an actor reads identically across the seven UIs.
private func shortActor(_ id: Data) -> String {
    shortId(hex: id.hexString)
}

// MARK: - Held-for-others card

/// One `custody-held-card`: the owner, the scope, the metered bytes against the
/// budget, the budget input (commit on Return), stop and remove. Every control
/// is live — each runs one shared custody act and answers on `error-message`.
private struct CustodyHeldCard: View {
    let vm: DevicesMachineVM
    let row: CustodyHeldRowView
    let index: Int

    /// The budget input's draft. Seeded from the fold's shared `budget_draft`
    /// text and re-seeded whenever that seed changes (a committed budget comes
    /// back formatted from the persisted cap — "3 GB", not the typed "3072
    /// MB"); typing alone never writes anything.
    @State private var draft = ""

    private var seed: String { renderLocalizedText(row.budgetDraft) }

    private var scopeText: String {
        guard let scopes = row.scopes, !scopes.wholeAccount else {
            return L.devices.custodyHeldScopeAccount
        }
        return scopes.scopes.joined(separator: ", ")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            automationText(Ids.custodyHeldOwner, L.devices.custodyHeldOwner(owner: shortActor(row.owner)))
                .font(.headline)
            automationText(Ids.custodyHeldScope, scopeText)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.custodyHeldBytes, ValueFormat.custodyHeldBytesText(row.receipt))
                .font(.caption)
            LabeledContent(L.devices.custodyBudgetLabel) {
                TextField(L.devices.custodyBudgetLabel, text: $draft)
                    .multilineTextAlignment(.trailing)
                    .onSubmit(commitBudget)
            }
            .font(.caption)
            .accessibilityIdentifier(Ids.custodyHeldBudgetInput)
            .automationField(Ids.custodyHeldBudgetInput, text: $draft, commit: commitBudget)
            HStack(spacing: 12) {
                // Stopping pauses the hold; it does not free the space. Once
                // stopped the control says so and disables — the bytes stay
                // until the custody is removed.
                Button(row.stopped ? L.devices.custodyStoppedBytesRemain : L.devices.custodyStop) {
                    stop()
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .disabled(row.stopped)
                .accessibilityIdentifier(Ids.custodyHeldStopButton)
                .automationActivate(Ids.custodyHeldStopButton, isEnabled: { !row.stopped }) { stop() }
                // Always available, stopped or not: stop is the pause and this
                // is the reclaim.
                Button(L.devices.custodyRemove, role: .destructive) {
                    remove()
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.custodyHeldRemoveButton)
                .automationActivate(Ids.custodyHeldRemoveButton) { remove() }
            }
        }
        .padding(.vertical, 2)
        .task(id: seed) { draft = seed }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.custodyHeldCard)
        .automationValue(Ids.custodyHeldCard, text: { row.owner.hexString })
        .automationScope(Ids.custodyHeldCard, index: index)
    }

    private func commitBudget() {
        let typed = draft
        Task { await vm.setCustodyBudget(grantId: row.grantId, typed: typed) }
    }

    private func stop() {
        Task { await vm.stopCustody(grantId: row.grantId) }
    }

    private func remove() {
        Task { await vm.removeCustody(grantId: row.grantId) }
    }
}

// MARK: - Offer consent card

/// One `custody-offer-card` — the consent surface. The floor copy is REQUIRED
/// before accept (`devices.md` § Custody facet piece 3): what this device would
/// see — the shape, never the content. The target select renders ONLY where
/// `showsTarget` says the nest choice is legal — absent otherwise, never
/// disabled — and the accept binds this device unless it chose the nest.
private struct CustodyOfferCard: View {
    let vm: DevicesMachineVM
    let offer: CustodyOfferRowView
    let index: Int
    let showsTarget: Bool

    @State private var onNest = false

    private var targetLabel: String {
        onNest ? L.devices.custodyOfferTargetNest : L.devices.custodyOfferTargetDevice
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.devices.custodyOfferTitle(owner: shortActor(offer.owner)))
                .font(.headline)
            automationText(Ids.custodyOfferFloorNote, L.devices.custodyOfferFloor)
                .font(.caption)
            if showsTarget {
                Picker(L.devices.custodyOfferTargetLabel, selection: $onNest) {
                    Text(L.devices.custodyOfferTargetDevice).tag(false)
                    Text(L.devices.custodyOfferTargetNest).tag(true)
                }
                .font(.caption)
                .accessibilityIdentifier(Ids.custodyOfferTargetSelect)
                .automationSelect(
                    Ids.custodyOfferTargetSelect,
                    value: { targetLabel },
                    options: { [L.devices.custodyOfferTargetDevice, L.devices.custodyOfferTargetNest] },
                    set: { onNest = ($0 == L.devices.custodyOfferTargetNest) }
                )
            }
            HStack(spacing: 12) {
                Button(L.devices.custodyOfferAccept) {
                    accept()
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.custodyOfferAcceptButton)
                .automationActivate(Ids.custodyOfferAcceptButton) { accept() }
                Button(L.devices.custodyOfferDecline) {
                    decline()
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.custodyOfferDeclineButton)
                .automationActivate(Ids.custodyOfferDeclineButton) { decline() }
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.custodyOfferCard)
        .automationValue(Ids.custodyOfferCard, text: { offer.owner.hexString })
        .automationScope(Ids.custodyOfferCard, index: index)
    }

    private func accept() {
        // Where the select is absent the accept binds this device, whatever
        // a stale `onNest` says.
        let nest = showsTarget && onNest
        Task { await vm.acceptCustody(grantId: offer.grantId, onNest: nest) }
    }

    private func decline() {
        Task { await vm.declineCustody(grantId: offer.grantId) }
    }
}

// MARK: - Offer initiation (the mint flow)

/// `custody-mint-*` — the owner asks a friend to hold sealed copies. The button
/// reads the host options (the account's 1:1 conversations — the request
/// travels over one); with none, the flow stays closed and `error-message`
/// says so. Open, it paints the host select, the REQUIRED floor copy, confirm
/// (live only once a real host is chosen) and cancel. v1 offers the Account
/// scope with the default window, so there is nothing else to choose.
private struct CustodyMintFlow: View {
    let vm: DevicesMachineVM

    /// `nil` = the flow is closed.
    @State private var candidates: [CustodyMintCandidateView]?
    /// The chosen candidate's index, `nil` = the placeholder.
    @State private var chosen: Int?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button(L.devices.custodyMintButton) {
                open()
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.custodyMintButton)
            .automationActivate(Ids.custodyMintButton) { open() }
            if let candidates {
                Picker(L.devices.custodyMintHostLabel, selection: $chosen) {
                    Text(L.devices.custodyMintHostPlaceholder).tag(Int?.none)
                    ForEach(Array(candidates.enumerated()), id: \.offset) { i, candidate in
                        Text(candidate.label).tag(Int?.some(i))
                    }
                }
                .accessibilityIdentifier(Ids.custodyMintHostSelect)
                .automationSelect(
                    Ids.custodyMintHostSelect,
                    value: { chosen.map { candidates[$0].label } ?? L.devices.custodyMintHostPlaceholder },
                    options: { [L.devices.custodyMintHostPlaceholder] + candidates.map(\.label) },
                    set: { label in chosen = candidates.firstIndex { $0.label == label } }
                )
                automationText(Ids.custodyMintFloorNote, L.devices.custodyMintFloor)
                    .font(.caption)
                HStack(spacing: 12) {
                    Button(L.devices.custodyMintConfirm) {
                        confirm()
                    }
                    .buttonStyle(.borderless)
                    .disabled(chosen == nil)
                    .accessibilityIdentifier(Ids.custodyMintConfirmButton)
                    .automationActivate(Ids.custodyMintConfirmButton, isEnabled: { chosen != nil }) { confirm() }
                    Button(L.common.cancel, role: .cancel) {
                        close()
                    }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier(Ids.custodyMintCancelButton)
                    .automationActivate(Ids.custodyMintCancelButton) { close() }
                }
            }
        }
        .padding(.vertical, 2)
    }

    private func open() {
        Task {
            let found = await vm.openCustodyMint()
            chosen = nil
            candidates = found
        }
    }

    private func confirm() {
        guard let candidates, let chosen, candidates.indices.contains(chosen) else { return }
        let candidate = candidates[chosen]
        Task {
            if await vm.mintCustody(candidate) { close() }
        }
    }

    private func close() {
        candidates = nil
        chosen = nil
    }
}

// MARK: - Device card

/// One registered device, off a shared `DeviceSummary`. Lifts the prior
/// per-target `DeviceCardView` recipe (`.accessibilityElement(children: .contain)`
/// + child IDs) into FaunaKit. Removal is direct (no confirm dialog) — the
/// canonical cross-app behaviour, matching `device-remove-button` with no
/// `device-remove-confirm` element.
private struct DeviceCard: View {
    let vm: DevicesMachineVM
    let device: DeviceSummary
    let index: Int
    let localDeviceId: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                automationText(Ids.deviceName, device.label)
                    .font(.headline)
                Spacer()
                HStack(spacing: 4) {
                    Circle()
                        .fill(device.online ? .green : .gray)
                        .frame(width: 8, height: 8)
                    automationText(Ids.deviceStatus, renderLocalizedText(deviceStatusLabel(online: device.online)))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }

            // The guardian-enrolled-device marker (family-safety.md § Full
            // visibility for young children, Slice F) — the ward's OWN list
            // renders it: the child must always see which device their
            // guardian enrolled (transparency by construction). Always false
            // on an unsupervised account, so the badge is simply absent then
            // (mirrors linux `roster.rs`'s `if device.guardian_marked`).
            if device.guardianMarked {
                automationText(Ids.deviceGuardianMarkBadge, L.devices.guardianMarkedBadge)
                    .font(.caption)
                    .foregroundStyle(.tint)
            }

            // Piece 1 of the custody facet (`devices.md` § Custody facet): an
            // own device enrolled relay-only wears the DERIVED posture — a
            // fact, never a toggle. The shared `keyless_posture` rule decides
            // (`DevicesMachineVM.keylessPrincipals`); every unknown reads as
            // "not keyless", so the marker never rests on a guess.
            if let principal = device.principal, vm.keylessPrincipals.contains(principal) {
                automationText(Ids.deviceKeylessPostureBadge, L.devices.keylessPostureBadge)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            // Not mutually exclusive with the guardian badge above — a
            // guardian marking their own enrolled device can legitimately
            // carry both (`devices.md` § This-device marker).
            if device.deviceId == localDeviceId {
                automationText(Ids.deviceThisMarkBadge, L.devices.thisDeviceBadge)
                    .font(.caption)
                    .foregroundStyle(.tint)
                // This device's own key fingerprint, beside the marker — the
                // user's half of the member group's comparison, through the
                // SAME shared-Rust formatter every `device-member-fingerprint`
                // uses (`devices.md` § Members without a matching entry).
                // Absent until the runtime answered.
                if let fingerprint = vm.snapshot?.ownFingerprint {
                    automationText(Ids.deviceOwnFingerprint, L.devices.ownFingerprint(fingerprint: fingerprint))
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
            }

            HStack(spacing: 4) {
                Text(shortId(hex: device.deviceId))
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Spacer()
            }

            // `device-p2p-participation-toggle` (`p2p.md` § Per-device
            // participation — rule 5's off switch; ID user-approved
            // 2026-09-25). The machine paints it per row
            // (`DeviceSummary.p2pParticipationPaint`: own-ness, checked, label,
            // actionable) and picks the arm the click takes, painting any
            // refusal on `error-message`; this view only draws it.
            let paint = participationPaint
            Toggle(isOn: Binding(get: { paint.checked }, set: { _ in toggleParticipation() })) {
                Text(paint.label)
                    .font(.caption)
            }
            .toggleStyle(.switch)
            .controlSize(.small)
            .disabled(!paint.actionable)
            .accessibilityIdentifier(Ids.deviceP2pParticipationToggle)
            // One Entry carries the click, the "on"/"off" read (→ `checked`),
            // the label text AND the enabled state; `isEnabled` mirrors the
            // `.disabled(...)` predicate above. Env-gated no-op.
            .automationActivate(
                Ids.deviceP2pParticipationToggle,
                isEnabled: { paint.actionable },
                text: { paint.label },
                value: { paint.checked ? "on" : "off" }
            ) { toggleParticipation() }

            HStack {
                if !device.folders.isEmpty {
                    HStack(spacing: 4) {
                        ForEach(device.folders, id: \.name) { fs in
                            DeviceFolderRoleBadge(
                                originates: fs.originates, accepts: fs.accepts,
                                appliesDeletes: fs.appliesDeletes)
                        }
                    }
                }
                Spacer()
                Button(L.common.remove, role: .destructive) {
                    removeDevice()
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.deviceRemoveButton)
                // Same `removeDevice()` the Button action runs. Env-gated no-op.
                .automationActivate(Ids.deviceRemoveButton) { removeDevice() }
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.deviceCard)
        // Presence anchor for the indexed `device-card` row (one Entry per card).
        // Env-gated no-op in production.
        .automationValue(Ids.deviceCard, text: { device.label })
        .automationScope(Ids.deviceCard, index: index)
    }

    private func removeDevice() {
        Task { await vm.removeDevice(index: index) }
    }

    /// The toggle's paint, drawn by the devices machine at every snapshot
    /// (`p2p.md` § Per-device participation → *Which row is this device's*);
    /// the app never re-derives own-ness. `nil` only on a row serialised
    /// without it (fixtures): the sibling arm over the row's own report,
    /// mirroring Rust's `DeviceSummary::participation_paint`.
    private var participationPaint: (checked: Bool, label: String, actionable: Bool) {
        if let paint = device.p2pParticipationPaint {
            return (paint.checked, renderLocalizedText(paint.label), paint.actionable)
        }
        let checked = device.p2pParticipation ?? true
        let label: String
        if device.p2pOffRequested {
            label = L.devices.p2pParticipationOffRequested
        } else if device.p2pParticipation == nil {
            label = L.devices.p2pParticipationUnreported
        } else {
            label = L.devices.p2pParticipation
        }
        return (checked, label, checked && !device.p2pOffRequested)
    }

    private func toggleParticipation() {
        let on = !participationPaint.checked
        Task { await vm.setP2pParticipation(index: index, on: on) }
    }
}

// MARK: - Custody holder card

/// One `custody-holder-card` — a cross-account custodian device holding
/// sealed copies of this account's planes (`devices.md` § Custody facet,
/// piece 2). Pieces 1 and 3 render beside it — the keyless-posture badge on
/// `DeviceCard`, the held-for-others and consent cards below.
private struct CustodyHolderCard: View {
    let vm: DevicesMachineVM
    let row: CustodyHolderRowView
    let index: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            // The counterpart account, abbreviated through the SAME shared
            // `shortId` every app uses, so an actor reads identically across
            // the seven UIs.
            automationText(Ids.custodyHolderName, shortId(hex: row.host.hexString))
                .font(.headline)
            // Three states, three strings — never collapsed, never empty (the
            // A7 honesty rule): a stale custodian must read as degraded
            // redundancy the owner can see, not as an absent row.
            automationText(Ids.custodyHolderReceiptStatus, ValueFormat.custodyReceiptStatusText(row.receipt))
                .font(.caption)
                .foregroundStyle(row.receiptState == .stale ? .red : .secondary)
            automationText(Ids.custodyHolderHeldBytes, ValueFormat.custodyHeldBytesText(row.receipt))
                .font(.caption)
                .foregroundStyle(.secondary)
            // The honest bound, stated beside the control it bounds (REQUIRED
            // — `nests.md` § Trust facet, custody rows): revoking stops
            // future carriage and serving on honest boxes; copies already
            // held stay held, and stay sealed forever. A pending ceremony has
            // minted nothing to revoke, so the note would over-promise there
            // and the control is disabled instead.
            if !row.pending {
                Text(L.devices.custodyRevokeBoundNote)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Button(L.devices.custodyRevoke, role: .destructive) {
                revoke()
            }
            .buttonStyle(.borderless)
            .controlSize(.small)
            // A control that cannot succeed is not offered: while the
            // ceremony is pending there is no minted grant to revoke and no
            // bound holder to name.
            .disabled(row.pending)
            .accessibilityIdentifier(Ids.custodyHolderRevokeButton)
            .automationActivate(Ids.custodyHolderRevokeButton, isEnabled: { !row.pending }) { revoke() }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.custodyHolderCard)
        .automationValue(Ids.custodyHolderCard, text: { row.host.hexString })
        .automationScope(Ids.custodyHolderCard, index: index)
    }

    private func revoke() {
        Task { await vm.revokeCustody(grantId: row.grantId, holder: row.custodianKey) }
    }
}

// MARK: - Member card (signed-in devices without a matching entry)

/// One `device-member-card` — a verified fleet member no roster row accounts
/// for (`DevicesSnapshot.members`, `devices.md` § Members without a matching
/// entry). Such a member has NO name (the label is the nest row's, exactly
/// what cannot be trusted here): the card carries its key fingerprint and the
/// sign-in time the device itself claims — its own self-signed word, a hint
/// never proof. `armedKey` is the PAGE's single armed slot
/// (`DevicesContent.memberRemoveArmedKey`, a fleet id) — arming a new card
/// silently retargets it, mirroring `AdminCustodyHostingView`'s `armedKey`
/// idiom. Removal is BY KEY through `DevicesMachine::remove_member_by_id`
/// with the armed card's own id — no nest row is touched. `index` is only the
/// automation scope's ordinal, never an address.
private struct MemberCard: View {
    let vm: DevicesMachineVM
    let member: FleetMemberSummary
    let index: Int
    @Binding var armedKey: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            automationText(Ids.deviceMemberFingerprint,
                            L.devices.memberFingerprint(fingerprint: member.fingerprint))
                .font(.callout.monospaced())
            // The member's own self-signed word about when it signed in — the
            // custody receipt line's precedent: the snapshot carries the
            // instant, the app formats it locally.
            automationText(Ids.deviceMemberEnrolledAt,
                            L.devices.memberEnrolledAt(when: formatUnixLocalMs(ms: member.enrolledAtMs)))
                .font(.caption)
                .foregroundStyle(.secondary)

            if armedKey == member.deviceId {
                HStack(spacing: 12) {
                    Button(L.devices.memberRemoveConfirm, role: .destructive) {
                        confirmRemove()
                    }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.deviceMemberRemoveConfirmButton)
                    .automationActivate(Ids.deviceMemberRemoveConfirmButton) { confirmRemove() }
                    Button(L.common.cancel, role: .cancel) {
                        armedKey = nil
                    }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.deviceMemberRemoveCancelButton)
                    .automationActivate(Ids.deviceMemberRemoveCancelButton) { armedKey = nil }
                }
            } else {
                HStack {
                    Spacer()
                    Button(L.common.remove, role: .destructive) {
                        armedKey = member.deviceId
                    }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.deviceMemberRemoveButton)
                    .automationActivate(Ids.deviceMemberRemoveButton) { armedKey = member.deviceId }
                }
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.deviceMemberCard)
        .automationValue(Ids.deviceMemberCard, text: { member.fingerprint })
        .automationScope(Ids.deviceMemberCard, index: index)
    }

    private func confirmRemove() {
        armedKey = nil
        let deviceId = member.deviceId
        Task { await vm.removeMember(deviceId: deviceId) }
    }
}
