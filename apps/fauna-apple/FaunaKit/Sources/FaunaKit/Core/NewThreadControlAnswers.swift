import Foundation

/// The possibility answer of the new-thread composer's gated controls — Send,
/// attach and topic — the apple face of `new_thread_possibility`
/// (`fauna_conversations::new_thread_controls`, read over FFI through
/// `newThreadControlPossibility`). The sibling of ``ThreadControlAnswers``:
/// an existing thread answers from its `ThreadCapabilities`, a thread not yet
/// created from its draft, because the rail is decided by who it goes to.
///
/// `docs/goal/ui/conversations.md` § Errors & edge cases → *The new-thread
/// composer offers only what it can send* rules the answer: Send is blocked
/// with `conversations.compose.needs_recipient` until a committed chip or typed
/// text exists; attach and topic are possible before any recipient decides a
/// rail, then follow that rail (blocked with `no_attachments_here` /
/// `no_topic_here`). Views paint it through ``SwiftUI/View/faunaPossible(_:)``
/// and each handler asks it again before it acts, refusing on `error-message`
/// with ``Possibility/refusal`` (`ui/README.md` § Offer only what is possible,
/// rules 6 and 7; tui's `new_thread_control`).
struct NewThreadControlAnswers {
    /// The open new-thread composer; `nil` (none open) answers absent.
    let compose: ComposeState?

    init(_ compose: ComposeState?) {
        self.compose = compose
    }

    /// The one shared answer for `control`.
    func callAsFunction(_ control: NewThreadControl) -> Possibility {
        newThreadControlPossibility(compose: compose, control: control)
    }
}
