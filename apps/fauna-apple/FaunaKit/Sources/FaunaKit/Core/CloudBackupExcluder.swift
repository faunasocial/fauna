import Foundation

/// iOS's `FfiCloudBackupExcluder` — the shell side of a store's cloud-backup
/// exclusion, for BOTH stores that state one: the client-device custodian store
/// (`docs/goal/behavior/backup-destinations.md` § Third destination kind →
/// *Durability + labeling*; called by `build_custodian_host`) and, since
/// 2026-08-26, the W3 (account-data-plane.md § Workstreams) account store (`FaunaClient.startAccountRuntime()`; the
/// writer key is a `ThisDeviceOnly` keychain row a restore does not carry, so
/// the store dir must be out of the same backup — `common.md` § Credential
/// storage → *The shared Rust credential slots on the phones*). Called with the
/// store root after it is created and before anything is written into it;
/// returning an error aborts the whole build / assembly, because from that
/// point on an unexcluded store looks identical to a correctly excluded one.
///
/// `ExcludedByShell` is the right arm for iOS (unlike macOS, which has no
/// iCloud device backup — its account runtime passes no container at all and
/// shared Rust states the desktop posture; its custodian rides the separate
/// `fauna-sync-agent` daemon) — do not carry macOS's `NotApplicable` answer
/// across to this leg. Only iOS actually constructs and passes this; the type
/// itself is left unguarded by `#if os(iOS)` because
/// `URLResourceValues.isExcludedFromBackup` is plain cross-platform Foundation,
/// which is what lets `swift test` on macOS headlessly verify the real
/// resource-value flip rather than only inspecting the source
/// (`CloudBackupExcluderTests.swift`).
public final class CloudBackupExcluder: FfiCloudBackupExcluder {
    public init() {}

    public func exclude(root: String) throws {
        var url = URL(fileURLWithPath: root, isDirectory: true)
        var resourceValues = URLResourceValues()
        resourceValues.isExcludedFromBackup = true
        do {
            try url.setResourceValues(resourceValues)
        } catch {
            throw FfiError.General(msg: "iCloud backup exclusion failed: \(error)")
        }
    }
}
