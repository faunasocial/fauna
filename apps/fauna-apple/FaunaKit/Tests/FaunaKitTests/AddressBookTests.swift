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
