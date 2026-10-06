import SwiftUI

/// The consolidated `admin-users` hub — one surface for the whole user-admission
/// lifecycle, five sections (admin.md § Users): **Pending requests** (approve a
/// request at a chosen tier), **Registration** (the nest's registration
/// posture), **Admit** (direct admission of a known actor id), **Invite**
/// (mint a code at a tier), **Users** (the user list + change-tier + eviction
/// + the read-only serving indicator).
///
/// Shared FaunaKit view consumed by **both** macOS and iOS (priority #1/#2) over
/// the FFI-backed `AdminVM` (`fauna.admin.*` WS-RPC, no `/admin/api/*`). Admission
/// = assign a TIER; the tier pickers are the shared cycle-`Button` widget
/// (`tierCycleButton`, `AdminTierPickers.swift`) registered as `select` (ui.yaml
/// types `admin-users-tier-select` / `invite-request-row-tier-select` /
/// `admin-settings-tier-select`) — not a SwiftUI `Picker`, which XCUITest can't see.
public struct AdminUsersHubView: View {
    @Bindable var vm: AdminVM
    /// Reload trigger — when this changes the hub re-fetches. macOS passes the
    /// shell's `navGeneration` so each (re)navigation refreshes; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, which re-runs the load anyway).
    var reloadToken: Int = 0

    // Per-request approve-at tier (no nest call until approve) + deny reason.
    @State private var requestTier: [Int64: String] = [:]
    @State private var denyReason: [Int64: String] = [:]
    // Per-request guardian, as the picker's DISPLAY LABEL ("" = the "None"
    // sentinel = an ordinary, unsupervised admission). Local UI state until
    // approve, exactly like the tier (family-safety.md § Wire & data shape).
    @State private var requestGuardian: [Int64: String] = [:]
    // Per-request age band, as an `ageBandOptions()` VALUE; absent = seeded
    // from the applicant's claim (`claimedAgeBandOption`) — family-safety.md
    // § App surface → *Age-band surfaces*. Local UI state until approve.
    @State private var requestAgeBand: [Int64: String] = [:]
    // Invite section: mint tier + max-uses (raw int string) + guardian label
    // + age band (option VALUE).
    @State private var mintTier: String = ""
    @State private var maxUses: String = "1"
    @State private var mintGuardian: String = ""
    @State private var mintAgeBand: String = ageBandNotSetValue()
    // Registration section: mode draft (wire value, blank = unknown posture)
    // + free-tier-ceiling draft (raw string). Re-seeded from the persisted
    // posture on every load — unlike the invite form's drafts, this mirrors
    // nest state rather than transient form input (tui's `UsersState::reseed`).
    @State private var registrationModeDraft: String = ""
    @State private var maxFreeUsersInput: String = ""
    // The age require-knob's draft, re-seeded with the posture; the section's
    // one save sends it only when it changed (tui's `age_verification_draft`).
    @State private var ageVerificationDraft = false
    // Admit section drafts.
    @State private var admitActorInput: String = ""
    @State private var admitHandleInput: String = ""
    @State private var admitTierDraft: String = ""

    public init(vm: AdminVM, reloadToken: Int = 0) {
        self.vm = vm
        self.reloadToken = reloadToken
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                automationText(Ids.adminUsersHeading, L.admin.usersPage.title)
                    .font(.title)

                requestsSection
                Divider()
                registrationSection
                Divider()
                admitSection
                Divider()
                inviteSection
                Divider()
                usersSection

                if let err = vm.actionError {
                    Text(err)
                        .foregroundStyle(.red)
                        .font(.caption)
                        .accessibilityIdentifier(Ids.adminUsersActionError)
                        // Optional-inside-`if let`: keep the literal id + re-read
                        // live (the error may toggle between body passes).
                        .automationValue(Ids.adminUsersActionError, text: { vm.actionError })
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            await vm.loadHubUntilReady()
            if mintTier.isEmpty || !vm.tierNames.contains(mintTier) {
                mintTier = vm.tierNames.first ?? defaultInviteTier()
            }
            if admitTierDraft.isEmpty || !vm.tierNames.contains(admitTierDraft) {
                admitTierDraft = vm.tierNames.first ?? defaultInviteTier()
            }
            syncRegistrationDrafts()
        }
    }

    // MARK: - Section 1: Pending requests

    private var requestsSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            automationText(Ids.adminUsersRequestsSection, L.admin.usersPage.sectionRequests)
                .font(.headline)

            if vm.inviteRequests.isEmpty {
                Text(L.admin.inviteRequestsPage.empty)
                    .foregroundStyle(.secondary)
                    .font(.caption)
            } else {
                ForEach(vm.inviteRequests, id: \.id) { req in
                    requestRow(req)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.adminInviteRequestsList)
                        // Per-row read anchor (indexed) exposing the request handle.
                        .automationValue(Ids.adminInviteRequestsList, text: { req.handle })
                }
            }
        }
    }

    @ViewBuilder
    private func requestRow(_ req: FfiAdminInviteRequest) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            automationText(Ids.inviteRequestRowHandle, req.handle)
                .font(.headline)
            automationText(Ids.inviteRequestRowActor, hexFull(bytes: req.actorId))
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)

            if !req.message.isEmpty {
                automationText(Ids.inviteRequestRowMessage, req.message)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }

            HStack(spacing: 12) {
                tierMenu(
                    id: "invite-request-row-tier-select",
                    current: { requestTier[req.id] ?? vm.tierNames.first ?? defaultInviteTier() }
                ) { requestTier[req.id] = $0 }

                // The per-row guardian picker: admitting this request with a
                // guardian selected makes the account supervised, in the same
                // admission transaction (family-safety.md § Wire & data shape).
                // Local UI state until approve — no nest call on selection.
                guardianMenu(
                    id: "invite-request-row-guardian-select",
                    current: { requestGuardian[req.id] ?? "" }
                ) { picked in
                    // A band presupposes a guardian: clearing one clears the other.
                    if picked.isEmpty { requestAgeBand[req.id] = ageBandNotSetValue() }
                    requestGuardian[req.id] = picked
                }

                TextField(L.admin.inviteRequestsPage.denyReasonPlaceholder,
                          text: Binding(
                            get: { denyReason[req.id] ?? "" },
                            set: { denyReason[req.id] = $0 }))
                    .textFieldStyle(.roundedBorder)
                    .font(.caption)
                    .accessibilityIdentifier(Ids.inviteRequestRowDenyReasonField)
                    .automationField(Ids.inviteRequestRowDenyReasonField,
                                     text: Binding(
                                        get: { denyReason[req.id] ?? "" },
                                        set: { denyReason[req.id] = $0 }))
            }

            // The applicant's claim — total: absence IS the signal the admitting
            // admin reads (D6), so a request with no claim says so.
            HStack(spacing: 12) {
                automationText(
                    Ids.inviteRequestRowAgeClaim,
                    renderLocalizedTextNested(ageClaimLabel(band: req.ageBand,
                                                            provenance: req.ageBandProvenance)))
                    .font(.caption)
                    .foregroundStyle(.secondary)

                ageBandMenu(
                    id: Ids.inviteRequestRowAgeBandSelect,
                    current: { requestAgeBandDraft(req) },
                    enabled: { !(requestGuardian[req.id] ?? "").isEmpty }
                ) { requestAgeBand[req.id] = $0 }
            }

            HStack(spacing: 12) {
                Button(L.admin.inviteRequestsPage.approve) {
                    approveRequest(req)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.inviteRequestRowApproveButton)
                .automationActivate(Ids.inviteRequestRowApproveButton) {
                    approveRequest(req)
                }
                .faunaGate("fauna.admin.invite_requests.approve")

                Button(L.admin.inviteRequestsPage.deny, role: .destructive) {
                    denyRequest(req)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.inviteRequestRowDenyButton)
                .automationActivate(Ids.inviteRequestRowDenyButton) {
                    denyRequest(req)
                }
                .faunaGate("fauna.admin.invite_requests.deny")
            }
        }
        .padding(.vertical, 4)
    }

    /// Approve action — shared by the `Button` and `automationActivate`.
    private func approveRequest(_ req: FfiAdminInviteRequest) {
        let tier = requestTier[req.id] ?? vm.tierNames.first ?? defaultInviteTier()
        let guardian = vm.guardianActorId(forLabel: requestGuardian[req.id] ?? "")
        let band = requestAgeBandDraft(req)
        Task { await vm.approveRequest(id: req.id, tier: tier, guardian: guardian, ageBand: band) }
    }

    /// A request row's band draft — the admin's pick, else the applicant's
    /// claimed band when this client can name it, else *not set*.
    private func requestAgeBandDraft(_ req: FfiAdminInviteRequest) -> String {
        requestAgeBand[req.id] ?? claimedAgeBandOption(claimed: req.ageBand)
    }

    /// Deny action — shared by the `Button` and `automationActivate`.
    private func denyRequest(_ req: FfiAdminInviteRequest) {
        let reason = denyReason[req.id]
        Task { await vm.denyRequest(id: req.id, reason: reason) }
    }

    // MARK: - Section 2: Registration

    /// The nest's registration posture (admin.md § 2 Users → Section 2;
    /// public-mode.md § Registration Modes). A posture this client can't name
    /// — a newer mode string this build predates —
    /// renders READ-ONLY: never coerced to a guess a Save could overwrite the
    /// nest's real posture with (public-mode.md § Implementation status
    /// today). Mirrors tui's `registration_section`.
    private var registrationSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            automationText(Ids.adminUsersRegistrationSection, L.admin.usersPage.sectionRegistration)
                .font(.headline)

            if registrationModeDraft.isEmpty {
                automationText(
                    Ids.adminUsersRegistrationModeSelect,
                    L.admin.usersPage.registrationModeUnknown(mode: vm.registrationMode ?? "")
                )
                .foregroundStyle(.secondary)
                .font(.caption)
            } else {
                HStack(spacing: 12) {
                    registrationModePicker

                    TextField(L.admin.usersPage.maxFreeUsersLabel, text: $maxFreeUsersInput)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 90)
                        .accessibilityIdentifier(Ids.adminUsersMaxFreeUsersInput)
                        .automationField(Ids.adminUsersMaxFreeUsersInput, text: $maxFreeUsersInput)
                }

                // The age require-knob (family-safety.md § The account age band
                // D5+D6) — a draft in this section's one save, the family
                // page's toggle idiom ("on"/"off" value → the driver's `state`).
                Toggle(L.admin.usersPage.ageVerificationRequiredLabel, isOn: $ageVerificationDraft)
                    .toggleStyle(.switch)
                    .accessibilityIdentifier(Ids.adminUsersRegistrationAgeVerificationToggle)
                    .automationActivate(Ids.adminUsersRegistrationAgeVerificationToggle,
                                        value: { ageVerificationDraft ? "on" : "off" }) {
                        ageVerificationDraft.toggle()
                    }

                Button(L.admin.usersPage.registrationSave) {
                    confirmSaveRegistration()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminUsersRegistrationSaveButton)
                .automationActivate(Ids.adminUsersRegistrationSaveButton) {
                    confirmSaveRegistration()
                }
                .faunaGate("fauna.admin.set_registration_mode")
            }
        }
    }

    /// The mode picker, built off the shared `registrationModeOptions()`
    /// catalog — never hand-rolled wire values or labels (the same catalog
    /// tui/android/web/linux read). A real `Picker` (like `guardianMenu`, not
    /// the tier cycle-button): the in-process driver reads `automationSelect`,
    /// not the native AX tree, so `Picker` is findable here.
    private var registrationModePicker: some View {
        Picker(L.admin.usersPage.registrationModeLabel, selection: $registrationModeDraft) {
            ForEach(registrationModeOptions(), id: \.value) { option in
                Text(renderLocalizedText(option.label)).tag(option.value)
            }
        }
        .pickerStyle(.menu)
        .accessibilityIdentifier(Ids.adminUsersRegistrationModeSelect)
        // `value`/`set` speak the WIRE value (the cross-app `select(id, value)`
        // contract — `admin.py::registration_mode` reads a wire string, never
        // the visible label), matching the tier pickers, not `guardianMenu`.
        .automationSelect(
            Ids.adminUsersRegistrationModeSelect,
            value: { registrationModeDraft },
            options: { registrationModeOptions().map { $0.value } }
        ) { picked in registrationModeDraft = picked }
    }

    /// Save action — resolves the drafted wire value back to the enum (the
    /// picker only ever offers a known option, so this is unreachable in
    /// practice) and hands off to the VM, which parses the ceiling and
    /// dispatches. Re-syncs the drafts from the persisted result afterwards.
    private func confirmSaveRegistration() {
        guard let mode = registrationModeFromWire(mode: registrationModeDraft) else { return }
        Task {
            await vm.saveRegistration(mode: mode, maxFreeUsersInput: maxFreeUsersInput,
                                      ageVerification: ageVerificationDraft)
            syncRegistrationDrafts()
        }
    }

    /// Re-seed the registration drafts from the persisted posture —
    /// unconditional, unlike the tier drafts: another admin (or this
    /// session's own prior save) may have changed the posture underneath a
    /// stale local draft, so this always tracks nest state rather than only
    /// filling a blank (mirrors tui's `UsersState::reseed`).
    private func syncRegistrationDrafts() {
        if let mode = vm.registrationMode, registrationModeFromWire(mode: mode) != nil {
            registrationModeDraft = mode
        } else {
            registrationModeDraft = ""
        }
        maxFreeUsersInput = vm.maxFreeUsers.map { String($0) } ?? ""
        ageVerificationDraft = vm.ageVerificationRequired
    }

    // MARK: - Section 3: Admit

    /// Direct admission — the third account-creation path
    /// (`admin-users-admit-*`; public-mode.md § Registration & Identity;
    /// user-approved 2026-08-15). The admin types a known actor id, names the
    /// handle the actor is admitted under (blank ⇒ the deliberate
    /// handle-less state — public-mode.md § A handle-less account), and
    /// picks a tier ("admission is always choosing a tier"). The form is
    /// never cleared on success or failure — the new row in the
    /// Users-section refetch is the feedback. Mirrors tui's `admit_section`.
    private var admitSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            automationText(Ids.adminUsersAdmitSection, L.admin.usersPage.sectionAdmit)
                .font(.headline)

            HStack(spacing: 12) {
                TextField(L.admin.usersPage.admitActorLabel, text: $admitActorInput)
                    .textFieldStyle(.roundedBorder)
                    .font(.caption.monospaced())
                    .accessibilityIdentifier(Ids.adminUsersAdmitActorInput)
                    .automationField(Ids.adminUsersAdmitActorInput, text: $admitActorInput)

                TextField(L.admin.usersPage.admitHandleLabel, text: $admitHandleInput)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.adminUsersAdmitHandleInput)
                    .automationField(Ids.adminUsersAdmitHandleInput, text: $admitHandleInput)

                tierMenu(
                    id: "admin-users-admit-tier-select",
                    current: { admitTierDraft }
                ) { admitTierDraft = $0 }

                Button(L.admin.usersPage.admitButton) {
                    admitUser()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminUsersAdmitButton)
                .automationActivate(Ids.adminUsersAdmitButton) {
                    admitUser()
                }
                .faunaGate("fauna.admin.users.create")
            }
        }
    }

    /// Admit action — shared by the `Button` and `automationActivate`. Local
    /// hex validation happens in `AdminVM.admitUser` (mirrors
    /// `approveRequest`/`denyRequest`'s thin pass-through of raw drafts).
    private func admitUser() {
        let tier = admitTierDraft.isEmpty ? (vm.tierNames.first ?? defaultInviteTier()) : admitTierDraft
        Task { await vm.admitUser(actorHex: admitActorInput, handle: admitHandleInput, tier: tier) }
    }

    // MARK: - Section 4: Invite

    private var inviteSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            automationText(Ids.adminUsersInviteSection, L.admin.usersPage.sectionInvite)
                .font(.headline)

            // Minted-token copy affordance (mint-on-empty: the nest returns the code).
            if let code = vm.mintedCode {
                HStack(spacing: 8) {
                    Text(L.admin.usersPage.mintedCode(code: code))
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                    CopyButton(Ids.adminUsersInviteCodeCopyBtn, text: code)
                }
            }

            HStack(spacing: 12) {
                Button(L.admin.settingsPage.createCode) {
                    openCreateInvite()
                }
                .accessibilityIdentifier(Ids.createInviteCodeBtn)
                .automationActivate(Ids.createInviteCodeBtn) {
                    openCreateInvite()
                }

                if vm.showCreateInvite {
                    inviteCreateForm
                }
            }

            // Existing codes.
            if vm.inviteCodes.isEmpty {
                Text(L.admin.settingsPage.noCodes)
                    .foregroundStyle(.secondary)
                    .font(.caption)
            } else {
                ForEach(vm.inviteCodes, id: \.code) { code in
                    HStack {
                        automationText(Ids.inviteCodeValue, code.code)
                            .font(.caption.monospaced())
                            .textSelection(.enabled)
                        Spacer()
                        Text(L.admin.settingsPage.usesLeftN(count: String(code.usesLeft)))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                        // The minted band echoes on the same row (no new id).
                        if let band = inviteCodeBandText(code) {
                            Text(band)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        Button(role: .destructive) {
                            Task { await vm.deleteInviteCode(code.code) }
                        } label: {
                            Image(systemName: "trash").font(.caption)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier(Ids.adminSettingsInviteDeleteButton)
                        .automationActivate(Ids.adminSettingsInviteDeleteButton) {
                            Task { await vm.deleteInviteCode(code.code) }
                        }
                        .faunaGate("fauna.admin.invite_codes.delete")
                    }
                    .padding(.vertical, 2)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.inviteCodeItem)
                    // Per-row read anchor (indexed): tui's row text — tier · uses
                    // left, plus the minted band when there is one ("free · 1 ·
                    // Under 13"); the token itself reads off `invite-code-value`.
                    .automationValue(Ids.inviteCodeItem, text: { inviteCodeItemText(code) })
                }
            }
        }
    }

    private var inviteCreateForm: some View {
        // Two stacked rows, NOT one — the same fix `MediaExplorerContent.
        // controlsBar` applies to the same bug class: a single HStack of every
        // control (tier picker + guardian picker, whose menu-style button
        // renders the FULL selected handle as its label + max-uses field +
        // Confirm/Cancel) can overflow the narrower iOS width once the
        // guardian handle is long enough, and SwiftUI resolves that by
        // centering the whole row off-screen — which does not just geo-park
        // the trailing controls at a negative x, it can push them out far
        // enough that they never register at all (`create-invite-confirm-btn`
        // reads `count=0`, not merely `visible=False`, for an 18-char handle
        // ). Pickers on top,
        // the mint action below — each row left-aligns via a trailing
        // `Spacer`.
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                tierMenu(id: "admin-settings-tier-select",
                         current: { mintTier }) { mintTier = $0 }

                // Minting with a guardian selected makes the redeemed account
                // supervised by that actor — the additive `guardian_actor` rides the
                // mint exactly as `tier` does (family-safety.md § Wire & data shape).
                // The onboarding `invite-code-supervised-notice` discloses it to the
                // joiner *before* redemption.
                guardianMenu(id: "admin-users-invite-guardian-select",
                             current: { mintGuardian }) { picked in
                    // A band presupposes a guardian: clearing one clears the
                    // other, so a stale band never rides an unsupervised mint.
                    if picked.isEmpty { mintAgeBand = ageBandNotSetValue() }
                    mintGuardian = picked
                }

                Spacer(minLength: 0)
            }
            HStack(spacing: 8) {
                ageBandMenu(id: Ids.adminUsersInviteAgeBandSelect,
                            current: { mintAgeBand },
                            enabled: { !mintGuardian.isEmpty }) { mintAgeBand = $0 }

                Spacer(minLength: 0)
            }
            HStack(spacing: 8) {
                TextField(L.admin.settingsPage.maxUses, text: $maxUses)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 70)
                    .accessibilityIdentifier(Ids.adminSettingsMaxUsesInput)
                    .automationField(Ids.adminSettingsMaxUsesInput, text: $maxUses)

                Button(L.common.confirm) {
                    confirmCreateInvite()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.createInviteConfirmBtn)
                .automationActivate(Ids.createInviteConfirmBtn) {
                    confirmCreateInvite()
                }
                // Revealing the form is local (`create-invite-code-btn` stays live);
                // minting the code is the nest-arbitrated call.
                .faunaGate("fauna.admin.invite_codes.create")

                Button(L.common.cancel) {
                    vm.showCreateInvite = false
                }
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminSettingsInviteCancelButton)
                .automationActivate(Ids.adminSettingsInviteCancelButton) {
                    vm.showCreateInvite = false
                }

                Spacer(minLength: 0)
            }
        }
        // `.contain` so the cycle-tier button / max-uses field / confirm button
        // stay individually addressable to XCUITest — a bare
        // `.accessibilityIdentifier` on the container VStack collapses it into one
        // element and hides the children (XCUITest then 404s the tier-select),
        // matching how `user-row` / `invite-code-item` expose their children.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminSettingsInviteCreateForm)
        // Group anchor read so the driver can locate the form (exposes the
        // currently-selected mint tier).
        .automationValue(Ids.adminSettingsInviteCreateForm, text: { mintTier })
    }

    /// Open-create-invite action — shared by the `Button` and `automationActivate`.
    private func openCreateInvite() {
        if mintTier.isEmpty { mintTier = vm.tierNames.first ?? defaultInviteTier() }
        vm.showCreateInvite = true
    }

    /// Confirm-mint action — shared by the `Button` and `automationActivate`.
    private func confirmCreateInvite() {
        // Clamp to >= 1 like the other five apps: the nest stores `uses`
        // unvalidated and redemption checks `uses_left > 0`, so a 0/negative
        // mint is a born-dead code the admin hands out believing it works.
        let uses = max(Int(maxUses) ?? 1, 1)
        let guardian = vm.guardianActorId(forLabel: mintGuardian)
        let band = mintAgeBand
        Task { await vm.mintInviteCode(tier: mintTier, uses: uses, guardian: guardian, ageBand: band) }
    }

    // MARK: - Section 5: Users

    private var usersSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                automationText(Ids.adminUsersListSection, L.admin.usersPage.sectionUsers)
                    .font(.headline)
                automationText(Ids.userCountText, String(vm.totalUsers))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
            }

            if vm.users.isEmpty {
                Text(L.admin.dashboard.noUsers)
                    .foregroundStyle(.secondary)
                    .font(.caption)
            } else {
                ForEach(Array(vm.users.enumerated()), id: \.element.actorId) { offset, user in
                    userRow(user)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.userRow)
                        // Per-row read anchor (indexed) exposing the actor id.
                        .automationValue(Ids.userRow, text: { shortHex(user.actorId) })
                        // Scoped container (goal-doc rule 5): suspend/evict/restore
                        // are conditional singletons on one row — the flat
                        // occurrence-index heuristic can't resolve scope index N
                        // once N picks a row whose control differs from row 0's.
                        .automationScope(Ids.userRow, index: offset)
                }
            }

            paginationControls
        }
    }

    @ViewBuilder
    private func userRow(_ user: FfiAdminUser) -> some View {
        // Which of the three lifecycle controls this row offers is the shared
        // decision (`admin.md` § 2 Users → *Cutting a user off*): the eviction
        // state crossed with the admin-role guard. Do NOT re-derive it from
        // `eviction`/`isAdmin` here — that dropped the `!is_admin` guard (Evict
        // rendered on admin rows the nest always refuses with `fauna.admin.conflict`)
        // and never offered Suspend at all (priority #2/#4).
        let controls = adminUserRowControls(user: user)
        HStack(spacing: 12) {
            automationText(Ids.userActorId, shortHex(user.actorId))
                .font(.caption.monospaced())

            tierMenu(
                id: "admin-users-tier-select",
                // Read the tier LIVE from the vm list by the row's stable actorId,
                // so the registry read reflects the post-update refetch (the row
                // view persists, so a captured `user.tier` would stay stale).
                current: { vm.users.first { $0.actorId == user.actorId }?.tier ?? user.tier }
            ) { newTier in
                Task { await vm.setUserTier(user, tier: newTier) }
            }
            // This picker COMMITS on pick (the tier is the quota, so the write is
            // `users.update`) — unlike the mint form's `admin-settings-tier-select`,
            // which only edits a local buffer and therefore stays live offline.
            .faunaGate("fauna.admin.users.update")

            automationText(Ids.adminUsersMailServingStatus,
                           // Read the serving flag LIVE from the vm list by stable
                           // actorId (post-refetch), then resolve the enabled→label
                           // choice through shared Rust (`mail_serving_status_label`)
                           // so no client hard-codes the serving_here/disabled keys
                           // (value-formatting.md; priority #2 — the deviceStatusLabel
                           // pattern).
                           renderLocalizedText(mailServingStatusLabel(
                               enabled: vm.users.first { $0.actorId == user.actorId }?.mailServingEnabled
                                   ?? user.mailServingEnabled)))
                .font(.caption)
                .foregroundStyle(.secondary)

            Spacer()

            // Suspend + Evict are independent (a non-admin Active row offers both;
            // a `warning` row offers Suspend + Restore), so these are three separate
            // `if`s off the shared booleans, not a mutually-exclusive branch.
            if controls.suspend {
                Button(L.admin.usersPage.suspend, role: .destructive) {
                    Task { await vm.suspendUser(user) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminUsersSuspendButton)
                .automationActivate(Ids.adminUsersSuspendButton) {
                    Task { await vm.suspendUser(user) }
                }
                .faunaGate("fauna.admin.users.suspend")
            }
            if controls.evict {
                Button(L.admin.usersPage.evict, role: .destructive) {
                    Task { await vm.evictUser(user) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminUsersEvictButton)
                .automationActivate(Ids.adminUsersEvictButton) {
                    Task { await vm.evictUser(user) }
                }
                .faunaGate("fauna.admin.users.evict")
            }
            if controls.restore {
                Button(L.admin.usersPage.cancelEviction) {
                    Task { await vm.cancelEviction(user) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminUsersCancelEvictionButton)
                .automationActivate(Ids.adminUsersCancelEvictionButton) {
                    Task { await vm.cancelEviction(user) }
                }
                .faunaGate("fauna.admin.users.cancel_eviction")
            }
            // The roster surface's grant/revoke instrument (admin.md § Admin
            // continuity and succession) — mutually exclusive per row like the
            // cut-off trio: a plain row offers Make Admin, an is_admin row
            // offers Remove Admin.
            if controls.makeAdmin {
                Button(L.admin.usersPage.makeAdmin) {
                    Task { await vm.makeAdmin(user) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminUsersMakeAdminButton)
                .automationActivate(Ids.adminUsersMakeAdminButton) {
                    Task { await vm.makeAdmin(user) }
                }
                .faunaGate("fauna.admin.admins.add")
            }
            if controls.removeAdmin {
                Button(L.admin.usersPage.removeAdmin, role: .destructive) {
                    Task { await vm.removeAdmin(user) }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.adminUsersRemoveAdminButton)
                .automationActivate(Ids.adminUsersRemoveAdminButton) {
                    Task { await vm.removeAdmin(user) }
                }
                .faunaGate("fauna.admin.admins.remove")
            }
        }
        .padding(.vertical, 4)
    }

    private var paginationControls: some View {
        HStack(spacing: 12) {
            Button(L.admin.usersPage.prevPage) {
                Task { await vm.prevPage() }
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.adminUsersPrevPage)
            .automationActivate(Ids.adminUsersPrevPage) {
                Task { await vm.prevPage() }
            }

            Text(L.admin.usersPage.total(count: String(vm.totalUsers)))
                .font(.caption2)
                .foregroundStyle(.secondary)

            Button(L.admin.usersPage.nextPage) {
                Task { await vm.nextPage() }
            }
            .controlSize(.small)
            .accessibilityIdentifier(Ids.adminUsersNextPage)
            .automationActivate(Ids.adminUsersNextPage) {
                Task { await vm.nextPage() }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminUsersPagination)
        .automationValue(Ids.adminUsersPagination, text: { String(vm.totalUsers) })
    }

    // MARK: - Tier picker (cycle button — the XCUITest-findable native control)

    /// The tier picker over `vm.tierNames` — thin wrapper over the shared
    /// `tierCycleButton` (`AdminTierPickers.swift`), which every admin tier-picking
    /// surface uses.
    private func tierMenu(id: String, current: @escaping () -> String, onPick: @escaping (String) -> Void) -> some View {
        tierCycleButton(id: id, names: vm.tierNames, current: current, onPick: onPick)
    }

    // MARK: - Guardian picker (family-safety.md § Wire & data shape)

    /// The guardian picker shared by the mint form
    /// (`admin-users-invite-guardian-select`) and each pending-request row
    /// (`invite-request-row-guardian-select`). Options are the non-suspended users
    /// already loaded in the hub, plus a **"None" sentinel first** — the default,
    /// i.e. an ordinary unsupervised admission.
    ///
    /// `current`/`onPick` carry the option's **display text** — the user's
    /// handle, or the full actor hex for a handle-less account
    /// (`vm.guardianLabel`, `admin.md` § 2: the handle identifies a user in an
    /// admin picker, the editable non-unique `label` never does) — not an
    /// actor id: that is what the menu shows, what the driver's
    /// `select(id, value)` sends, and what `get_text` reads back. The caller
    /// maps the text to an actor id at submit time via
    /// `vm.guardianActorId(forLabel:)`, injective by construction — so a stale
    /// option (a user evicted between selection and approve) resolves to
    /// `nil` (unsupervised) rather than to the wrong actor.
    ///
    /// A real `Picker` (not the `tierMenu` cycle-button): that workaround existed
    /// only because a `Picker` was unfindable to **XCUITest**, which the apple
    /// apps no longer use — the in-process driver reads the registry, and
    /// `AdminDnsView`'s actor pickers are the proven precedent. The e2e drives this
    /// with a bare `driver.select` (no cycle fallback), which `automationSelect`
    /// serves directly.
    private func guardianMenu(id: String, current: @escaping () -> String,
                              onPick: @escaping (String) -> Void) -> some View {
        Picker(L.admin.usersPage.guardianLabel, selection: Binding(
            get: { current() },
            set: { onPick($0) }
        )) {
            Text(L.admin.usersPage.guardianNone).tag("")
            ForEach(vm.guardianOptions, id: \.actorId) { user in
                Text(vm.guardianLabel(user)).tag(vm.guardianLabel(user))
            }
        }
        .pickerStyle(.menu)
        .accessibilityIdentifier(id)
        // `value` re-reads live (the users list refetches after each admission, so
        // a captured string would freeze the read). The "None" sentinel reads back
        // as its localized label, matching every other app's picker.
        .automationSelect(
            id,
            value: { current().isEmpty ? L.admin.usersPage.guardianNone : current() }
        ) { picked in
            onPick(picked == L.admin.usersPage.guardianNone ? "" : picked)
        }
    }

    // MARK: - Age-band picker (family-safety.md § App surface → *Age-band surfaces*)

    /// The age-band picker shared by the mint form
    /// (`admin-users-invite-age-band-select`) and each pending-request row
    /// (`invite-request-row-age-band-select`) — the shared `ageBandOptions()`
    /// catalog by VALUE (*not set* + the four bands in the ratified order), so
    /// no app spells the vocabulary or its order; the driver's `select(id,
    /// value)` speaks the value. **Enabled only while a guardian is selected**:
    /// the nest refuses a band without a guardian designation and the picker
    /// gates the same way (a disabled picker still reads its value).
    private func ageBandMenu(id: String, current: @escaping () -> String,
                             enabled: @escaping () -> Bool,
                             onPick: @escaping (String) -> Void) -> some View {
        Picker(L.family.ageBand.label, selection: Binding(
            get: { current() },
            set: { onPick($0) }
        )) {
            ForEach(ageBandOptions(), id: \.value) { option in
                Text(renderLocalizedText(option.label)).tag(option.value)
            }
        }
        .pickerStyle(.menu)
        .disabled(!enabled())
        .accessibilityIdentifier(id)
        .automationSelect(
            id,
            value: { current() },
            options: { ageBandOptions().map { $0.value } },
            isEnabled: enabled
        ) { picked in onPick(picked) }
    }

    /// The minted band's label for an `invite-code-item` row, `nil` for an
    /// ordinary code or a token this client cannot name.
    private func inviteCodeBandText(_ code: FfiAdminInviteCode) -> String? {
        code.ageBand.flatMap { ageBandLabel(band: $0) }.map(renderLocalizedText)
    }

    /// An `invite-code-item` row's read text — tier · uses left, then the
    /// minted band when there is one (tui's row shape).
    private func inviteCodeItemText(_ code: FfiAdminInviteCode) -> String {
        ([code.tier, String(code.usesLeft)] + [inviteCodeBandText(code)].compactMap { $0 })
            .joined(separator: " · ")
    }

    // MARK: - Helpers

    // Truncated actor-id label; byte→hex via the shared encoder, then the canonical
    // short-id truncation (value-formatting.md § Short id).
    private func shortHex(_ data: Data) -> String {
        shortId(hex: hexFull(bytes: data))
    }
}
