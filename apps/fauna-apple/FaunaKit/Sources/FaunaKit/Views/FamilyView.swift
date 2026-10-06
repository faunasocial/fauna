import SwiftUI

/// The shared **Family** page (both apple apps, priority #2) — the
/// family-safety client surface (family-safety.md § App surface), lifted from
/// the landed windows / linux / web references onto SwiftUI. One view renders
/// both role-dependent sections off a single `fauna.family.status` read:
///
/// - **Guardian section** (rendered when the caller guards ≥1 account): the ward
///   rows, the ONE shared reach-policy editor (ui.yaml carries exactly one,
///   non-indexed, element set — so a guardian with several wards taps a
///   `family-ward-item` row to load that ward into it), the approvals queue
///   (NOT ward-scoped — one read returns every ward's), contact pre-approval,
///   and graduation (reveal-then-confirm).
/// - **Supervised section** (rendered when the caller is supervised): who
///   supervises me + the active policy, read-only.
///
/// Reached via the gated `family-tab` (macOS sidebar row / iOS More entry) and
/// from the global `SupervisedIndicatorBar`.
///
/// ⚠ **Eager `ScrollView { VStack { GroupBox } }`, never a `Form`.** On iOS a
/// SwiftUI `Form` is a lazy `List`: rows below the fold never fire the
/// `.onAppear` the in-process `AutomationRegistry` rides, so they stay
/// unregistered and the driver reads them as absent (`apple-e2e-automation.md`
/// § limitation (b) — the A1 mail-settings root cause). This page is taller than
/// one screen on iOS, so it uses the eager shape every `Admin*View` uses.
public struct FamilyView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = FamilyVM()
    /// macOS passes `navGeneration` so a *re*-navigation to the same page
    /// refetches: the e2e's `reload()` re-enters `family` via the state protocol
    /// and then polls, which a first-mount-only `.task` would never satisfy
    /// (the trap linux hit — it needed a nav-patch refresh hook for exactly this).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) {
        self.reloadToken = reloadToken
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                automationText(Ids.familyHeading, L.family.title)
                    .font(.title2)

                // Page error surface. Absent from the tree when nil — a
                // registered-but-empty element would read as present.
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                // Incoming transfer prompts — ABOVE the role sections (any user
                // can be a proposed guardian; family-safety.md § Graduation &
                // transfer → Visibility). This group, not the role sections
                // below, is what the widened family-tab gate exists to reach.
                incomingTransfersSection

                if let guardian = vm.supervisedBy {
                    supervisedSection(guardian)
                }

                // Guardian side. Mirrors linux: the ward/approval groups render
                // iff the caller actually guards someone.
                if !vm.wards.isEmpty {
                    wardsSection
                    if vm.selectedWard != nil {
                        policyEditor
                        contactAddSection
                        transferSection
                        graduateSection
                    }
                    approvalsSection
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        // Keyed on the session's client AND the nav token, not the token alone: iOS
        // reaches this page as a More destination, which the switch teardown does not
        // unmount, so a token-only key left the outgoing account's guardian, policy
        // and ward roster rendered under the incoming account — a family graph naming
        // real people it has no relationship with (`FamilyVM.reset()` spells it out).
        // `SessionKey` carries the token, so the existing nav-patch refetch is
        // unchanged. `account-scoping.md` § The scoping taxonomy, the "reused shell"
        // case; macOS unmounts the window shell wholesale, so there the drop is
        // redundant — carried for uniformity, as `SearchVM`'s is.
        .task(id: SessionKey(client, reloadToken: reloadToken)) {
            guard let client else {
                vm.reset()
                return
            }
            await vm.configure(api: client.api)
        }
    }

    // MARK: - Supervised section

    @ViewBuilder
    private func supervisedSection(_ guardian: FfiFamilyGuardianInfo) -> some View {
        GroupBox(L.family.policySummaryHeading) {
            VStack(alignment: .leading, spacing: 8) {
                automationText(
                    Ids.familyGuardianHandle,
                    L.family.guardianLabel(guardian: guardian.handle))
                    .font(.headline)
                // The active policy, read-only. `policy` is absent only if the
                // nest sent none; the unsupervised-equivalent default is the
                // honest render then (it is what is enforced).
                automationText(
                    Ids.familyPolicySummary,
                    FamilyVM.policySummary(
                        vm.myPolicy ?? FfiReachPolicy(
                            contactApproval: false,
                            unknownSenderMail: "allow",
                            federationContact: true,
                            feedSources: "allow",
                            contentPolicy: nil,
                            screenTime: nil,
                            contentNotify: nil,
                            unknownPeerDm: nil),
                        usageTodayMinutes: vm.myUsageTodayMinutes))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                // The ward's own band + how it was established — absent, never
                // placeholdered, when there is no band (family-safety.md § App
                // surface → *Age-band surfaces*).
                if let band = FamilyVM.ageBandText(vm.myAgeBand, own: true) {
                    automationText(Ids.familyAgeBandSummary, band)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: - Guardian section: wards

    private var wardsSection: some View {
        GroupBox(L.family.wardsHeading) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(vm.wards.enumerated()), id: \.element.actorId) { offset, ward in
                    wardRow(ward, offset: offset)
                    if offset < vm.wards.count - 1 { Divider() }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func wardRow(_ ward: FfiFamilyWardInfo, offset: Int) -> some View {
        let isSelected = vm.selectedWardId == ward.actorId
        return VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 8) {
                automationText(Ids.familyWardHandle, ward.handle)
                Spacer()
                if isSelected {
                    Image(systemName: "checkmark")
                        .foregroundStyle(.tint)
                }
            }
            // The ward's band + provenance, only when the ward has a nameable
            // band (a band-less admission paints no row — never a placeholder).
            if let band = FamilyVM.ageBandText(ward.ageBand, own: false) {
                automationText(Ids.familyWardAgeBand, band)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // Guardian Notify readout — one "{category}: {count}" line per
            // flagged category today, rendered only when non-empty so a ward
            // with nothing flagged stays clean. Mirrors android's placement
            // directly under the handle, inside the same indexed ward row.
            if !ward.contentNotices.isEmpty {
                automationText(Ids.familyWardContentNotices, FamilyVM.contentNoticesText(ward.contentNotices))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            // Screen-time usage readout — rendered only while this ward has a
            // daily budget set (no accounting without one), on the SAME terms
            // as the Notify readout above: the same number the ward's own
            // read-only summary shows (transparency).
            if let used = ward.usageTodayMinutes {
                automationText(
                    Ids.familyWardUsageToday,
                    FamilyVM.usageTodayText(usedMinutes: used, budgetMinutes: ward.policy.screenTime?.dailyMinutes)
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 8)
        .contentShape(Rectangle())
        // Tapping a row loads that ward into the shared editor — the same
        // `loadEditor` the in-process `automationActivate` fires, so a human tap
        // and the driver can't diverge.
        .onTapGesture { vm.loadEditor(wardId: ward.actorId) }
        // Container id + `.contain` so the row's `family-ward-handle` child stays
        // queryable alongside the row's own id (apple container-a11y rule).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.familyWardItem)
        .automationActivate(Ids.familyWardItem) { vm.loadEditor(wardId: ward.actorId) }
        .automationScope(Ids.familyWardItem, index: offset)
    }

    // MARK: - Guardian section: the ONE shared reach-policy editor

    private var policyEditor: some View {
        GroupBox(vm.selectedWard?.handle ?? "") {
            VStack(alignment: .leading, spacing: 12) {
                policyToggle(
                    L.family.policyContactApprovalLabel,
                    Ids.familyPolicyContactApprovalToggle,
                    Binding(get: { vm.draftContactApproval },
                            set: { vm.draftContactApproval = $0 }))

                policySelect(
                    L.family.policyUnknownSenderLabel,
                    "family-policy-unknown-sender-select",
                    options: unknownSenderOptions(),
                    label: unknownSenderLabel(value:),
                    wire: Binding(get: { vm.draftUnknownSender },
                                  set: { vm.draftUnknownSender = $0 }))

                policyToggle(
                    L.family.policyFederationLabel,
                    Ids.familyPolicyFederationToggle,
                    Binding(get: { vm.draftFederation },
                            set: { vm.draftFederation = $0 }))

                policySelect(
                    L.family.policyFeedSourcesLabel,
                    "family-policy-feed-sources-select",
                    options: feedSourcesOptions(),
                    label: feedSourcesLabel(value:),
                    wire: Binding(get: { vm.draftFeedSources },
                                  set: { vm.draftFeedSources = $0 }))

                // The honest v1 bound on `feed_sources`, which family-safety.md
                // § Guardian policy pillar 1 requires be stated on ANY surface
                // exposing the knob (linux + web render it here too). Prose, not
                // an assertable element — deliberately no test id.
                Text(L.family.policyFeedSourcesCaveat)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                // The bridge-DM gate (family-safety.md § The bridge-DM gate):
                // whether a DM arriving over an already-connected bridge
                // account, from a peer the ward has never messaged, is held
                // for guardian review — the twin `feed_sources` cannot cover
                // (that one only gates *new* connections). `unknownPeerDm` is
                // `Option<String>` on the wire, so the setter here also arms
                // `unknownPeerDmTouched` — the only thing allowed to send it.
                policySelect(
                    L.family.policyUnknownPeerDmLabel,
                    "family-policy-unknown-peer-dm-select",
                    options: unknownPeerDmOptions(),
                    label: unknownPeerDmLabel(value:),
                    wire: Binding(get: { vm.draftUnknownPeerDm },
                                  set: { vm.draftUnknownPeerDm = $0; vm.unknownPeerDmTouched = true }))

                // ── The v1.x content pillar (family-safety.md § Content policy)
                // — four per-category render floors over the one shared
                // `contentFloorOptions()` catalog, so their value list cannot
                // drift from what `fauna.family.policy.update` accepts, plus
                // the Guardian Notify knob. Same editor, same save button.
                Divider()

                contentFloorSelect(L.family.policyContentNsfwLabel, "nsfw",
                                   Binding(get: { vm.draftContentNsfw },
                                           set: { vm.draftContentNsfw = $0 }))
                contentFloorSelect(L.family.policyContentSpamLabel, "spam",
                                   Binding(get: { vm.draftContentSpam },
                                           set: { vm.draftContentSpam = $0 }))
                contentFloorSelect(L.family.policyContentPhishingLabel, "phishing",
                                   Binding(get: { vm.draftContentPhishing },
                                           set: { vm.draftContentPhishing = $0 }))
                contentFloorSelect(L.family.policyContentCommercialLabel, "commercial",
                                   Binding(get: { vm.draftContentCommercial },
                                           set: { vm.draftContentCommercial = $0 }))

                policyToggle(
                    L.family.policyContentNotifyLabel,
                    Ids.familyPolicyContentNotifyToggle,
                    Binding(get: { vm.draftContentNotify },
                            set: { vm.draftContentNotify = $0 }))

                // ── The v1.x screen-time pillar (family-safety.md § Screen
                // time) — a usage window ("HH:MM"..HH:MM", wrapping allowed)
                // and a daily budget in whole minutes. Typed exactly as the
                // guardian enters them; parsing + the half-set-pair / range
                // refusal are shared Rust, applied at Save (**write no policy
                // logic here**). Same editor, same save button.
                Divider()

                Text(L.family.policyScreenHeading)
                    .font(.headline)
                screenTimeField(L.family.policyScreenWindowStartLabel, Ids.familyPolicyScreenWindowStartInput,
                                Binding(get: { vm.draftScreenWindowStart },
                                        set: { vm.draftScreenWindowStart = $0 }))
                screenTimeField(L.family.policyScreenWindowEndLabel, Ids.familyPolicyScreenWindowEndInput,
                                Binding(get: { vm.draftScreenWindowEnd },
                                        set: { vm.draftScreenWindowEnd = $0 }))
                screenTimeField(L.family.policyScreenDailyMinutesLabel, Ids.familyPolicyScreenDailyMinutesInput,
                                Binding(get: { vm.draftScreenDailyMinutes },
                                        set: { vm.draftScreenDailyMinutes = $0 }))
                Text(L.family.policyScreenCaveat)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)

                Button(L.family.policySaveButton) {
                    Task { await vm.savePolicy() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(vm.isSaving)
                .accessibilityIdentifier(Ids.familyPolicySaveButton)
                .automationActivate(
                    Ids.familyPolicySaveButton,
                    isEnabled: { !vm.isSaving }
                ) {
                    Task { await vm.savePolicy() }
                }

                // ── The guardian-enrolled-device marker (family-safety.md §
                // Full visibility for young children, Slice F) — one
                // `family-device-mark-item` per device of the SELECTED ward.
                // Lives inside this editor, not on the ward rows, for the
                // same reason the policy knobs do: the index space then
                // belongs to exactly one ward, so `family-device-mark-toggle[i]`
                // is unambiguous with several wards on the page. Unlike the
                // policy knobs above, each toggle is NOT batched behind Save
                // — `fauna.family.device.mark` is its own per-device RPC, so
                // a flip dispatches immediately and re-reads (mirrors linux's
                // `devices_list`/web's `.devices` block, both inside the same
                // editor group).
                Divider()
                deviceMarkSection

                // ── The guardian's denied bridge-DM peers and the one-click flip
                // back (family-safety.md § The bridge-DM gate → *The un-deny
                // surface*). Inside the same editor, beside the device list, so the
                // index space belongs to exactly one ward at a time — the same
                // reason `deviceMarkSection` lives here rather than on the ward rows.
                Divider()
                blockedPeersSection
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// The guardian's DENIED bridge-DM peers, and the one-click flip back.
    ///
    /// Exists because a deny was otherwise a **one-way door in the UI**: the flip has
    /// always been wire-supported, but nothing named the peer to flip.
    /// `FfiFamilyWardInfo.blockedDmPeers` is that read. Only `block` verdicts arrive
    /// here (the nest filters), so there is no "allowed peers" roster: an allowed
    /// peer is simply un-held, and listing them would read as a roster the guardian
    /// must curate rather than a list of decisions they can undo.
    @ViewBuilder
    private var blockedPeersSection: some View {
        if let ward = vm.selectedWard {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.family.blockedPeersHeading)
                    .font(.headline)
                Text(L.family.blockedPeersHint(handle: ward.handle))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if ward.blockedDmPeers.isEmpty {
                    Text(L.family.noBlockedPeers)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(Array(ward.blockedDmPeers.enumerated()), id: \.offset) { offset, peer in
                        blockedPeerRow(ward: ward, peer: peer, offset: offset)
                    }
                }
            }
        }
    }

    /// One `family-blocked-peer-item` row. The allow button is a CHILD of the row
    /// (`.accessibilityElement(children: .contain)`), so a scoped read resolves it,
    /// and it closes over ITS OWN row's peer — the whole record, `(bridgeId, peerId)`
    /// together — so a button can only ever un-deny the person whose row it sits in.
    /// (Pointing every button at row 0 would un-deny the wrong person while the
    /// surface still looked correct.)
    ///
    /// The row's text IS the peer id — the only name this nest has for an external
    /// bridge peer (it has no actor here, which is why the gate exists). It is read
    /// by id, never by position, so the nest's row order is not silently part of the
    /// contract.
    private func blockedPeerRow(ward: FfiFamilyWardInfo, peer: FfiFamilyBlockedPeer, offset: Int) -> some View {
        HStack(spacing: 8) {
            Text(peer.peerId)
                .font(.callout.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(L.family.blockedPeerAllow) {
                Task { await vm.allowBlockedPeer(wardId: ward.actorId, peer: peer) }
            }
            .accessibilityIdentifier(Ids.familyBlockedPeerAllowButton)
            .automationActivate(Ids.familyBlockedPeerAllowButton) {
                Task { await vm.allowBlockedPeer(wardId: ward.actorId, peer: peer) }
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.familyBlockedPeerItem)
        .automationValue(Ids.familyBlockedPeerItem, text: { peer.peerId })
        .automationScope(Ids.familyBlockedPeerItem, index: offset)
    }

    @ViewBuilder
    private var deviceMarkSection: some View {
        if let ward = vm.selectedWard {
            VStack(alignment: .leading, spacing: 8) {
                Text(L.family.wardDevicesHeading)
                    .font(.headline)
                Text(L.family.wardDevicesHint(handle: ward.handle))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if ward.devices.isEmpty {
                    Text(L.family.noWardDevices)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(Array(ward.devices.enumerated()), id: \.element.deviceId) { offset, device in
                        deviceMarkRow(ward: ward, device: device, offset: offset)
                    }
                }
            }
        }
    }

    /// One `family-device-mark-item` row — the toggle is a CHILD of the row
    /// (`.accessibilityElement(children: .contain)`), so a scoped read
    /// resolves it; a flat paint would return nothing for BOTH the marked and
    /// unmarked device — a false pass on the negative half, exactly how
    /// tui's ward-side badge shipped broken (fixed).
    ///
    /// The row's text is the device's DISPLAY IDENTITY (`device.label`, the
    /// nest-substituted short-id form when no plaintext label rests — the
    /// 2026-08-02 display-identity ruling), never the ward's own sealed
    /// label.
    private func deviceMarkRow(ward: FfiFamilyWardInfo, device: FfiFamilyWardDevice, offset: Int) -> some View {
        HStack(spacing: 8) {
            Text(device.label)
                .font(.callout)
                .frame(maxWidth: .infinity, alignment: .leading)
            Toggle(
                L.family.deviceMarkLabel,
                isOn: Binding(
                    get: { device.guardianMarked },
                    set: { newValue in
                        Task {
                            await vm.deviceMark(wardId: ward.actorId, deviceId: device.deviceId, marked: newValue)
                        }
                    }))
                .toggleStyle(.switch)
                .accessibilityIdentifier(Ids.familyDeviceMarkToggle)
                .automationActivate(
                    Ids.familyDeviceMarkToggle,
                    value: { device.guardianMarked ? "on" : "off" }
                ) {
                    Task {
                        await vm.deviceMark(wardId: ward.actorId, deviceId: device.deviceId, marked: !device.guardianMarked)
                    }
                }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.familyDeviceMarkItem)
        .automationValue(Ids.familyDeviceMarkItem, text: { device.label })
        .automationScope(Ids.familyDeviceMarkItem, index: offset)
    }

    /// A policy toggle carrying its ui.yaml id. **Plain-label `Toggle`** (never a
    /// custom-view label — the id must land on the underlying Switch), and the
    /// `value:` closure is the ONLY source of the driver's `get_attr(id,"state")`
    /// read: SwiftUI ignores `.accessibilityValue` on a Toggle. One `Entry`
    /// registers both the flip and the read.
    private func policyToggle(_ label: String, _ id: String, _ isOn: Binding<Bool>) -> some View {
        Toggle(label, isOn: isOn)
            // `.switch` — `.toggleStyle(.checkbox)` does not compile on iOS, and
            // this view is shared.
            .toggleStyle(.switch)
            .accessibilityIdentifier(id)
            .automationActivate(id, value: { isOn.wrappedValue ? "on" : "off" }) {
                isOn.wrappedValue.toggle()
            }
    }

    /// One screen-time text input (`family-policy-screen-*-input` —
    /// family-safety.md § Screen time). A plain typed field: the guardian's
    /// raw "HH:MM" / whole-minutes text is exactly what `FamilyVM.savePolicy`
    /// parses via the shared `parseTimeOfDay`/`parseDailyMinutes` at Save
    /// time — this view holds no parsing of its own.
    private func screenTimeField(_ labelText: String, _ id: String, _ text: Binding<String>) -> some View {
        TextField(labelText, text: text)
            #if os(iOS)
            .keyboardType(.numbersAndPunctuation)
            #endif
            .accessibilityIdentifier(id)
            .automationField(id, text: text)
    }

    /// One per-category content floor (`family-policy-content-{category}-select`
    /// — family-safety.md § Content policy). A thin spelling of `policySelect`
    /// over the shared `contentFloorOptions()`/`contentFloorLabel` catalog, so
    /// all four rows are one line each and cannot drift from one another.
    private func contentFloorSelect(
        _ labelText: String, _ category: String, _ wire: Binding<String>
    ) -> some View {
        policySelect(
            labelText,
            "family-policy-content-\(category)-select",
            options: contentFloorOptions(),
            label: contentFloorLabel(value:),
            wire: wire)
    }

    /// A policy select. The control's options and the driver's `select`/`get_text`
    /// both speak the **localized label**; `wire` holds the value the nest stores.
    /// Both directions route through the shared `options`/`label` catalog
    /// (`FamilyReachPolicyFormat`), so an unrecognized value can only ever
    /// resolve to the strictest option — never `allow`.
    private func policySelect(
        _ labelText: String,
        _ id: String,
        options: [ReachPolicyOption],
        label: @escaping (String) -> LocalizedText,
        wire: Binding<String>
    ) -> some View {
        Picker(labelText, selection: wire) {
            ForEach(options, id: \.value) { option in
                Text(renderLocalizedText(option.label)).tag(option.value)
            }
        }
        .pickerStyle(.menu)
        .accessibilityIdentifier(id)
        .automationSelect(
            id,
            value: { renderLocalizedText(label(wire.wrappedValue)) }
        ) { picked in
            wire.wrappedValue = FamilyReachPolicyFormat.wire(
                forPickedLabel: picked, options: options, label: label)
        }
    }

    // MARK: - Guardian section: contact pre-approval

    private var contactAddSection: some View {
        HStack(spacing: 8) {
            TextField(
                L.family.contactAddPlaceholder,
                text: Binding(get: { vm.contactAddInput },
                              set: { vm.contactAddInput = $0 }))
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.familyContactAddInput)
                .automationField(
                    Ids.familyContactAddInput,
                    text: Binding(get: { vm.contactAddInput },
                                  set: { vm.contactAddInput = $0 }))

            Button(L.family.contactAddButton) {
                Task { await vm.addContact() }
            }
            .accessibilityIdentifier(Ids.familyContactAddButton)
            .automationActivate(Ids.familyContactAddButton) {
                Task { await vm.addContact() }
            }
        }
    }

    // MARK: - Guardian section: transfer initiation (per selected ward)

    /// Transfer initiation for the selected ward (family-safety.md §
    /// Graduation & transfer) — the same hex-actor-id convention as
    /// contact-add. While a proposal is outstanding the input+button swap for
    /// the nest-confirmed pending marker + cancel (never optimistic — driven
    /// by `selectedWard?.pendingTransfer`, refreshed on every `load()`). The
    /// two states are mutually exclusive `if`/`else` branches so the inactive
    /// one is fully ABSENT from the tree, not merely hidden.
    @ViewBuilder
    private var transferSection: some View {
        if let pending = vm.selectedWard?.pendingTransfer {
            HStack(spacing: 8) {
                automationText(
                    Ids.familyTransferPending,
                    L.family.transferPending(handle: pending.proposedGuardianHandle))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Button(L.family.transferCancelButton) {
                    Task { await vm.cancelTransfer() }
                }
                .accessibilityIdentifier(Ids.familyTransferCancelButton)
                .automationActivate(Ids.familyTransferCancelButton) {
                    Task { await vm.cancelTransfer() }
                }
                // Not a form cancel — cancelling a PENDING transfer is itself a
                // nest write (`fauna.family.transfer.cancel`), so unlike a
                // dialog's cancel this one declares.
                .faunaGate("fauna.family.transfer.cancel")
            }
        } else {
            HStack(spacing: 8) {
                TextField(
                    L.family.transferPlaceholder,
                    text: Binding(get: { vm.transferInput },
                                  set: { vm.transferInput = $0 }))
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.familyTransferInput)
                    .automationField(
                        Ids.familyTransferInput,
                        text: Binding(get: { vm.transferInput },
                                      set: { vm.transferInput = $0 }))

                Button(L.family.transferButton) {
                    Task { await vm.submitTransfer() }
                }
                .accessibilityIdentifier(Ids.familyTransferButton)
                .automationActivate(Ids.familyTransferButton) {
                    Task { await vm.submitTransfer() }
                }
                // The handle field beside it is buffer and stays live.
                .faunaGate("fauna.family.transfer")
            }
        }
    }

    // MARK: - Incoming transfer prompts (the proposed-guardian side)

    /// Absent from the tree when empty — a registered-but-empty group would
    /// read as present.
    @ViewBuilder
    private var incomingTransfersSection: some View {
        if !vm.incomingTransfers.isEmpty {
            GroupBox(L.family.incomingTransfersHeading) {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(vm.incomingTransfers.enumerated()), id: \.offset) { offset, entry in
                        incomingTransferRow(entry, offset: offset)
                        if offset < vm.incomingTransfers.count - 1 { Divider() }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func incomingTransferRow(_ entry: FfiFamilyIncomingTransfer, offset: Int) -> some View {
        HStack(spacing: 8) {
            // "{guardian} asks you to take over supervision of {ward}" — the
            // CURRENT guardian, which is the fact the prompt renders (the
            // initiator may have been the admin).
            Text(L.family.incomingTransferText(guardian: entry.guardianHandle, ward: entry.supervisedHandle))
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
            Button(L.family.incomingTransferAcceptButton) {
                Task { await vm.decideIncoming(entry, accept: true) }
            }
            .controlSize(.small)
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier(Ids.familyIncomingTransferAcceptButton)
            .automationActivate(Ids.familyIncomingTransferAcceptButton) {
                Task { await vm.decideIncoming(entry, accept: true) }
            }
            .faunaGate("fauna.family.transfer.accept")

            Button(L.family.incomingTransferDeclineButton) {
                Task { await vm.decideIncoming(entry, accept: false) }
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.familyIncomingTransferDeclineButton)
            .automationActivate(Ids.familyIncomingTransferDeclineButton) {
                Task { await vm.decideIncoming(entry, accept: false) }
            }
            // A decline is a nest write of its own, not a local dismiss —
            // matching tui's `DecideIncoming { accept }` split.
            .faunaGate("fauna.family.transfer.decline")
        }
        .padding(.vertical, 8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.familyIncomingTransferItem)
        .automationValue(Ids.familyIncomingTransferItem, text: {
            L.family.incomingTransferText(guardian: entry.guardianHandle, ward: entry.supervisedHandle)
        })
        .automationScope(Ids.familyIncomingTransferItem, index: offset)
    }

    // MARK: - Guardian section: graduation (reveal-then-confirm)

    private var graduateSection: some View {
        HStack(spacing: 8) {
            Button(L.family.graduateButton) {
                vm.graduateConfirmVisible = true
            }
            .accessibilityIdentifier(Ids.familyGraduateButton)
            .automationActivate(Ids.familyGraduateButton) {
                vm.graduateConfirmVisible = true
            }

            // Inline `@State`-driven reveal, not a `.confirmationDialog`: a system
            // presentation layer may not `.onAppear`-register its children
            // in-process (apple-e2e-automation.md), and the e2e reads this
            // button's text for the ward handle.
            if vm.graduateConfirmVisible, let ward = vm.selectedWard {
                Button(L.family.graduateConfirmButton(handle: ward.handle), role: .destructive) {
                    Task { await vm.graduate() }
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier(Ids.familyGraduateConfirmButton)
                .automationActivate(
                    Ids.familyGraduateConfirmButton,
                    value: { L.family.graduateConfirmButton(handle: ward.handle) }
                ) {
                    Task { await vm.graduate() }
                }
                // Arming is local: `family-graduate-button` above only reveals
                // this confirm, so it stays live with no nest — the confirm is
                // the commit and the only one that declares.
                .faunaGate("fauna.family.graduate")
            }
        }
    }

    // MARK: - Guardian section: the approvals queue

    private var approvalsSection: some View {
        GroupBox(L.family.approvalsHeading) {
            VStack(alignment: .leading, spacing: 0) {
                if vm.approvals.isEmpty {
                    // An empty queue is the empty state, not an error.
                    Text(L.family.noApprovals)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 8)
                } else {
                    ForEach(Array(vm.approvals.enumerated()), id: \.offset) { offset, entry in
                        approvalRow(entry, offset: offset)
                        if offset < vm.approvals.count - 1 { Divider() }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func approvalRow(_ entry: FfiFamilyApprovalEntry, offset: Int) -> some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                // A `mail_hold` renders its envelope `peerAddress`, never
                // `summary` (always empty for a hold — a subject is content,
                // sealed to the ward) — decided by the shared
                // `approvalDisplayText` FFI export; `nil` is the mail_hold
                // null reverse-path, which falls back to the localized
                // no-sender label.
                Text(approvalDisplayText(entry: entry) ?? L.family.approvalNoSender)
                    .font(.callout)
                Text(entry.supervisedHandle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button(L.family.approve) {
                Task { await vm.decide(entry, approve: true) }
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.familyApprovalApproveButton)
            .automationActivate(Ids.familyApprovalApproveButton) {
                Task { await vm.decide(entry, approve: true) }
            }

            Button(L.family.deny) {
                Task { await vm.decide(entry, approve: false) }
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.familyApprovalDenyButton)
            .automationActivate(Ids.familyApprovalDenyButton) {
                Task { await vm.decide(entry, approve: false) }
            }
        }
        .padding(.vertical, 8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.familyApprovalItem)
        .automationValue(Ids.familyApprovalItem, text: { approvalDisplayText(entry: entry) ?? L.family.approvalNoSender })
        .automationScope(Ids.familyApprovalItem, index: offset)
    }
}

/// The global **`supervised-indicator`** — permanent, non-dismissable chrome for
/// a supervised account, on every page ("This account is supervised by X"),
/// navigating to the Family page (family-safety.md § App surface; ui.yaml's
/// one conditionally-present global element).
///
/// Rendered for the **supervised** side only: a guardian who is not themself
/// supervised sees no indicator (the `family-tab` gate is the broader
/// guardian-OR-supervised one).
///
/// Both shells mount it in the app-root `.safeAreaInset(edge: .top)`, beside
/// `ConnectionStatusBar` — **outside** the sidebar/tab container on purpose: the
/// macOS sidebar is swapped out in place by the Settings and Admin shells, so a
/// sidebar-hosted indicator would vanish on exactly the pages a supervised user
/// is most likely to go looking at. (Linux placed it in the header bar for the
/// same reason; windows uses a page-level overlay.)
///
/// Takes the `@Observable FamilyStatusStore` by **reference**, not a snapshot
/// value: the in-process registry captures the `value` reader once on
/// `.onAppear` and calls it live, so a frozen value would pin the indicator at
/// its initial (empty) state.
public struct SupervisedIndicatorBar: View {
    private let store: FamilyStatusStore
    private let onOpenFamily: () -> Void

    public init(store: FamilyStatusStore, onOpenFamily: @escaping () -> Void) {
        self.store = store
        self.onOpenFamily = onOpenFamily
    }

    public var body: some View {
        // Absent from the tree when not supervised — a registered-but-empty
        // element would read as present to `is_visible`.
        if let guardian = store.supervisedByHandle {
            Button {
                onOpenFamily()
            } label: {
                Text(L.family.supervisedIndicator(guardian: guardian))
                    .font(.caption)
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 4)
            }
            .buttonStyle(.plain)
            .background(.thinMaterial)
            .contentShape(Rectangle())
            .accessibilityIdentifier(Ids.supervisedIndicator)
            .automationActivate(
                Ids.supervisedIndicator,
                value: { L.family.supervisedIndicator(guardian: guardian) }
            ) {
                onOpenFamily()
            }
        }
    }
}

/// The app-root projection of `fauna.family.status` that gates the two global
/// family surfaces, refreshed post-auth and on reconnect by each shell.
///
/// Two distinct gates, per family-safety.md § App surface:
/// - `hasRelationship` (guardian **OR** supervised **OR** a pending/incoming
///   transfer, § Graduation & transfer → Visibility) → the gated `family-tab`.
/// - `supervisedByHandle` (supervised only) → the global `supervised-indicator`.
///
/// **Split fail policy.** The guardian/wards/transfer half fails CLOSED: any
/// read error hides that contribution rather than leaking a family surface
/// onto an account that has none (the same rule as the `am-i-admin` gate;
/// linux's `check_family_status` logs and hides) — nothing persists that half,
/// so a failed read has no fallback to offer it. The SUPERVISED half instead
/// follows clause 1 (`family-safety.md` § Content policy, the unfetched-policy
/// ruling): "read failed" and "read says unsupervised" are different facts, so
/// a failed read must not undo a live or restored guardian — the same
/// keep-last-known rule `ContentPolicyStore`/`ScreenTimeStore` follow, split
/// out here as its own `restoredGuardianHandle` fallback (web's
/// `restoredGuardian` is the reference shape) so `clear()`'s fail-closed sweep
/// on the wards/transfer half cannot undo it.
///
/// The class is **not** actor-isolated — both `AppState` and `MacAppState` are
/// plain `@Observable` classes that construct it in a stored property, which a
/// `@MainActor` initializer could not serve (the same shape as the sibling
/// `SessionState`). Mutation is pinned to the main actor on `refresh` instead,
/// so the two gates only ever change where SwiftUI observes them.
@Observable
public final class FamilyStatusStore {
    /// The live read's guardian, once one has landed for this session — `nil`
    /// before the first read, after a failed read, or once a successful read
    /// reports unsupervised.
    private var liveSupervisedByHandle: String?
    /// Whether a LIVE read has reported a guardian/ward/transfer relationship.
    /// No snapshot covers this half (only the ward's OWN supervision is
    /// persisted), so it has no restore fallback and stays fail-closed.
    private var liveHasWardOrGuardianRelationship = false
    /// The restored last-known guardian from the persisted supervision
    /// snapshot (`family-safety.md` § Content policy, clause 2) — superseded
    /// the moment a live read lands for this session, even one reporting
    /// unsupervised: a read that says "no guardian" is the truth now.
    private var restoredGuardianHandle: String?

    /// The supervised caller's OWN pending contact asks, off the same status read
    /// (`family-safety.md` § Child-initiated contact requests → *Ward
    /// transparency*). Lives here — the app-root projection of the one
    /// `fauna.family.status` reply — rather than on the Contacts or Profile page,
    /// because both pages render "asked — waiting for your guardian" from it and
    /// each reads it back through ``contactAskPending(peerActorIdHex:)``. tui's
    /// `FamilyState::own_contact_requests` is the reference shape.
    ///
    /// **Gated on a live `supervisedBy`** where the read folds it in: a graduated
    /// account has no guardian to be waiting on, so a stale ask from the last read
    /// must not keep saying "asked — waiting" on a page that now sends freely (the
    /// nest drops the rows at graduation too; this is the client half of the same
    /// rule). Nothing persists these — an ask is live nest state, not enforcement.
    public private(set) var contactRequests: [FfiFamilyContactRequest] = []
    /// The supervised caller's OWN feed-source asks — pending *and*
    /// approved-but-unredeemed (`family-safety.md` § Feed-source approvals), gated
    /// on `supervisedBy` for the same reason. The Bridges page reads it back
    /// through ``feedRequestState(bridgeId:operation:target:)``.
    public private(set) var feedRequests: [FfiFamilyFeedRequest] = []

    public var supervisedByHandle: String? { liveSupervisedByHandle ?? restoredGuardianHandle }
    public var hasRelationship: Bool { liveHasWardOrGuardianRelationship || restoredGuardianHandle != nil }

    public init() {}

    /// The two live states a feed-source ask can be in, as
    /// `bridge-source-request-state` renders them.
    ///
    /// There is deliberately no `lapsed` member: the nest lists only live rows, so
    /// a dead ask is an ABSENT row, not a third state. Modelling one would invite a
    /// surface that says "expired" while the gate has already gone back to refusing
    /// outright.
    public enum FeedRequestState: Equatable, Sendable {
        /// Asked, no verdict yet.
        case pending
        /// Approved — a single-use grant the ward redeems by RETRYING the original
        /// operation. The surface prompts the retry; it never retries on its own
        /// (auto-retrying would spend the grant on a navigation the user did not
        /// ask for, and a lapsed grant would then look like a silent failure).
        case approved
    }

    /// Whether this (supervised) account has an ask outstanding for a peer —
    /// what `contact-request-pending` renders from. Case-insensitive on the hex
    /// id (the caller's is whatever the Find User form resolved; the wire's is
    /// canonical lowercase); an id that is not hex matches nothing, which is the
    /// honest answer.
    public func contactAskPending(peerActorIdHex: String) -> Bool {
        guard let want = Data(hexString: peerActorIdHex.lowercased()) else { return false }
        return contactRequests.contains { $0.peerActorId == want }
    }

    /// The live ask state for one feed-source operation, or `nil` when this
    /// account has no live ask for it — what `bridge-source-request-state`
    /// renders, and what decides whether `bridge-source-request-button` is
    /// offered at all.
    ///
    /// Keyed on the whole `(bridgeId, operation, target)` triple because that is
    /// what the grant is scoped to: a `follow` ask for one account must not light
    /// up the row of a different follow on the same bridge, and a `link` ask
    /// (whose `target` is empty by construction) must not match either.
    public func feedRequestState(bridgeId: String, operation: String, target: String) -> FeedRequestState? {
        feedRequests
            .first { $0.bridgeId == bridgeId && $0.operation == operation && $0.target == target }
            .map(Self.state(of:))
    }

    /// Every live ask on one bridge, in the nest's order — what a card paints as
    /// `bridge-source-request-state` rows (a card can carry several: a blocked link
    /// AND two blocked follows are three independent asks, each with its own
    /// verdict).
    public func feedRequestStates(bridgeId: String) -> [FeedRequestState] {
        feedRequests.filter { $0.bridgeId == bridgeId }.map(Self.state(of:))
    }

    /// `approvedAt` carries the approval INSTANT rather than a state string, so the
    /// two states are exactly "has one" / "does not".
    private static func state(of request: FfiFamilyFeedRequest) -> FeedRequestState {
        request.approvedAt == nil ? .pending : .approved
    }

    /// Restore (or clear) the supervised-side fallback from the persisted
    /// last-known supervision snapshot, ahead of the first `refresh` landing.
    /// `nil` clears the fallback, which is what makes this safe to call at
    /// every session establish, including the bare `client == nil` teardown
    /// phase: a departing account's restored guardian can never bleed into
    /// the next one's first paint — see `seedSupervisionSnapshot`.
    @MainActor
    func seed(from snapshot: FfiSupervisionSnapshot?) {
        restoredGuardianHandle = snapshot?.supervisedBy.handle
    }

    @MainActor
    public func refresh(api: APIClient?) async {
        guard let api else {
            clear()
            return
        }
        do {
            let status = try await api.familyStatus()
            liveSupervisedByHandle = status.supervisedBy?.handle
            liveHasWardOrGuardianRelationship = status.supervisedBy != nil || !status.wards.isEmpty
                || !status.incomingTransfers.isEmpty
            // The live reply supersedes any restored fallback — the choke
            // point has already re-persisted the snapshot from it.
            restoredGuardianHandle = nil
            // Gated on the LIVE read's guardian (never the restored fallback): the
            // asks come only off a live reply, and only a supervised account has a
            // guardian to be waiting on.
            let supervised = status.supervisedBy != nil
            contactRequests = supervised ? status.contactRequests : []
            feedRequests = supervised ? status.feedRequests : []
        } catch {
            // Fail closed on the guardian/wards/transfer half only; the
            // supervised half (`supervisedByHandle`) keeps whatever live or
            // restored value it already had (clause 1). The asks keep what the
            // last live read said: a failed re-read is not a withdrawn ask, and
            // dropping it would repaint a live "asked — waiting" as an offer to ask
            // again.
            liveHasWardOrGuardianRelationship = false
        }
    }

    @MainActor
    private func clear() {
        liveSupervisedByHandle = nil
        liveHasWardOrGuardianRelationship = false
        restoredGuardianHandle = nil
        contactRequests = []
        feedRequests = []
    }
}
