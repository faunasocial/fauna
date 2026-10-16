import Testing
import Foundation
@testable import FaunaKit

// A rejected feed verb's `error-message` copy (`restricted-posts.md` ruling 6): the
// refusal is recognized by the real shared `feedRefusalI18nKey` and painted as
// `feed.reference_restricted`; any other failure keeps its own text. The twin of
// windows' `FeedVerbErrorCopyTests` and tui's
// `a_restricted_reply_refusal_paints_the_shared_string`.
@MainActor
struct FeedVerbErrorCopyTests {
    // `fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED`, spelled out on purpose:
    // if the shared text moves, the shared recognizer moves with it and this says so.
    private let refusalText =
        "this post is restricted to a smaller audience, and a reply or quote with text "
        + "would be public — it was not sent"

    @Test func aRestrictedReplyRefusalPaintsTheSharedString() {
        let shown = FeedVM.verbErrorCopy(FfiError.General(msg: refusalText))
        #expect(shown == L.lookup("feed.reference_restricted"))
        #expect(shown != refusalText)
    }

    @Test func anyOtherFailureKeepsItsOwnText() {
        #expect(FeedVM.verbErrorCopy(FfiError.General(msg: "nest unreachable")) == "nest unreachable")
    }
}
