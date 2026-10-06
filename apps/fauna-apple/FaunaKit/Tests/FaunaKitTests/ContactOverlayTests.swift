import Testing
import Foundation
@testable import FaunaKit

// The private contact overlay's apple glue (`contacts.md` § The private
// overlay; `profile.md` § The private section). The names, the roster filter,
// the staging and every refusal are shared Rust behind `FfiContactOverlays` /
// `FfiOverlayEditor` and are pinned there; these pin what FaunaKit adds: the
// roster's flat row index, and the view model's reading of the editor.

private func hex(_ byte: String) -> String { String(repeating: byte, count: 32) }

private func contact(_ byte: String, _ status: String, handle: String? = nil) -> Contact {
    Contact(peerId: hex(byte), status: status, updatedAt: nil, handle: handle,
            domain: handle == nil ? nil : "example.test")
}

@Suite @MainActor struct ContactOverlayTests {
    /// The projection of a manager nothing was saved on: every name is the
    /// public one.
    private func overlays() -> FfiContactOverlays {
        FfiContactOverlays(manager: ConversationsManager())
    }

    // MARK: - The roster door

    /// The row index is flat across the status groups, in display order —
    /// `contact-row[i]` names one row however the rows are grouped. A per-group
    /// offset would give two rows index 0.
    @Test func rosterRowIndexIsFlatAcrossStatusGroups() {
        let vm = ContactsVM()
        // Loaded out of display order on purpose.
        vm.contacts = [
            contact("cc", "pending"),
            contact("aa", "confirmed", handle: "alice"),
            contact("bb", "accepted"),
            contact("dd", "confirmed", handle: "dora"),
        ]
        let groups = vm.rosterGroups(searchFilter: "", overlays: overlays())

        #expect(groups.map(\.status) == ["confirmed", "accepted", "pending"])
        #expect(groups.flatMap(\.rows).map(\.index) == [0, 1, 2, 3])
        #expect(groups.flatMap(\.rows).map(\.contact.peerId)
                == [hex("aa"), hex("dd"), hex("bb"), hex("cc")])
    }

    /// With no overlay a row shows its public name only: the handle where the
    /// nest resolved one, else the canonical short id — and neither secondary
    /// line.
    @Test func rosterRowWithoutAnOverlayShowsThePublicNameOnly() {
        let vm = ContactsVM()
        vm.contacts = [contact("aa", "confirmed", handle: "alice"), contact("bb", "confirmed")]
        let rows = vm.rosterGroups(searchFilter: "", overlays: overlays()).flatMap(\.rows)

        #expect(rows.map(\.name) == ["alice", shortId(hex: hex("bb"))])
        #expect(rows.allSatisfy { $0.publicName == nil && $0.labelsLine == nil })
    }

    /// The filter narrows the groups and the flat index counts only the rows
    /// that remain — the index the driver's `contact-row[0]` reads after
    /// narrowing to one person.
    @Test func rosterFilterNarrowsAndReindexes() {
        let vm = ContactsVM()
        vm.contacts = [
            contact("aa", "confirmed", handle: "alice"),
            contact("bb", "accepted", handle: "bob"),
        ]
        let groups = vm.rosterGroups(searchFilter: hex("bb"), overlays: overlays())

        #expect(groups.map(\.status) == ["accepted"])
        #expect(groups.flatMap(\.rows).map(\.index) == [0])
        #expect(vm.rosterGroups(searchFilter: "no such person", overlays: overlays()).isEmpty)
    }

    // MARK: - The private section's view model

    /// Every gesture only stages: the form follows the editor, a refused label
    /// stages nothing and shows its refusal, and a removal drops the chip.
    @Test func privateSectionStagesThroughTheSharedEditor() {
        let vm = ProfilePrivateVM()
        vm.open(editor: overlays().editor(actorId: hex("aa")))
        #expect(vm.form.nickname == "" && vm.form.notes == "" && vm.form.labels.isEmpty)

        vm.setNickname("Mum")
        vm.setNotes("likes tea")
        #expect(vm.addLabel("Book club"))
        #expect(vm.form.nickname == "Mum")
        #expect(vm.form.notes == "likes tea")
        #expect(vm.form.labels == ["Book club"])
        #expect(vm.errorMessage == nil)

        #expect(!vm.addLabel("   "))
        #expect(vm.errorMessage == L.profile.privateLabelEmpty)
        #expect(vm.form.labels == ["Book club"])

        vm.removeLabel(at: 0)
        #expect(vm.form.labels.isEmpty)
    }

    /// A Save that cannot be written keeps the staged edits on screen and puts
    /// the shared text on the page's error — here the account store has not
    /// assembled (no login in this process).
    @Test func aSaveThatCannotBeWrittenKeepsTheStagedEdits() async {
        let vm = ProfilePrivateVM()
        vm.open(editor: overlays().editor(actorId: hex("aa")))
        vm.setNickname("Mum")

        await vm.save()

        #expect(vm.errorMessage != nil)
        #expect(vm.form.nickname == "Mum")
        #expect(!vm.isSaving)
    }

    /// A Save with nothing staged writes nothing and is not an error.
    @Test func aSaveWithNothingStagedIsNotAnError() async {
        let vm = ProfilePrivateVM()
        vm.open(editor: overlays().editor(actorId: hex("aa")))

        await vm.save()

        #expect(vm.errorMessage == nil)
        #expect(!vm.isSaving)
    }

    /// Opening another person's profile starts clean: nothing staged for the
    /// last person carries over, and neither does their refusal.
    @Test func openingAnotherPersonDropsTheLastOnesStaging() {
        let vm = ProfilePrivateVM()
        vm.open(editor: overlays().editor(actorId: hex("aa")))
        vm.setNickname("Mum")
        _ = vm.addLabel("   ")
        #expect(vm.errorMessage != nil)

        vm.open(editor: overlays().editor(actorId: hex("bb")))
        #expect(vm.form.nickname == "")
        #expect(vm.errorMessage == nil)

        vm.reset()
        vm.setNickname("ignored")
        #expect(vm.form.nickname == "")
    }

    // MARK: - The observer's revision

    /// Reading names never moves the revision: the face is built afresh on
    /// every read, and a read that re-rendered its own reader would loop.
    @Test func readingTheFaceDoesNotMoveTheRevision() {
        let vm = ConversationsVM()
        let observed = vm.overlayRevision
        let projection = vm.contactOverlays.revision()
        _ = vm.contactOverlays.peerLabel(displayName: nil, handle: "alice", actorId: hex("aa"))
        #expect(vm.contactOverlays.revision() == projection)
        #expect(vm.overlayRevision == observed)
    }
}
