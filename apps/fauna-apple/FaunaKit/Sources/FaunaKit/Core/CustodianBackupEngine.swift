#if os(iOS)
import Foundation

/// iOS's app leg of the **client-device backup custodian**
/// (`docs/goal/behavior/backup-destinations.md` § Third destination kind;
/// `docs/goal/behavior/backup-restore.md` § Background Tasks) — the two
/// drives `build_custodian_host`'s doc comment prescribes for a
/// scheduler-owned platform, mirroring android's `CustodianHostWorker` +
/// `CustodianPushKick` split in one class: `runPass()` for the periodic
/// `BGProcessingTask` wake, `startForegroundPushKick()` /
/// `stopForegroundPushKick()` for the `scenePhase`-gated low-latency wake
/// `FaunaClient.resume()` / `suspend()` drive.
///
/// **Construct-run-drop, never held.** Unlike `syncHost` / `photoBackup`
/// (built once per session), a fresh `FfiCustodianHost` is built for every
/// pass: the registry row this device pulls against can change between
/// passes (a new enrollment, a raised cap), and "not an enrolled custodian"
/// — the ordinary answer on most devices — must re-check every time, not
/// cache a stale no-op forever. No `startForever` is exported by the FFI
/// face at all, so there is nothing here to hold between wakes.
public final class CustodianBackupEngine {
    private var api: APIClient?
    private var deviceId: String?

    /// Held only while the foreground push-debounce loop is running — the
    /// cancel handle, not the host itself. The Rust side clones its own `Arc`
    /// before the loop spawns (`FfiCustodianHost::start_push_debounce`), so
    /// the loop survives independently of whether Swift keeps the host
    /// object alive.
    private var pushHandle: FfiCustodianPushHandle?

    public init() {}

    public func configure(api: APIClient, deviceId: String) {
        self.api = api
        self.deviceId = deviceId
    }

    /// One pull pass — the `BGProcessingTask` entry point. Throws on a build
    /// failure (offline, registry read failed) so the caller's
    /// `task.setTaskCompleted(success:)` reports failure and the OS backs off
    /// and retries sooner, exactly as `BackgroundScheduler`'s upload/pull
    /// handlers already do for their own drivers. A `nil` host — this device
    /// is not an enrolled custodian — is NOT a failure: it returns normally,
    /// the same clean no-op android's worker treats it as.
    public func runPass() async throws {
        guard let api, let deviceId else { return }
        guard
            let host = try await api.custodianHost(
                deviceId: deviceId,
                dataDir: AccountStateDir.base.path,
                excluder: CloudBackupExcluder()
            )
        else { return }
        let summary = await host.runAllKinds()
        // `runAllKinds` never fails as a whole — a per-kind failure is logged
        // Rust-side and the pass continues — so the summary is how this
        // trigger learns what happened. `capReached` is read from the pass's
        // own verdict, never inferred from `held >= cap`: a pass that stops
        // at its cap ends *below* it.
        logMessage(
            level: .debug, target: "fauna.backup",
            message:
                "[custodian] pass: stored=\(summary.storedSegments) "
                + "tombstoned=\(summary.tombstonedSegments) held=\(summary.heldBytes) "
                + "capReached=\(summary.capReached)"
        )
    }

    /// Start the foreground push-debounce loop. Idempotent — a redundant
    /// start while already holding a handle is a no-op, mirroring android's
    /// `CustodianPushKick.onStart` guard.
    public func startForegroundPushKick() async {
        guard pushHandle == nil, let api, let deviceId else { return }
        do {
            guard
                let host = try await api.custodianHost(
                    deviceId: deviceId,
                    dataDir: AccountStateDir.base.path,
                    excluder: CloudBackupExcluder()
                )
            else { return }
            pushHandle = await host.startPushDebounce()
        } catch {
            logMessage(
                level: .warn, target: "fauna.backup",
                message: "[custodian] push-kick build failed: \(error)")
        }
    }

    /// Stop the foreground push-debounce loop. Idempotent and cheap.
    /// Cancelling is **required**, not hygiene — the push-only loop has no
    /// periodic tick, so nothing else would ever wake it to notice the
    /// source went away.
    public func stopForegroundPushKick() {
        pushHandle?.cancel()
        pushHandle = nil
    }
}
#endif
