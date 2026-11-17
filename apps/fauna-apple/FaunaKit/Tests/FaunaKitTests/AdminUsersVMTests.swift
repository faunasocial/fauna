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

// Pending admin actions section (admin.md § Pending admin actions). Mirrors
// android's `AdminUsersVMTest.refreshLoadsPendingAdminActions` /
// `approveAndCancelCallTheKindsThenRelist`; `APIClient` is concrete here, so
// the list narrowing is pinned as a pure rule and the act paths through the
// unconfigured-VM error surface.

private func pendingAction(id: Int64, status: String) -> FfiAdminPendingAction {
    FfiAdminPendingAction(
        id: id,
        actorId: Data([0x01]),
        actorShort: "01",
        actionType: "admin.add",
        target: nil,
        description: "Grant the admin role",
        status: status,
        isPending: status == "pending",
        createdAt: 0,
        executeAfter: 0,
        requiresQuorum: 1,
        approvals: []
    )
}

@Test @MainActor func pendingSectionKeepsOnlyStillPendingActions() {
    let live = pendingAction(id: 3, status: "pending")
    let done = pendingAction(id: 4, status: "executed")
    let called = pendingAction(id: 5, status: "cancelled")

    #expect(AdminVM.stillPending([live, done, called]).map(\.id) == [3])
    #expect(AdminVM.stillPending([]).isEmpty)
}

@Test @MainActor func approveAndCancelSurfaceTheErrorOnTheHubLine() async {
    let vm = AdminVM()
    vm.pendingActions = [pendingAction(id: 3, status: "pending")]

    await vm.approvePendingAction(id: 3)
    #expect(vm.actionError != nil)

    vm.actionError = nil
    await vm.cancelPendingAction(id: 3)
    #expect(vm.actionError != nil)
    // A failed act never invents or drops rows: the list is the last good read.
    #expect(vm.pendingActions.map(\.id) == [3])
}

// MARK: - Admin take-over (family-safety.md § App surface → *Admin take-over*)
//
// The armed confirm's state machine — arm, one row at a time, cancel sends
// nothing, confirm disarms BEFORE it dispatches `fauna.family.transfer
// { ward, self }` — and the viewer reaching the shared offer. Mirrors linux
// `take_over_arms_one_row_at_a_time_and_disarm_returns_the_ward`; the full
// round-trip is `test_family.py::test_family_admin_takes_over_guardianship`.

private func actor32(_ byte: UInt8) -> Data {
    Data(repeating: byte, count: 32)
}

private func ward(actor: UInt8, guardian: UInt8) -> FfiAdminUser {
    FfiAdminUser(
        actorId: actor32(actor),
        tier: "free",
        label: "",
        handle: "ward\(actor)",
        suspended: false,
        createdAt: 0,
        inboxBytesUsed: 0,
        storageBytesUsed: 0,
        eviction: nil,
        mailServingEnabled: true,
        isAdmin: false,
        guardian: FfiAdminUserGuardian(actorId: actor32(guardian), handle: "parent\(guardian)")
    )
}

@Test @MainActor func takeOverArmsOneRowAtATime() {
    let vm = AdminVM()
    let first = ward(actor: 0x01, guardian: 0x09)
    let second = ward(actor: 0x02, guardian: 0x09)

    #expect(!vm.isTakeOverArmed(first))
    vm.armTakeOver(first)
    #expect(vm.isTakeOverArmed(first))
    vm.armTakeOver(second)
    #expect(!vm.isTakeOverArmed(first))
    #expect(vm.isTakeOverArmed(second))
}

@Test @MainActor func takeOverCancelDisarmsAndSendsNothing() async {
    let vm = AdminVM()
    vm.setViewer(actor32(0xAA))
    var sent = 0
    vm.takeOverSender = { _, _ in sent += 1 }

    vm.armTakeOver(ward(actor: 0x01, guardian: 0x09))
    vm.cancelTakeOver()
    #expect(vm.takeOverArmed == nil)
    await vm.confirmTakeOver()
    #expect(sent == 0)
}

@Test @MainActor func takeOverConfirmDisarmsBeforeItDispatchesWardAndSelf() async {
    let vm = AdminVM()
    vm.setViewer(actor32(0xAA))
    var sent: [(ward: Data, guardian: Data, armedAtSend: Data?)] = []
    vm.takeOverSender = { ward, guardian in
        sent.append((ward, guardian, vm.takeOverArmed))
    }

    vm.armTakeOver(ward(actor: 0x01, guardian: 0x09))
    await vm.confirmTakeOver()
    #expect(sent.count == 1)
    #expect(sent.first?.ward == actor32(0x01))
    #expect(sent.first?.guardian == actor32(0xAA))
    // Disarmed when the send ran, so a second confirm can never re-send.
    #expect(sent.first?.armedAtSend == nil)
    await vm.confirmTakeOver()
    #expect(sent.count == 1)
}

@Test @MainActor func takeOverRefusalLandsOnTheHubLine() async {
    struct Refused: Error {}
    let vm = AdminVM()
    vm.setViewer(actor32(0xAA))
    vm.takeOverSender = { _, _ in throw Refused() }

    vm.armTakeOver(ward(actor: 0x01, guardian: 0x09))
    await vm.confirmTakeOver()
    #expect(vm.actionError != nil)
    #expect(vm.takeOverArmed == nil)
}

@Test @MainActor func takeOverOfferNeedsTheViewer() {
    let vm = AdminVM()
    let supervised = ward(actor: 0x01, guardian: 0x09)
    // No viewer: the shared rule offers nothing.
    #expect(!vm.rowControls(supervised).takeOver)
    // A viewer who is not the guardian is offered the take-over.
    vm.setViewer(actor32(0xAA))
    #expect(vm.rowControls(supervised).takeOver)
    // The guardian themself is not.
    vm.setViewer(actor32(0x09))
    #expect(!vm.rowControls(supervised).takeOver)
}
