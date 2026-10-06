import Foundation
import Testing
@testable import FaunaKit

// Non-e2e unit tests for `AdminWebVM.apply` — the apex-actor picker's option
// text (admin.md § 2 → *What identifies a user in an admin picker*): the
// handle, falling back to the full actor hex — never the editable,
// non-unique `label`. Mirrors `AdminUsersVMTests`' pin of the same rule for
// the guardian family; closes the gap found for the apex
// family.

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

@Test @MainActor func apexOptionsPreferHandleOverLabel() {
    let vm = AdminWebVM()
    vm.apply(current: nil, users: [user(actor: 0x01, label: "Alex's Account", handle: "alex99")])
    #expect(vm.options.map(\.label) == [L.admin.webPage.apexNone, "alex99"])
}

@Test @MainActor func apexOptionsFallBackToFullHexWhenHandleAbsent() {
    let vm = AdminWebVM()
    vm.apply(current: nil, users: [user(actor: 0xAB, label: "free", handle: nil)])
    #expect(vm.options.map(\.label) == [L.admin.webPage.apexNone, hexFull(bytes: Data([0xAB]))])
}

// The defect this exists to close: two accounts sharing an editable label
// must still produce two DISTINCT option strings.
@Test @MainActor func apexOptionsStayInjectiveWhenLabelsCollide() {
    let vm = AdminWebVM()
    let alex = user(actor: 0x11, label: "e2e-test", handle: "alex99")
    let bao = user(actor: 0x22, label: "e2e-test", handle: "bao77")
    vm.apply(current: nil, users: [alex, bao])

    let labels = vm.options.map(\.label)
    #expect(labels == [L.admin.webPage.apexNone, "alex99", "bao77"])
    #expect(Set(labels).count == labels.count)
}

@Test @MainActor func apexOptionsFallBackToFullHexWhenCurrentDesignationIsPaginatedOut() {
    // The designated apex actor isn't on the currently-loaded page — the
    // pre-existing "paginated out" fallback, now full hex, never hexShort.
    let vm = AdminWebVM()
    let current = Data([0xCD, 0xEF])
    vm.apply(current: current, users: [])
    #expect(vm.options.last?.label == L.admin.actorIdFallbackLabel(short: hexFull(bytes: current)))
    #expect(vm.selectedIndex == vm.options.count - 1)
}
