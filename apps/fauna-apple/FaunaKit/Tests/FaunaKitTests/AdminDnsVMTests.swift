import Foundation
import Testing
@testable import FaunaKit

// Non-e2e unit tests for `AdminDnsVM.actorLabel` — the DNS catch-all +
// role-address pickers' option text (admin.md § 2 → *What identifies a user
// in an admin picker*): the handle, falling back to the full actor hex —
// never the editable, non-unique `label`. Mirrors `AdminUsersVMTests`' pin of
// the same rule for the guardian family; this closes the gap row
// 281 found — the DNS/apex families had no pin at all.

private func user(
    actor: UInt8, label: String, handle: String?
) -> FfiAdminUser {
    FfiAdminUser(
        actorId: Data([actor]),
        tier: "free",
        label: label,
        handle: handle,
        suspended: false,
        createdAt: 0,
        inboxBytesUsed: 0,
        storageBytesUsed: 0,
        eviction: nil,
        mailServingEnabled: true,
        isAdmin: false
    )
}

@Test @MainActor func actorLabelPrefersHandleOverLabel() {
    let vm = AdminDnsVM()
    vm.actors = [user(actor: 0x01, label: "Alex's Account", handle: "alex99")]
    #expect(vm.actorLabel(Data([0x01])) == "alex99")
}

@Test @MainActor func actorLabelFallsBackToFullHexWhenHandleAbsent() {
    let vm = AdminDnsVM()
    vm.actors = [user(actor: 0xAB, label: "free", handle: nil)]
    #expect(vm.actorLabel(Data([0xAB])) == hexFull(bytes: Data([0xAB])))
}

@Test @MainActor func actorLabelFallsBackToFullHexWhenActorNotLoaded() {
    // The actor bound to this domain isn't on the currently-loaded page —
    // the pre-existing "not found" fallback, now full hex, never hexShort.
    let vm = AdminDnsVM()
    vm.actors = []
    let id = Data([0xCD, 0xEF])
    #expect(vm.actorLabel(id) == L.admin.actorIdFallbackLabel(short: hexFull(bytes: id)))
}

// The defect this exists to close: two accounts sharing an editable label
// must still produce two DISTINCT option strings.
@Test @MainActor func actorLabelStaysInjectiveWhenLabelsCollide() {
    let vm = AdminDnsVM()
    let alex = user(actor: 0x11, label: "e2e-test", handle: "alex99")
    let bao = user(actor: 0x22, label: "e2e-test", handle: "bao77")
    vm.actors = [alex, bao]

    let first = vm.actorLabel(Data([0x11]))
    let second = vm.actorLabel(Data([0x22]))
    #expect(first != second)
    #expect(first == "alex99")
    #expect(second == "bao77")
}
