import Foundation
import Testing
@testable import FaunaKit

// Non-e2e unit tests for `AdminVM`'s guardian-picker option text and
// resolution (family-safety.md § Wire & data shape; admin.md § 2 → *What
// identifies a user in an admin picker*): the handle, falling back to the
// full actor hex for a handle-less account — never the editable, non-unique
// `label`. Mirrors the sibling legs' pin tests (linux
// `guardian_option_label_prefers_the_handle_over_the_label` /
// `guardian_option_label_falls_back_to_full_hex_when_handle_absent`, tui's
// `guardian_label`/`resolve_guardian` tests). The full UI round-trip
// (picker render → mint/approve) is the harness e2e `test_family.py`; this
// pins the pure rule so a reverted-to-label picker fails fast.

private func user(
    actor: UInt8, label: String, handle: String?, suspended: Bool = false
) -> FfiAdminUser {
    FfiAdminUser(
        actorId: Data([actor]),
        tier: "free",
        label: label,
        handle: handle,
        suspended: suspended,
        createdAt: 0,
        inboxBytesUsed: 0,
        storageBytesUsed: 0,
        eviction: nil,
        mailServingEnabled: true,
        isAdmin: false
    )
}

@Test @MainActor func guardianLabelPrefersHandleOverLabel() {
    let vm = AdminVM()
    let u = user(actor: 0x01, label: "Alex's Account", handle: "alex99")
    #expect(vm.guardianLabel(u) == "alex99")
}

@Test @MainActor func guardianLabelFallsBackToFullHexWhenHandleAbsent() {
    let vm = AdminVM()
    let noHandle = user(actor: 0xAB, label: "free", handle: nil)
    let blankHandle = user(actor: 0xAB, label: "free", handle: "")
    #expect(vm.guardianLabel(noHandle) == hexFull(bytes: Data([0xAB])))
    #expect(vm.guardianLabel(blankHandle) == hexFull(bytes: Data([0xAB])))
}

// The defect the ruling exists to close: two accounts sharing an editable
// label must still resolve to their OWN actor id, never the first match.
@Test @MainActor func guardianActorIdResolvesInjectivelyEvenWithSharedLabels() {
    let vm = AdminVM()
    let first = user(actor: 0x11, label: "e2e-test", handle: "alex99")
    let second = user(actor: 0x22, label: "e2e-test", handle: "bao77")
    vm.allUsers = [first, second]

    #expect(vm.guardianActorId(forLabel: "alex99") == Data([0x11]))
    #expect(vm.guardianActorId(forLabel: "bao77") == Data([0x22]))
}

@Test @MainActor func guardianActorIdNoneSentinelAndEmptyResolveToNil() {
    let vm = AdminVM()
    vm.allUsers = [user(actor: 0x33, label: "free", handle: "alex99")]

    #expect(vm.guardianActorId(forLabel: "") == nil)
    #expect(vm.guardianActorId(forLabel: L.admin.usersPage.guardianNone) == nil)
    #expect(vm.guardianActorId(forLabel: "no-such-handle") == nil)
}

@Test @MainActor func guardianOptionsExcludeSuspendedUsers() {
    let vm = AdminVM()
    let active = user(actor: 0x44, label: "free", handle: "alex99")
    let suspended = user(actor: 0x55, label: "free", handle: "bao77", suspended: true)
    vm.allUsers = [active, suspended]

    #expect(vm.guardianOptions.map(\.actorId) == [active.actorId])
    // A suspended user's handle no longer resolves via the option list either.
    #expect(vm.guardianActorId(forLabel: "bao77") == nil)
}
