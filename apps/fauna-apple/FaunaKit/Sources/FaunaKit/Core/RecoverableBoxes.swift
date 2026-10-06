import Foundation
import FaunaFFISwift

/// The custodied-box list a recovery surface renders — `nest_actor_id` (hex) per
/// box — read for the two entries that need it: `nest_recovery`'s own load
/// (`OnboardingVM.loadRecoveryBoxesIfNeeded`) and the launch-retry
/// `launch-recover-button` reveal (`box-recovery.md` § The plane-era recovery
/// floor, (b) The reads: never either-or).
///
/// The shared `deploymentSeeds` getter already joins the nest's answer with the
/// device's own store, but it needs a connected nest. A **dead saved nest is the
/// expected failure mode here**, not an error, so the read falls back to the
/// device-local `deploymentSeedsLocal` whenever the nest does not answer (or
/// answers empty) — a surviving device still offers every box it custodies. Never
/// throws; the twin of windows' `DeploymentSeedCustody.LoadRecoverableBoxesAsync`.
public enum RecoverableBoxes {
    /// `nestUrl` empty ⇒ no nest to ask (the single-last-box total-loss case): the
    /// device-local read alone.
    public static func load(nestUrl: String, ownerSecret: Data) async -> [String] {
        if !nestUrl.isEmpty, let client = try? FfiNestClient(nestUrl: nestUrl, secret: ownerSecret) {
            var boxes: [String] = []
            do {
                try await client.connect()
                boxes = try await FaunaFFISwift.deploymentSeeds(
                    nest: client, ownerSecret: ownerSecret,
                    storeContainerDir: AccountStateDir.storeContainerDir
                ).map(\.nestActorId)
            } catch {
                logMessage(level: .warn, target: "fauna.recovery",
                           message: "[recovery] reachable-nest box-list read failed (falling back to the device store): \(error)")
            }
            await client.disconnect()
            if !boxes.isEmpty { return boxes }
        }
        return loadLocal(ownerSecret: ownerSecret)
    }

    /// The device's own store alone — never throws (a fault logs and reads empty).
    public static func loadLocal(ownerSecret: Data) -> [String] {
        do {
            return try FaunaFFISwift.deploymentSeedsLocal(
                ownerSecret: ownerSecret, appDataDir: APIClient.recoveryConfigDataDir,
                storeContainerDir: AccountStateDir.storeContainerDir
            ).map(\.nestActorId)
        } catch {
            logMessage(level: .warn, target: "fauna.recovery",
                       message: "[recovery] device-store box-list read failed: \(error)")
            return []
        }
    }
}
