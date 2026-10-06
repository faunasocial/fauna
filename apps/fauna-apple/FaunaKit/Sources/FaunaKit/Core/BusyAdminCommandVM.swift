import Foundation

/// Shared busy-lock/error/refetch lifecycle for an admin-page mutation: a
/// conformer's own guard-let on its FFI handle stays per-call-site (the
/// handle's property name and type genuinely differ per page — `admin`,
/// `natMachine`, `api`), but the busy flag, error surfacing, and unconditional
/// non-optimistic refetch afterward were byte-identical across every
/// `AdminNestVM`/`AdminTiersVM` mutation that isn't independently gated
/// (`AdminNestVM.saveNatMode`, which deliberately skips this cycle — see its
/// own doc) or conditionally-refreshing on success only
/// (`AdminCustodyHostingVM.remove`, which does NOT refetch after a failure —
/// a genuine behavior difference, not accidental duplication; left as a
/// non-conformer).
@MainActor
protocol BusyAdminCommandVM: AnyObject {
    var isBusy: Bool { get set }
    var errorMessage: String? { get set }
    func hydrate() async
}

extension BusyAdminCommandVM {
    /// Run an admin mutation: lock `isBusy`, clear/report the thrown error,
    /// then always refetch (non-optimistic — the row re-renders from
    /// persisted state, proving the round-trip) regardless of outcome.
    func runAdminCommand(_ operation: () async throws -> Void) async {
        isBusy = true
        do {
            try await operation()
            errorMessage = nil
        } catch {
            errorMessage = DisplayError.message(error)
        }
        isBusy = false
        await hydrate()
    }
}
