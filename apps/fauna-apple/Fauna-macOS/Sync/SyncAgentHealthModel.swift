import Foundation
import FaunaKit

/// The subset of the sync-agent control channel the health poll needs —
/// `FfiSyncAgentProvisioner.agentHealth`. Mirrors `LocationControlChannel`'s seam
/// (`LocationMap.swift`) so unit tests substitute a fake instead of a real agent.
protocol AgentHealthChannel: AnyObject, Sendable {
    func agentHealth(localBuildVersion: String) async -> FfiAgentStatus
    /// `GetServiceStatus`'s sync signal — `nil` while the agent is unreachable.
    func syncStatus() async -> FfiAgentSyncStatus?
    func listEngineHolds() async throws -> [FfiEngineHold]
    func listLocations() async throws -> [FfiAgentLocation]
}

extension FfiSyncAgentProvisioner: AgentHealthChannel {}

/// Observable local `fauna-sync-agent` process-health indicator — the macOS peer
/// of linux's `update_sync_agent_status_indicator`/`start_sync_agent_status_poll`
/// and windows' `PollSyncAgentStatusAsync` (`sync-agent.md` § Local agent health).
/// Distinct from `ConnectionStatusBar`: that is the nest WS-RPC link, this is
/// whether the LOCAL per-user agent process is up and current. Polls
/// `GetServiceStatus` every 10 s (matching both siblings) through the shared
/// tri-state derivation (`FfiSyncAgentProvisioner.agentHealth` →
/// `fauna_client_sync::agent::agent_health_state`) — never reimplementing the
/// Running/RestartPending/NotRunning comparison here.
@MainActor
@Observable
final class SyncAgentHealthModel {
    private(set) var stateText: String
    private(set) var versionText: String = ""
    private(set) var uptimeText: String = ""
    /// The Status page's sync leg (`status-sync-pending` / `status-sync-last`,
    /// `ui/status.md` § State & data shape) — the agent's local backlog off the
    /// same `GetServiceStatus` tick, projected by shared Rust
    /// (`statusSyncLeg`). `nil` until the agent first answers and again whenever
    /// it stops: an agent that is down is `stateText`'s to report, not a zero
    /// backlog here.
    private(set) var syncLeg: FfiStatusSyncLeg?

    private var channel: (any AgentHealthChannel)?
    private var pollTask: Task<Void, Never>?

    /// `FaunaMacApp` wires this to `LocationsModel.foldEngineHolds` — the
    /// mass-delete floor's per-set hold, folded on this SAME 10 s tick because
    /// the hold is derived inside the agent on its own rescan cadence: no user
    /// gesture or reachability edge ever produces it
    /// (`delete-propagation.md` § A wholesale-vanished folder is
    /// infrastructure failure). Never fold this in `LocationsModel.reconcile()`
    /// — that runs on mutation/reachability edges only, and would leave a set
    /// that emptied minutes ago still painting as healthy between edges.
    var onEngineHoldsTick: (@Sendable ([FfiEngineHold]) async -> Void)?

    /// `FaunaMacApp` wires this to `LocationsModel.foldParks` — the binding
    /// PARK (`accessRevoked`), watched on this SAME tick for the hold's reason:
    /// the agent derives it when the owning nest refuses a write, and a
    /// demoted writer's app sees no gesture or reachability edge that would
    /// re-run the reconcile (`file-sync.md` § Multi-writer shared sets →
    /// *Revocation*; tui/linux fold it on their status poll likewise). Called
    /// only when `listLocations` answered: an unreachable agent folds nothing,
    /// so the rows keep their last-known park.
    var onLocationsTick: (@Sendable ([FfiAgentLocation]) async -> Void)?

    private static let pollIntervalSeconds: UInt64 = 10

    init() {
        stateText = L.status.syncAgent.notRunning
    }

    /// Start polling against `channel`. Idempotent — the poll loop starts once
    /// per model; a second call only swaps which channel it reads (harmless in
    /// practice, since the provisioner is built once per session).
    func start(channel: any AgentHealthChannel) {
        self.channel = channel
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            while let self, !Task.isCancelled {
                await self.poll()
                try? await Task.sleep(nanoseconds: Self.pollIntervalSeconds * 1_000_000_000)
            }
        }
    }

    /// Stop polling and reset to the not-running display (session teardown —
    /// every path that calls `FfiSyncAgentProvisioner.unprovision()`).
    func stop() {
        pollTask?.cancel()
        pollTask = nil
        channel = nil
        stateText = L.status.syncAgent.notRunning
        versionText = ""
        uptimeText = ""
        syncLeg = nil
    }

    /// Inject a channel WITHOUT starting the background poll loop (mirrors
    /// `LocationsModel.setChannelForTest`) — call `poll()` directly afterward
    /// to drive one tick deterministically, with no loop/timing race.
    func setChannelForTest(_ channel: any AgentHealthChannel) {
        self.channel = channel
    }

    /// One poll tick. Not `private`: the background loop calls it, and tests
    /// drive it directly (via `setChannelForTest`) instead of racing the
    /// loop's real 10 s sleep.
    func poll() async {
        guard let channel else { return }
        let health = await channel.agentHealth(localBuildVersion: faunaFfiBuildVersion())
        switch health.state {
        case .running: stateText = L.status.syncAgent.running
        case .restartPending: stateText = L.status.syncAgent.restartPending
        case .keysPending: stateText = L.status.syncAgent.keysPending
        case .notEnrolled: stateText = L.status.syncAgent.notEnrolled
        case .notRunning: stateText = L.status.syncAgent.notRunning
        }
        // version/uptime stay empty while not running (sync-agent.md § Local
        // agent health) — `health.state == .notRunning` is exactly the case
        // `agent_health_state` derives from a failed `GetServiceStatus` call.
        if health.state == .notRunning {
            versionText = ""
            uptimeText = ""
        } else {
            versionText = health.version
            uptimeText = ValueFormat.durationSecs(health.uptimeSecs)
        }

        syncLeg = await channel.syncStatus().map(statusSyncLeg(status:))

        // The mass-delete floor's per-set hold — the ONLY channel by which the
        // app learns a bound folder emptied. An unreachable/too-old agent
        // errors; the honest fold for that is an empty roster (a mirror, not a
        // skip — a hold that was showing must retract if the agent stops
        // reporting it), matching linux's `refresh_engine_holds`.
        let holds = (try? await channel.listEngineHolds()) ?? []
        await onEngineHoldsTick?(holds)

        // The binding park — skipped, not emptied, when the agent does not
        // answer (unlike the hold's mirror): an unanswered read is no evidence
        // the park lifted, and only a re-bind the agent reports clears it.
        if let locations = try? await channel.listLocations() {
            await onLocationsTick?(locations)
        }
    }
}
