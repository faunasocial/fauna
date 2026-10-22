import SwiftUI

/// The community-room invitations standing for this account, atop the
/// conversation list — `room-invitation[i]` with its
/// `room-invitation-accept-button[i]` / `room-invitation-decline-button[i]`
/// pair, flat and indexed alike (the `room-admin-toggle` convention).
/// Shared by the macOS list pane and the iOS list screen (`ui/conversations.md`
/// § Element IDs → *Pending invitations*).
///
/// **Nothing here decides anything.** The rows are already verified against
/// their signers in shared Rust, the sentence is `roomInvitationText`, the
/// `role` attribute is `roomRoleAttrToken`, and accept/decline are one manager
/// call each (accept seats the account and selects the room's thread itself).
/// Accept is an online-only act, so it carries the offline gate
/// (`fauna.conversations.room.accept_invite`); decline is `fauna.inbox.ack`,
/// offline-safe and deliberately UNGATED. tui's `room_invitation_elements` is
/// the contract. Absent while none stands.
public struct RoomInvitationsSection: View {
    @Environment(ConversationsVM.self) private var vm

    public init() {}

    public var body: some View {
        let invitations = vm.snapshot.roomInvitations
        if !invitations.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(invitations, id: \.id) { invitation in
                    row(invitation)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
        }
    }

    @ViewBuilder
    private func row(_ invitation: RoomInvitationSnapshot) -> some View {
        let sentence = roomInvitationText(invitation: invitation)
        let role = roomRoleAttrToken(role: invitation.role)
        let id = invitation.id
        HStack(spacing: 8) {
            Text(sentence)
                .font(.callout)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier(Ids.roomInvitation)
                // `text` = the stated sentence; the `role` attribute (like every
                // attribute a driver names) reads `value`.
                .automationValue(Ids.roomInvitation, text: { sentence }, value: { role })
            Button(L.common.accept) { accept(id) }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier(Ids.roomInvitationAcceptButton)
                .automationActivate(Ids.roomInvitationAcceptButton) { accept(id) }
                .faunaGate("fauna.conversations.room.accept_invite")
            Button(L.common.decline) { decline(id) }
                .buttonStyle(.bordered)
                .accessibilityIdentifier(Ids.roomInvitationDeclineButton)
                .automationActivate(Ids.roomInvitationDeclineButton) { decline(id) }
        }
    }

    private func accept(_ id: Int64) {
        Task { _ = await vm.acceptRoomInvitation(id) }
    }

    private func decline(_ id: Int64) {
        Task { await vm.declineRoomInvitation(id) }
    }
}
