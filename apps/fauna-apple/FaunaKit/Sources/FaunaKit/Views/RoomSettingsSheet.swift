import SwiftUI

/// The room policy editor — ui.yaml `conversations.sub_pages.room_settings`.
/// Opened from `thread-room-settings-button`; hosted as a `.sheet` by
/// `ThreadDetailView` on both macOS and iOS (the content is identical).
///
/// **Nothing here decides anything.** The staged state, which rows either
/// control may act on, the at-most-one-staged hand-over rule and the diff
/// back to commits are all `RoomSettingsDraft`'s (`fauna_conversations::
/// room_settings`, through its FFI twins), and Save is one
/// `ConversationsManager.applyRoomSettings` call — so this view is a painter,
/// and the seven apps cannot drift on the policy rules (priority #2;
/// `conversation-rooms.md` § Roles and authorization). linux's
/// `room_settings_overlay.rs` is the reference painter.
///
/// **The editor outlives its own Save** — it "closes only when all landed"
/// (`ui/conversations.md` § Element IDs), staying open with the refused value
/// still staged otherwise. So Save never closes it on its own: only an
/// `applyRoomSettings` verdict of `true` does. Esc (the Cancel button's
/// `.cancelAction` shortcut) cancels, like the rename sheet — no cancel id.
///
/// The presentation is the HOST's state: it hands in `onClose` (clearing the
/// item its `.sheet(item:)` is bound to), and this view calls it exactly when
/// the editor is done — an all-landed Save, a Save with nothing staged, or
/// Cancel — so "who closes the editor, and when" reads in one place rather
/// than hiding in an environment dismiss on an asynchronous tail.
public struct RoomSettingsSheet: View {
    public let threadId: ThreadId
    private let onClose: () -> Void

    /// The participant list the painted rows are indexed against — the roster
    /// as it stood when the editor opened. Every read and gesture resolves a
    /// row through the draft's identity column against THIS list, never by
    /// position into the draft's vectors (`RoomSettingsDraft`'s own doc). Save
    /// re-reads the LIVE list instead, so a member who left while the editor
    /// sat open cannot be written into the room's signed policy.
    private let participants: [TypedAddress]
    private let displays: [String]
    private let canAppointAdmins: Bool
    private let canTransferOwnership: Bool

    @Environment(ConversationsVM.self) private var vm
    @State private var draft: RoomSettingsDraft
    /// The refusal the page's own `error-message` shows — set only by a Save
    /// that did not land every commit.
    @State private var refusal: String = ""
    @State private var saving = false
    /// The walk-out's confirm is open (`room-leave-confirm` is painted).
    @State private var confirmingLeave = false

    /// `seed` is `roomSettingsSeed(detail:)`'s answer; the host opens the
    /// editor only when there is one (a policy-less room or a non-room has no
    /// policy to edit, so that is the belt). The door is live for any member;
    /// each policy control greys off `canSetPolicy` inside.
    public init(detail: ThreadDetail, seed: RoomSettingsDraft, onClose: @escaping () -> Void) {
        self.threadId = detail.threadId
        self.onClose = onClose
        self.participants = detail.participants
        self.displays = detail.participants.enumerated().map { i, addr in
            i < detail.participantDisplays.count && !detail.participantDisplays[i].isEmpty
                ? detail.participantDisplays[i]
                : ConversationsUI.display(addr)
        }
        self.canAppointAdmins = detail.capabilities.canAppointAdmins
        self.canTransferOwnership = detail.capabilities.canTransferOwnership
        self._draft = State(initialValue: seed)
    }

    private var joinRuleToken: String { roomJoinRuleToken(rule: draft.joinRule) }
    private var historyPolicyToken: String { roomHistoryPolicyToken(policy: draft.historyPolicy) }
    private var joinRuleTokens: [String] { roomJoinRuleEditorChoices().map { roomJoinRuleToken(rule: $0) } }
    private var historyPolicyTokens: [String] {
        roomHistoryPolicyEditorChoices().map { roomHistoryPolicyToken(policy: $0) }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L.conversations.unified.threadRoomSettings).font(.headline)

            // The staged content scrolls; Save/Cancel stay put below it.
            ScrollView {
                content
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 240, idealHeight: 360, maxHeight: 520)

            ErrorBanner(message: refusal)

            HStack {
                Spacer()
                Button(L.common.cancel) { onClose() }
                    .keyboardShortcut(.cancelAction)
                Button(L.common.save) { save() }
                    .buttonStyle(.borderedProminent)
                    .disabled(saving || !canSetPolicy)
                    .accessibilityIdentifier(Ids.roomSettingsSaveButton)
                    .automationActivate(Ids.roomSettingsSaveButton, isEnabled: { !saving && canSetPolicy }) { save() }
            }

            leaveSection
        }
        .padding(20)
        .frame(minWidth: 360)
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 12) {

            // The two token pickers, the `event-detail-reminder-select` way: the
            // selection is the driver-facing TOKEN (so `select`/`get_text`
            // round-trip it, the cross-app contract) and each row paints the
            // localized label over it.
            LabeledContent(L.conversations.unified.roomJoinRuleLabel) {
                Picker(L.conversations.unified.roomJoinRuleLabel, selection: joinRuleBinding) {
                    ForEach(roomJoinRuleEditorChoices(), id: \.self) { rule in
                        Text(roomJoinRuleLabel(rule: rule)).tag(roomJoinRuleToken(rule: rule))
                    }
                }
                .labelsHidden()
                .disabled(!canSetPolicy)
                .accessibilityIdentifier(Ids.roomJoinRuleSelect)
                .automationSelect(
                    Ids.roomJoinRuleSelect,
                    value: { joinRuleToken },
                    options: { joinRuleTokens },
                    isEnabled: { canSetPolicy },
                    set: { setJoinRule($0) })
            }

            LabeledContent(L.conversations.unified.roomHistoryPolicyLabel) {
                Picker(L.conversations.unified.roomHistoryPolicyLabel, selection: historyPolicyBinding) {
                    ForEach(roomHistoryPolicyEditorChoices(), id: \.self) { policy in
                        Text(roomHistoryPolicyLabel(policy: policy)).tag(roomHistoryPolicyToken(policy: policy))
                    }
                }
                .labelsHidden()
                .disabled(!canSetPolicy)
                .accessibilityIdentifier(Ids.roomHistoryPolicySelect)
                .automationSelect(
                    Ids.roomHistoryPolicySelect,
                    value: { historyPolicyToken },
                    options: { historyPolicyTokens },
                    isEnabled: { canSetPolicy },
                    set: { setHistoryPolicy($0) })
            }

            // One row per member, indexed like the chips: the admin switch and
            // the hand-over control, painted for every member and merely GREYED
            // where the viewer's capability or the row's own eligibility says
            // no — never hidden, and never live on the owner's own row
            // (`ui/conversations.md` § Architectural rules 5).
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(displays.enumerated()), id: \.offset) { i, display in
                    memberRow(index: i, display: display)
                }
            }

            // The home nest's read, only where there is one to stage (a
            // community room whose answer this device has read).
            if draft.nestRead != nil {
                nestReadToggle
            }

            pendingInvitesSection

            #if !FAUNA_KIDS
            // Present only where the draft stages a labeler set; the kids flavor
            // compiles the catalog plane out, so the rows are a declared kids
            // absence (`account-age-band.md`, the kids-app bullet).
            if draft.labelers != nil {
                RoomLabelerRows(draft: $draft, canSetPolicy: canSetPolicy)
            }
            #endif
        }
    }

    private var canSetPolicy: Bool { vm.detail(threadId)?.capabilities.canSetPolicy ?? false }
    /// The live `can_leave_room` (owner ✗) — read again when the confirm opens,
    /// not only at paint, so a role that changed under the open editor is
    /// honoured.
    private var canLeaveRoom: Bool { vm.detail(threadId)?.capabilities.canLeaveRoom ?? false }

    /// `room-leave-button` + `room-leave-confirm` — the walk-out
    /// (`ui/conversations.md` § Element IDs → *The walk-out*). Below Save and
    /// OUTSIDE its staged set: leaving acts at once, it is not a policy edit.
    /// Greyed — never hidden — off `can_leave_room`, so the owner sees it dead
    /// and hands the room over first. The confirm is the `thread-rename-confirm`
    /// shape; the whole control is online-only, so the confirm carries the gate.
    @ViewBuilder
    private var leaveSection: some View {
        VStack(alignment: .trailing, spacing: 8) {
            Button(L.conversations.unified.roomLeave) { openLeaveConfirm() }
                .buttonStyle(.bordered)
                .disabled(!canLeaveRoom)
                .accessibilityIdentifier(Ids.roomLeaveButton)
                .automationActivate(Ids.roomLeaveButton, isEnabled: { canLeaveRoom }) { openLeaveConfirm() }
            if confirmingLeave {
                HStack(spacing: 8) {
                    Button(L.common.cancel) { confirmingLeave = false }
                    Button(L.common.leave) { confirmLeave() }
                        .buttonStyle(.borderedProminent)
                        .accessibilityIdentifier(Ids.roomLeaveConfirm)
                        .automationActivate(Ids.roomLeaveConfirm) { confirmLeave() }
                        .faunaGate("fauna.conversations.room.leave")
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
    }

    private func openLeaveConfirm() {
        guard canLeaveRoom else { return }
        confirmingLeave = true
    }

    /// Re-checks `can_leave_room` (the open is not the commit), closes the
    /// editor, then makes the one manager call; a refusal rides the page's
    /// `error-message`.
    private func confirmLeave() {
        confirmingLeave = false
        guard canLeaveRoom else { return }
        onClose()
        Task { await vm.leaveRoom(threadId) }
    }

    /// `room-nest-read-toggle` — whether the home nest reads the room. Staged
    /// through the draft; greyed unless the viewer may set policy.
    private var nestReadToggle: some View {
        let reads = draft.nestRead ?? false
        return Button(reads ? L.conversations.unified.roomNestReadYes : L.conversations.unified.roomNestReadNo) {
            toggleNestRead()
        }
        .buttonStyle(.bordered)
        .tint(reads ? Color.accentColor : nil)
        .disabled(!canSetPolicy)
        .accessibilityIdentifier(Ids.roomNestReadToggle)
        .automationActivate(
            Ids.roomNestReadToggle,
            isEnabled: { canSetPolicy },
            text: { (draft.nestRead ?? false) ? L.conversations.unified.roomNestReadYes : L.conversations.unified.roomNestReadNo },
            attributes: { ["checked": (draft.nestRead ?? false) ? "true" : "false"] }
        ) { toggleNestRead() }
    }

    /// What stands pending on the room, only once the home nest has served this
    /// viewer a list and only while it is not empty (absent for `nil` and for an
    /// empty answer alike). Every row is one the nest served *because* this viewer
    /// may withdraw it, so the button is live on every row — online-only, so
    /// gated on the revoke kind; it acts at once, with no confirm.
    @ViewBuilder
    private var pendingInvitesSection: some View {
        if let pending = vm.detail(threadId)?.room?.pendingInvites, !pending.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text(L.conversations.unified.roomPendingInvitesLabel).font(.subheadline)
                ForEach(pending, id: \.inviteeActorHex) { invite in
                    let sentence = roomPendingInviteText(invite: invite)
                    let lapsed = invite.lapsed ? "true" : "false"
                    HStack(spacing: 8) {
                        Text(sentence)
                            .font(.callout)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .accessibilityIdentifier(Ids.roomPendingInvite)
                            .automationValue(Ids.roomPendingInvite, text: { sentence }, value: { lapsed })
                        Button(L.conversations.unified.roomPendingInviteWithdraw) {
                            withdraw(invite.inviteeActorHex)
                        }
                        .buttonStyle(.bordered)
                        .accessibilityIdentifier(Ids.roomPendingInviteWithdrawButton)
                        .automationActivate(Ids.roomPendingInviteWithdrawButton) {
                            withdraw(invite.inviteeActorHex)
                        }
                        .faunaGate("fauna.conversations.room.revoke_invite")
                    }
                }
            }
        }
    }

    private func toggleNestRead() {
        draft = roomSettingsToggleNestRead(draft: draft)
    }

    private func withdraw(_ inviteeActorHex: String) {
        Task { await vm.withdrawRoomInvite(threadId, inviteeActorHex: inviteeActorHex) }
    }

    // Each row's reads resolve through the draft's identity column
    // (`roomSettings…At`), never by indexing its vectors — and the registered
    // automation closures call these too, so they read the LIVE `draft` rather
    // than a value captured by the last body pass: a `checked` read straight
    // after a click must not see the frame before it.
    private func eligible(_ i: Int) -> Bool {
        roomSettingsIsEligible(draft: draft, participants: participants, index: UInt32(i))
    }
    private func stagedAdmin(_ i: Int) -> Bool {
        roomSettingsAdminAt(draft: draft, participants: participants, index: UInt32(i))
    }
    private func stagedOwner(_ i: Int) -> Bool {
        roomSettingsTransferStagedAt(draft: draft, participants: participants, index: UInt32(i))
    }
    private func adminLive(_ i: Int) -> Bool { canAppointAdmins && eligible(i) }
    private func transferLive(_ i: Int) -> Bool { canTransferOwnership && eligible(i) }
    private func adminLabel(_ i: Int) -> String {
        stagedAdmin(i) ? L.conversations.unified.roomAdminYes : L.conversations.unified.roomAdminNo
    }
    private func transferLabel(_ i: Int) -> String {
        stagedOwner(i) ? L.conversations.unified.roomTransferStaged : L.conversations.unified.roomTransferMark
    }

    @ViewBuilder
    private func memberRow(index i: Int, display: String) -> some View {
        HStack(spacing: 8) {
            Text(display)
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(adminLabel(i)) { toggleAdmin(i) }
                .buttonStyle(.bordered)
                .tint(stagedAdmin(i) ? Color.accentColor : nil)
                .disabled(!adminLive(i))
                .accessibilityIdentifier(Ids.roomAdminToggle)
                // `checked` is a NAMED attribute (the server maps a bare value
                // only for "on"/"off"); the text is the painted label.
                .automationActivate(
                    Ids.roomAdminToggle,
                    isEnabled: { adminLive(i) },
                    text: { adminLabel(i) },
                    attributes: { ["checked": stagedAdmin(i) ? "true" : "false"] }
                ) { toggleAdmin(i) }
            Button(transferLabel(i)) { toggleTransfer(i) }
                .buttonStyle(.bordered)
                .tint(stagedOwner(i) ? Color.accentColor : nil)
                .disabled(!transferLive(i))
                .accessibilityIdentifier(Ids.roomOwnerTransferButton)
                .automationActivate(
                    Ids.roomOwnerTransferButton,
                    isEnabled: { transferLive(i) },
                    text: { transferLabel(i) },
                    attributes: { ["checked": stagedOwner(i) ? "true" : "false"] }
                ) { toggleTransfer(i) }
        }
    }

    private var joinRuleBinding: Binding<String> {
        Binding(get: { joinRuleToken }, set: { setJoinRule($0) })
    }

    private var historyPolicyBinding: Binding<String> {
        Binding(get: { historyPolicyToken }, set: { setHistoryPolicy($0) })
    }

    private func setJoinRule(_ token: String) {
        draft = roomSettingsSetJoinRule(draft: draft, token: token)
    }

    private func setHistoryPolicy(_ token: String) {
        draft = roomSettingsSetHistoryPolicy(draft: draft, token: token)
    }

    private func toggleAdmin(_ i: Int) {
        draft = roomSettingsToggleAdmin(draft: draft, participants: participants, index: UInt32(i))
    }

    private func toggleTransfer(_ i: Int) {
        draft = roomSettingsToggleTransfer(draft: draft, participants: participants, index: UInt32(i))
    }

    /// Diff the staged draft against the LIVE roster and hand the edits to the
    /// manager in one call. Nothing staged → Save is a close, not a commit.
    private func save() {
        guard !saving else { return }
        let live = vm.detail(threadId)?.participants ?? participants
        let edits = roomSettingsEdits(draft: draft, participants: live)
        if edits.isEmpty {
            onClose()
            return
        }
        saving = true
        refusal = ""
        logMessage(level: .info, target: "fauna.conversations.room_settings",
                   message: "room settings save: \(edits.count) staged edit(s)")
        Task { @MainActor in
            let allLanded = await vm.applyRoomSettings(threadId, edits)
            logMessage(level: .info, target: "fauna.conversations.room_settings",
                       message: "room settings save: all landed = \(allLanded)")
            saving = false
            if allLanded {
                onClose()
            } else {
                // The manager stopped at the first refusal and painted it on the
                // page error; the editor says the same thing on its own
                // `error-message`, with the refused value still staged.
                refusal = vm.pageError
            }
        }
    }
}

#if !FAUNA_KIDS
/// The room's labeler set — `room-labeler-toggle[i]` +
/// `room-labeler-inspect-button[i]` per catalog labeler a room may name
/// (`ui/conversations.md` § Element IDs → *The room's labeler set*; behavior
/// owner `community-rooms.md` § The three classes, purpose 2).
///
/// **Every decision is shared Rust**: which kinds a room may name
/// (`roomMayNameLabelerKind`), the `checked` mark (`roomSettingsLabelerStaged`)
/// and whether a row may act (`roomSettingsLabelerToggleLive`: a full set greys
/// only the rows it does not name). This paints them and adds the one capability
/// term. The catalog is the page's own `LabelerCatalogVM` — the same machine the
/// Community labelers page drives — and its inspect view opens IN PLACE through
/// the shared `LabelerInspectPanel`, so the staged draft survives an inspect.
/// The kids flavor compiles the catalog plane out, so this whole view is a
/// declared kids absence (android's `kids/RoomLabelerRows.kt` twin).
private struct RoomLabelerRows: View {
    @Binding var draft: RoomSettingsDraft
    let canSetPolicy: Bool

    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var catalog = LabelerCatalogVM()

    private var entries: [LabelerCatalogEntry] { catalog.snapshot?.entries ?? [] }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(L.conversations.unified.roomLabelersLabel).font(.subheadline)
            ForEach(Array(entries.enumerated()), id: \.offset) { index, entry in
                if roomMayNameLabelerKind(kind: entry.artifactKind) {
                    row(index: index, entry: entry)
                }
            }
            if let inspecting = catalog.snapshot?.inspecting {
                LabelerInspectPanel(view: inspecting, onClose: { catalog.closeInspect() })
            }
        }
        .task {
            guard let client else { return }
            await catalog.configure(api: client.api)
        }
    }

    // Reads go through the LIVE `draft`, so a `checked` read straight after a
    // click does not see the frame before it.
    private func staged(_ id: String) -> Bool { roomSettingsLabelerStaged(draft: draft, labeler: id) }
    private func live(_ id: String) -> Bool {
        canSetPolicy && roomSettingsLabelerToggleLive(draft: draft, labeler: id)
    }
    private func label(_ entry: LabelerCatalogEntry) -> String {
        // The kind and a prefix of the id: the catalog row carries no name, and
        // the full id sits one gesture away in the inspect view.
        let short = String(entry.labelerId.prefix(16))
        let mark = staged(entry.labelerId) ? L.conversations.unified.roomLabelerOn : L.conversations.unified.roomLabelerOff
        return "\(entry.artifactKind) · \(short)… · \(mark)"
    }

    @ViewBuilder
    private func row(index: Int, entry: LabelerCatalogEntry) -> some View {
        let id = entry.labelerId
        HStack(spacing: 8) {
            Button(label(entry)) { toggle(id) }
                .buttonStyle(.bordered)
                .tint(staged(id) ? Color.accentColor : nil)
                .disabled(!live(id))
                .accessibilityIdentifier(Ids.roomLabelerToggle)
                .automationActivate(
                    Ids.roomLabelerToggle,
                    isEnabled: { live(id) },
                    text: { label(entry) },
                    attributes: { ["checked": staged(id) ? "true" : "false"] }
                ) { toggle(id) }
            // Inspecting is a read any member may make; the index is the catalog
            // SNAPSHOT index the machine's `inspect` is addressed by.
            Button(L.labelerCatalog.inspect) { Task { await catalog.inspect(index: index) } }
                .buttonStyle(.bordered)
                .accessibilityIdentifier(Ids.roomLabelerInspectButton)
                .automationActivate(Ids.roomLabelerInspectButton) {
                    Task { await catalog.inspect(index: index) }
                }
        }
    }

    private func toggle(_ id: String) {
        draft = roomSettingsToggleLabeler(draft: draft, labeler: id)
    }
}
#endif // !FAUNA_KIDS
