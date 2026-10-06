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

    /// `seed` is `roomSettingsSeed(detail:)`'s answer; the host opens the
    /// editor only when there is one (a policy-less room or a non-room has no
    /// policy to edit — the door is greyed there, so that is the belt).
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
                .accessibilityIdentifier(Ids.roomJoinRuleSelect)
                .automationSelect(
                    Ids.roomJoinRuleSelect,
                    value: { joinRuleToken },
                    options: { joinRuleTokens },
                    set: { setJoinRule($0) })
            }

            LabeledContent(L.conversations.unified.roomHistoryPolicyLabel) {
                Picker(L.conversations.unified.roomHistoryPolicyLabel, selection: historyPolicyBinding) {
                    ForEach(roomHistoryPolicyEditorChoices(), id: \.self) { policy in
                        Text(roomHistoryPolicyLabel(policy: policy)).tag(roomHistoryPolicyToken(policy: policy))
                    }
                }
                .labelsHidden()
                .accessibilityIdentifier(Ids.roomHistoryPolicySelect)
                .automationSelect(
                    Ids.roomHistoryPolicySelect,
                    value: { historyPolicyToken },
                    options: { historyPolicyTokens },
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

            ErrorBanner(message: refusal)

            HStack {
                Spacer()
                Button(L.common.cancel) { onClose() }
                    .keyboardShortcut(.cancelAction)
                Button(L.common.save) { save() }
                    .buttonStyle(.borderedProminent)
                    .disabled(saving)
                    .accessibilityIdentifier(Ids.roomSettingsSaveButton)
                    .automationActivate(Ids.roomSettingsSaveButton, isEnabled: { !saving }) { save() }
            }
        }
        .padding(20)
        .frame(minWidth: 360)
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
                // `checked` (like every other attribute a driver names here)
                // reads the entry's value; the text is the painted label.
                .automationActivate(
                    Ids.roomAdminToggle,
                    isEnabled: { adminLive(i) },
                    text: { adminLabel(i) },
                    value: { stagedAdmin(i) ? "true" : "false" }
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
                    value: { stagedOwner(i) ? "true" : "false" }
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
