import SwiftUI
import FaunaKit

/// Global local sync-agent process-health indicator, sibling to
/// `ConnectionStatusBar` (`sync-agent-status`; `sync-agent.md` § Local agent
/// health, `ui.yaml` `components.sync-agent-status`). Distinct concept:
/// `ConnectionStatusBar` is the nest WS-RPC link, this is whether the LOCAL
/// per-user `fauna-sync-agent` process is running and current. macOS-only
/// (desktop; no local agent on iOS), so this lives in `FaunaMacOSLib`, not the
/// shared FaunaKit package `ConnectionStatusBar` uses.
///
/// Reads a `SyncAgentHealthModel` (a reference, like `ConnectionStatusBar`
/// takes `FaunaClient?`) so the in-process e2e automation registry's captured
/// text closure re-reads the model live on each poll tick rather than a frozen
/// value.
struct SyncAgentStatusBar: View {
    private let model: SyncAgentHealthModel?

    init(model: SyncAgentHealthModel?) {
        self.model = model
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            automationText(Ids.syncAgentStatus, model?.stateText ?? L.status.syncAgent.notRunning)
                .font(.caption)
                .foregroundStyle(.secondary)
            // Dim subtitle row (version/uptime) — linux's placement choice,
            // reused here (`sync-agent.md` § Local agent health: "exact
            // placement is a per-app implementation choice").
            HStack(spacing: 8) {
                automationText(Ids.syncAgentStatusVersion, model?.versionText ?? "")
                automationText(Ids.syncAgentStatusUptime, model?.uptimeText ?? "")
            }
            .font(.caption2)
            .foregroundStyle(.tertiary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
        .padding(.vertical, 4)
        .background(.bar)
    }
}
