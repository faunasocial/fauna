import Foundation
import Testing
@testable import FaunaKit

// The knock-accept composer (account-offline-mutation.md § Implementation
// status today, the knock-accept and abuse-report composers).
//
// With no nest `FfiContactsClient.knocksAcceptOrQueue` answers QUEUED (the
// device outbox holds the accept, scoped to the sender, and drains it when the
// nest is back — the queue-then-send half is pinned in Rust,
// `libs/fauna-ffi/src/contacts_client.rs`'s `offline_composer_tests`). These pin
// the page's half, which this unit target can reach without a socket: a queued
// accept drops the knock from the local list and re-reads nothing; a sent one
// re-reads from the nest. Mirrors android's `ContactsVMKnockAcceptTest`.

private let sender = String(repeating: "ab", count: 32)
private let other = String(repeating: "cd", count: 32)

private func knock(_ id: Int, from peer: String) -> Knock {
    Knock(id: id, sender: peer, senderNode: nil, summary: nil, createdAt: 0)
}

@MainActor @Test func aKnockAcceptWithNoNestIsQueuedAndLeavesTheLocalList() {
    let vm = ContactsVM()
    vm.knocks = [knock(1, from: sender), knock(2, from: other)]

    let reRead = vm.settleKnockAccept(peerId: sender, queued: true)

    #expect(!reRead, "a queued accept has no nest to re-read from")
    #expect(vm.knocks.map(\.sender) == [other], "the accepted knock leaves the list, the rest stay")
    #expect(vm.contacts.isEmpty, "no roster row until the nest's read after the drain")
}

@MainActor @Test func aKnockAcceptWithTheNestConnectedIsSentAndReReads() {
    let vm = ContactsVM()
    vm.knocks = [knock(1, from: sender)]

    let reRead = vm.settleKnockAccept(peerId: sender, queued: false)

    #expect(reRead, "a sent accept re-reads from the nest, the list's one word")
    #expect(vm.knocks.count == 1, "the re-read, not a local fold, settles a sent accept")
}
