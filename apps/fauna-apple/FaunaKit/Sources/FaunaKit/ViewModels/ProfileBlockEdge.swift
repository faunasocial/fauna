import SwiftUI

/// The viewed actor's block edge as `ProfileView`'s Block ⇄ Unblock toggle sees
/// it (`profile.md` § User actions → `profile-block-button`). The edge is
/// not-yet-known (`nil`) from open until the open-time contact read lands, so
/// the toggle is held disabled: a press on the not-blocked default would send
/// `knocks_block` again for an actor the viewer already blocks (it succeeds, the
/// label stays "Unblock", nothing errors — the slow-box e2e signature). Mirrors
/// tui `is_blocked: Option<bool>` + `block_user_authored`, windows
/// `ProfileViewModel`, android `applyBlockRead`.
///
/// Extracted from the view because `@State` cannot be pinned by a unit test.
struct ProfileBlockEdge: Equatable {
    private(set) var isBlocked: Bool?
    /// The viewer's own press has set the edge: a read that lands after it was
    /// too early to see the press and must not rewind it.
    private(set) var userAuthored = false
    /// The open-time contact read threw (and no press has set the edge since).
    private(set) var readFailed = false

    /// The toggle is pressable only once the edge is known.
    var isToggleEnabled: Bool { isBlocked != nil }

    /// `profile-block-button`'s answer (`profile.md` § Errors & edge cases,
    /// *Unread state is not possible*; tui's `block_answer`,
    /// `Possibility::from_read`): blocked `common.still_loading` until the
    /// edge is known, blocked `common.load_failed` once its read failed — a
    /// dead toggle that says why, never live on a guessed edge. The press
    /// asks it again.
    var answer: Possibility {
        let read: ReadState = isBlocked != nil ? .resolved : (readFailed ? .failed : .pending)
        return readPossibility(read: read)
    }

    /// What the label paints: "Unblock" only on a known blocked edge.
    var labelBlocked: Bool { isBlocked ?? false }

    /// A fresh open: the previous actor's edge (and any press on it) is gone.
    mutating func reset() {
        isBlocked = nil
        userAuthored = false
        readFailed = false
    }

    /// The open-time contact read landed.
    mutating func applyRead(blocked: Bool) {
        guard !userAuthored else { return }
        isBlocked = blocked
        readFailed = false
    }

    /// The open-time contact read threw: the edge stays unknown, and the
    /// toggle says the read failed rather than guessing not-blocked.
    mutating func applyReadFailed() {
        guard !userAuthored else { return }
        readFailed = true
    }

    /// The viewer's own block/unblock call succeeded.
    mutating func applyPress(blocked: Bool) {
        isBlocked = blocked
        userAuthored = true
        readFailed = false
    }
}
