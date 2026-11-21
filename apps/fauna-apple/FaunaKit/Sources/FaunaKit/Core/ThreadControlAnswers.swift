import Foundation

/// The possibility answer of every thread and room control gated by the
/// viewer's role or by the rail, for one thread — the apple face of
/// `ThreadCapabilities::possibility` (`fauna_conversations::thread_controls`,
/// read over FFI through `threadControlPossibility`).
///
/// `docs/goal/ui/conversations.md` § Errors & edge cases → *Controls this
/// person can never use* rules the paint: every `false` is ABSENT (a role only
/// the owner or an admin grants, or a rail with no such act) except the
/// owner's walk-out, which is BLOCKED naming the hand-over. The capability
/// booleans stay for the manager's refusals; a view never reads them for these
/// controls, it asks this — and each handler asks it again before it acts
/// (`ui/README.md` § Offer only what is possible, rule 6; tui's
/// `open_thread_control`). Painted through ``SwiftUI/View/faunaPossible(_:)``.
///
/// The three shapes that keep what a member still reads are named here too, so
/// the views and their unit pin read one decision: the member chips stay the
/// inert roster when the remove is absent, the policy reads as plain lines
/// when the editor is absent, and a per-participant list is shown whole (one
/// row per participant) or not at all.
struct ThreadControlAnswers {
    let capabilities: ThreadCapabilities
    /// The viewer's role in the room; `nil` on a policy-less thread.
    let myRole: RoomRole?

    init(capabilities: ThreadCapabilities, myRole: RoomRole?) {
        self.capabilities = capabilities
        self.myRole = myRole
    }

    init(_ detail: ThreadDetail) {
        self.init(capabilities: detail.capabilities, myRole: detail.room?.myRole)
    }

    /// The one shared answer for `control`.
    func callAsFunction(_ control: ThreadControl) -> Possibility {
        threadControlPossibility(capabilities: capabilities, control: control, myRole: myRole)
    }

    /// The policy pickers, the home nest's read and the labeler rows are
    /// controls (else plain lines where they would sit), and Save is painted.
    var policyEditable: Bool { self(.setPolicy).isShown }

    /// The per-participant section (`room-admin-toggle[i]`,
    /// `room-owner-transfer-button[i]`) is painted at all — one row per
    /// participant, so `[i]` stays the chip's index — only for a viewer who
    /// holds the role behind one of its two lists.
    var participantRowsShown: Bool {
        self(.appointAdmins).isShown || self(.transferOwnership).isShown
    }

    /// The chip is the remove control; otherwise the chip is the inert
    /// roster (a mail chip's address reveal, never a dead remove).
    var memberChipRemoves: Bool { self(.removeMember).isShown }
}

extension ThreadDetail {
    /// ``ThreadControlAnswers`` for this thread.
    var controlAnswers: ThreadControlAnswers { ThreadControlAnswers(self) }
}
