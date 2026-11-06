#if !FAUNA_KIDS
import Testing
import Foundation
@testable import FaunaKit

// Phase two's Contacts-row affordances (`archive-import.md` § Linking and
// merging, ruling 8) — the apple pin of tui's
// `contacts::tests::the_archive_link_prompt_renders_only_on_its_asked_row_and_unlink_only_when_linked`.
// The state is the shared `ArchiveLinkView` and the words the machine crate's
// `text` composers; this pins what FaunaKit adds: which row paints what.

private func hex(_ byte: String) -> String { String(repeating: byte, count: 32) }

private func view(_ contact: String, _ state: ArchiveLinkState, unmatched: UInt64 = 0) -> ArchiveLinkView {
    ArchiveLinkView(contact: contact, state: state, platform: "facebook",
                    archivedName: "Friend Two", claimed: 0, unmatched: unmatched)
}

@Suite struct ArchiveLinkRowPaintTests {
    /// The question and its yes / no only on an `Asked` row, the linked line
    /// and unlink only on a `Linked` one, nothing for an `Offered` row — the
    /// asked contact deliberately second, so a paint keyed on anything but the
    /// row's own peer would name the wrong person.
    @Test func promptPaintsOnlyOnItsAskedRowAndUnlinkOnlyWhenLinked() {
        let (a, b, c) = (hex("aa"), hex("bb"), hex("cc"))
        let views = [view(a, .offered), view(b, .asked), view(c, .linked, unmatched: 2)]

        #expect(archiveLinkRowPaints(views: views, peerId: a, contactName: "ada").isEmpty)
        #expect(archiveLinkRowPaints(views: views, peerId: b, contactName: "bo") == [
            .asked(prompt: "Is bo the “Friend Two” from your Facebook friends?",
                   contact: b, platform: "facebook"),
        ])
        #expect(archiveLinkRowPaints(views: views, peerId: c, contactName: "cy") == [
            .linked(label: "Linked: Facebook · 2 could not be matched to a post",
                    contact: c, platform: "facebook"),
        ])
    }

    /// Declined and unlinked rows ask nothing and offer nothing.
    @Test func declinedAndUnlinkedRowsPaintNothing() {
        let (b, c) = (hex("bb"), hex("cc"))
        let views = [view(b, .declined), view(c, .unlinked)]
        #expect(archiveLinkRowPaints(views: views, peerId: b, contactName: "bo").isEmpty)
        #expect(archiveLinkRowPaints(views: views, peerId: c, contactName: "cy").isEmpty)
    }

    /// The view's contact is hex; the roster's peer id may differ in case.
    @Test func rowMatchIgnoresHexCase() {
        let views = [view(hex("ab"), .asked)]
        #expect(archiveLinkRowPaints(views: views, peerId: hex("AB"), contactName: "bo").count == 1)
    }
}
#endif
