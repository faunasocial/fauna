import Foundation
import FaunaKit

/// The subset of the sync-agent control channel `unprovisionSyncAgent()` needs —
/// mirrors `LocationControlChannel`/`AgentHealthChannel`'s seam (`LocationMap.swift`,
/// `SyncAgentHealthModel.swift`) so a unit test can order the await against a fake
/// instead of a real agent.
protocol SyncAgentUnprovisioning: AnyObject, Sendable {
    func unprovision() async throws
}

extension FfiSyncAgentProvisioner: SyncAgentUnprovisioning {}

/// Await the sync agent's un-provision reply before the caller's erase runs
/// (`sync-agent.md` § Control plane split: "The app awaits the reply before its
/// erase") — matches tui's `session::await_before_erase` and linux's blocking
/// `teardown()`. `nil` (this session never provisioned an agent) is a no-op; an
/// unreachable agent or a refusal still returns rather than throwing (degrade
/// open), so every caller's erase always proceeds.
@MainActor
func awaitSyncAgentUnprovision(_ provisioner: (any SyncAgentUnprovisioning)?) async {
    guard let provisioner else { return }
    do {
        try await provisioner.unprovision()
    } catch {
        logMessage(level: .warn, target: "fauna.sync",
                   message: "[sync-agent] unprovision failed: \(error)")
    }
}
