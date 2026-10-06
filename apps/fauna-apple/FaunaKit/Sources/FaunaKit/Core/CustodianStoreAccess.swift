import Foundation

/// This device's sealed **custodian store**, reached the way *this* platform
/// hosts it — the seam behind `backup-orphaned-store-row` and the reclaim
/// gesture (`docs/goal/ui/backups.md` § Manage backup destinations → *Reclaim
/// this device's copy*).
///
/// The two apple shells host the store in genuinely different places, and this
/// protocol is where that difference is confined so nothing above it diverges
/// (priority #1): `BackupDestinationsView` and `BackupDestinationsVM` are one
/// file each for macOS + iOS and neither carries an `#if os(...)`. The single
/// implementation is `FaunaClient`, which picks the platform's source once.
///
/// - **iOS** hosts its own replica in the app process (`CustodianBackupEngine`),
///   so the read and the reclaim go straight to this device's disk through the
///   shared free fns — the same pair android's leg uses.
/// - **macOS** delegates hosting to the external `fauna-sync-agent`, so the
///   store's location is the agent's to resolve (`sync-agent.md` § Control plane
///   split) and only the agent can promise a reclaim does not delete bytes out
///   from under its own live pull pass. macOS therefore asks the agent, exactly
///   as linux and windows do.
///
/// `FaunaClient` picks between them on *whether a provisioner exists*, not on
/// `#if os(...)` — see its `custodianStoreFootprint` for why a compile-time
/// branch here would be typechecked by nothing on this box.
///
/// Neither method answers *whether* the store is orphaned. That verdict is the
/// page's, made against the destination rows it already holds through the shared
/// `custodianStoreIsOrphaned` — see `BackupDestinationsVM.refreshOrphanedStore`.
@MainActor
public protocol CustodianStoreAccess: AnyObject {
    /// What this device's sealed store occupies on disk. A store that does not
    /// exist is an **empty** store, not an error.
    func custodianStoreFootprint() async throws -> FfiCustodianStoreInfo

    /// Free this device's whole sealed store. A `stillHosting` outcome means the
    /// hosting side would not stop in time and **nothing was deleted** — a
    /// reported outcome, not an error.
    func reclaimCustodianStore() async throws -> FfiCustodianReclaimOutcome

    /// Restore the signed-in nest from this device's store — the re-seed
    /// ceremony plus its post-ceremony re-enrollment, both in shared Rust
    /// (`docs/goal/ui/backups.md` § Restore after losing the nest). Run by
    /// whichever side hosts the store: the agent on macOS, this process on iOS.
    /// A ceremony that stopped comes back as the result's `stopped` reason, not
    /// as a throw.
    func reseedCustodianStore() async throws -> FfiReseedResult
}
