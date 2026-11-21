import Testing
@testable import FaunaKit

/// Unit coverage for the Address Book segment's client-only render logic
/// (`AddressBookVM.orgLabel`) — the sole non-trivial pure function this slice
/// adds; the FFI reads (`listAddressbooks`/`queryCards`) and the shared
/// `FfiPostalAddress.formatted` one-line join are proven by the crate's own
/// tests (`libs/fauna-client-carddav`), no re-derivation here (priority #2/#4).
/// Mirrors linux's `carddav_backend::vcard_maps_to_row_with_all_fields`
/// org-join assertion.

@Test func orgLabelJoinsNonEmptyComponentsWithMiddleDot() {
    #expect(AddressBookVM.orgLabel(["Analytical Engine", "Research"]) == "Analytical Engine · Research")
}

@Test func orgLabelDropsEmptyComponents() {
    #expect(AddressBookVM.orgLabel(["Analytical Engine", "", "Research"]) == "Analytical Engine · Research")
}

@Test func orgLabelEmptyForNoComponents() {
    #expect(AddressBookVM.orgLabel([]) == "")
    #expect(AddressBookVM.orgLabel(["", ""]) == "")
}

// ── The book read never waits on a credential (`ui/contacts.md` § Errors &
// edge cases; `mail-credentials.md` ruling 7) — tui's `address_book::render`
// and linux's `book_placeholder_text` are the prior art. ──────────────────────

private let readBook = FfiAddressbookRow(id: "ab", name: "Personal", description: "", cardCount: 0)

@MainActor @Test func aFreshAddressBookPaintsLoadingNeverTheEmptyBook() {
    let vm = AddressBookVM()
    #expect(vm.addressbooksUnresolved)
    #expect(vm.bookPickerPlaceholder == L.contacts.addressBook.loading)
}

@MainActor @Test func anUnresolvedReadPaintsLoadingNeverTheEmptyBook() {
    let vm = AddressBookVM()
    #expect(vm.land(.unresolved) == nil)
    #expect(vm.addressbooks.isEmpty)
    #expect(vm.bookPickerPlaceholder == L.contacts.addressBook.loading)
    #expect(vm.bookPickerPlaceholder != L.contacts.addressBook.noAddressbooks)
}

@MainActor @Test func aLoadedReadWithNoBooksPaintsTheEmptyState() {
    let vm = AddressBookVM()
    #expect(vm.land(.loaded(addressbooks: [])) == [])
    #expect(!vm.addressbooksUnresolved)
    #expect(vm.bookPickerPlaceholder == L.contacts.addressBook.noAddressbooks)
}

@MainActor @Test func aLoadedReadListsTheBooksWithNoPlaceholder() {
    let vm = AddressBookVM()
    vm.land(.loaded(addressbooks: [readBook]))
    #expect(vm.addressbooks == [readBook])
    #expect(vm.bookPickerPlaceholder == nil)
}

@MainActor @Test func aReadThatFallsBackToUnresolvedDropsTheBooksAndPaintsLoading() {
    let vm = AddressBookVM()
    vm.land(.loaded(addressbooks: [readBook]))
    vm.land(.unresolved)
    #expect(vm.addressbooks.isEmpty)
    #expect(vm.selectedAddressbookId == nil)
    #expect(vm.bookPickerPlaceholder == L.contacts.addressBook.loading)
}

@MainActor @Test func resetPutsTheReadBackToUnresolved() {
    let vm = AddressBookVM()
    vm.land(.loaded(addressbooks: []))
    vm.reset()
    #expect(vm.addressbooksUnresolved)
}
