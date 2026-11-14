import Foundation
import Testing
@testable import FaunaKit

// The email filter-create composer (account-offline-mutation.md
// § Implementation status today, the filter-create composer).
//
// With no nest `FfiEmailClient.filtersCreateOrQueue` answers QUEUED (the
// device outbox holds the create, scoped to the rule's name, and drains it
// when the nest is back — the queue-then-send half is pinned in Rust,
// `libs/fauna-ffi/src/email_client.rs`'s `offline_composer_tests`). These pin
// the page's half, which this unit target can reach without a socket: a
// queued create closes the form and lists nothing; a sent one closes the form
// and re-lists from the nest. Mirrors android's
// `PrivacySettingsVMFilterCreateTest` and FamilyVM's `settleDecision`.

@MainActor @Test func aFilterCreateWithNoNestIsQueuedAndListsNothing() {
    let vm = PrivacySettingsVM()
    vm.showFilterForm = true

    let reList = vm.settleFilterCreate(queued: true)

    #expect(!reList, "a queued create has no server id and no nest to list from")
    #expect(!vm.showFilterForm, "a queued create resets the add form")
    #expect(vm.emailFilters.isEmpty, "nothing is listed until the drain")
}

@MainActor @Test func aFilterCreateWithTheNestConnectedIsSentAndReLists() {
    let vm = PrivacySettingsVM()
    vm.showFilterForm = true

    let reList = vm.settleFilterCreate(queued: false)

    #expect(reList, "a sent create re-lists from the nest, the list's one word")
    #expect(!vm.showFilterForm, "a sent create closes the add form")
}
