import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for `ThreadControlAnswers` — the one decision the shared
// FaunaKit room editor (`RoomSettingsSheet`), thread header (`ThreadHeader`)
// and reply bar (`DmComposeBar`) paint every role- or rail-gated control from
// (`ui/conversations.md` § Errors & edge cases → *Controls this person can
// never use*; `ui/README.md` § Offer only what is possible, rules 2 and 6).
// The answers come from the shared predicate over FFI
// (`threadControlPossibility`), so this also pins that the binding reaches the
// apple apps; how a painted control LOOKS is the last inch.
@Suite("Thread controls answer in Possibility (role and rail)")
struct ThreadControlAnswersTests {
    /// A capability vector as `RoomSnapshot::gate` writes it; the defaults are
    /// a governed MLS group seen by its owner.
    private static func caps(
        membership: Bool = true,
        invite: Bool = true,
        remove: Bool = true,
        setPolicy: Bool = true,
        appoint: Bool = true,
        transfer: Bool = true,
        leave: Bool = false,
        subject: Bool = true,
        localRemove: Bool = false
    ) -> ThreadCapabilities {
        ThreadCapabilities(
            supportsAttachments: true,
            supportsMarkdown: true,
            supportsReactions: true,
            supportsMessageDelete: true,
            supportsPerMessageReply: true,
            supportsMembershipChange: membership,
            supportsRecipientSelection: false,
            supportsRename: true,
            supportsSubject: subject,
            deliveryMode: .realtime,
            encryption: .e2e,
            canInvite: invite,
            canRemoveMembers: remove,
            canSetPolicy: setPolicy,
            canAppointAdmins: appoint,
            canTransferOwnership: transfer,
            canLeaveRoom: leave,
            canSearchRoom: false,
            canSend: true,
            supportsLocalRemove: localRemove
        )
    }

    private static let owner = ThreadControlAnswers(capabilities: caps(), myRole: .owner)
    private static let member = ThreadControlAnswers(
        capabilities: caps(invite: false, remove: false, setPolicy: false,
                           appoint: false, transfer: false, leave: true),
        myRole: .member)
    /// A bridged room: membership is not ours to change, no subject, no
    /// policy — and the local remove is the rail's.
    private static let bridged = ThreadControlAnswers(
        capabilities: caps(membership: false, invite: false, remove: false, setPolicy: false,
                           appoint: false, transfer: false, leave: false, subject: false,
                           localRemove: true),
        myRole: nil)

    @Test("a plain member's editor paints no policy control, no per-participant control and no Save")
    func memberReadsTheEditor() {
        #expect(!Self.member(.setPolicy).isShown, "pickers, nest read, labeler toggles and Save absent")
        #expect(!Self.member.policyEditable, "the policy reads as plain lines")
        #expect(!Self.member(.appointAdmins).isShown)
        #expect(!Self.member(.transferOwnership).isShown)
        #expect(!Self.member.participantRowsShown, "no per-participant section at all")
        #expect(Self.member(.leaveRoom).isPossible, "the walk-out is a member's verb")
    }

    @Test("a plain member's chips carry no remove and the add-participant button is absent")
    func memberChipsAreTheRoster() {
        #expect(!Self.member.memberChipRemoves)
        #expect(!Self.member(.removeMember).isShown)
        #expect(!Self.member(.invite).isShown, "thread-add-participant-button absent")
    }

    @Test("the owner's walk-out is blocked with the hand-over sentence; everything else is live")
    func ownerLeaveBlocked() {
        let leave = Self.owner(.leaveRoom)
        #expect(leave.isShown && !leave.isPossible)
        let reason = leave.reasonText
        #expect(reason?.isEmpty == false)
        #expect(reason != "error.send.room_owner_cannot_leave", "the reason renders, not echoes its key")
        for control in [ThreadControl.invite, .removeMember, .setPolicy, .appointAdmins,
                        .transferOwnership, .subject] {
            #expect(Self.owner(control).isPossible, "\(control)")
        }
        #expect(Self.owner.policyEditable && Self.owner.participantRowsShown && Self.owner.memberChipRemoves)
    }

    @Test("an admin governs the room but sees neither per-participant list's controls live")
    func adminRows() {
        let admin = ThreadControlAnswers(
            capabilities: Self.caps(appoint: false, transfer: false, leave: true), myRole: .admin)
        #expect(admin.policyEditable)
        #expect(!admin(.appointAdmins).isShown && !admin(.transferOwnership).isShown)
        #expect(!admin.participantRowsShown)
        #expect(admin.memberChipRemoves && admin(.invite).isPossible)
    }

    @Test("the local remove and the reply bar's topic toggle are absent off a rail that has them")
    func railGatedControls() {
        #expect(!Self.owner(.localRemove).isShown, "room-remove-button absent on an MLS group")
        #expect(Self.bridged(.localRemove).isPossible, "a bridged room offers it")
        #expect(!Self.bridged(.subject).isShown, "topic-toggle-button absent with no subject")
        #expect(Self.owner(.subject).isPossible)
    }

    @Test("no answer is blocked but the owner's walk-out — every other false is absent")
    func onlyTheOwnerLeaveIsBlocked() {
        let all: [ThreadControl] = [.invite, .removeMember, .setPolicy, .appointAdmins,
                                    .transferOwnership, .leaveRoom, .localRemove, .subject]
        for answers in [Self.member, Self.bridged] {
            for control in all {
                let got = answers(control)
                #expect(got.isPossible || !got.isShown, "\(control): \(got)")
            }
        }
    }
}
