import SwiftUI

/// Global connection-status indicator pinned to the top of the shell — the live
/// nest WS-RPC connection state (`connection-status`). The Apple twin of linux's
/// top-of-sidebar indicator, the web SPA's `connectionStatus` store, and android's
/// `ConnectionStatusBar`. Reads **Connected / Connecting… / Disconnected** off the
/// shared `NestClient::connection_state()` watch (pumped through UniFFI into
/// `FaunaClient.connectionState`); a transient Watchtower-swap or drop shows live
/// as "Connecting…" and returns to "Connected" on reconnect — it is the *visible*
/// half of the reconnect machinery and is **never** surfaced as an error
/// banner/toast (`transport.md` § Connection lifecycle → Connection-status
/// indicator). Always visible (including "Connected"), so tests read its text to
/// assert the client holds/loses the connection. One shared FaunaKit view for
/// macOS + iOS (priority #2).
///
/// Takes the `@Observable FaunaClient` (a **reference**), not a snapshot value, on
/// purpose: the in-process e2e registry captures the `automationValue` text reader
/// once on `.onAppear` and reads it live, so the closure must dereference a live
/// reference each lookup — a frozen value would pin the indicator at its initial
/// state. Reading `client.connectionState` in `body` also establishes the
/// SwiftUI Observation dependency that re-renders the visible `Text` on each
/// transition.
public struct ConnectionStatusBar: View {
    private let client: FaunaClient?

    public init(client: FaunaClient?) {
        self.client = client
    }

    public var body: some View {
        Text(label)
            .font(.caption)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity)
            .padding(.horizontal, 16)
            .padding(.vertical, 4)
            .background(.bar)
            .accessibilityIdentifier(Ids.connectionStatus)
            // In-process e2e read of `connection-status`: the macOS/iOS driver
            // reads `/element/text` from the AutomationRegistry, not XCUITest's
            // a11y tree. The closure reads `client.connectionState` live (through
            // the FaunaClient reference) so it reflects each transition, not the
            // value at registration. Env-gated no-op in production.
            .automationValue(Ids.connectionStatus, text: { label })
    }

    /// Resolve the four transport states through the shared
    /// `connectionStateLabel` decision (`transport.md` § Connection-status
    /// indicator, `## Implementation status today` gap 3) instead of a
    /// hand-rolled per-app match — still no `Reconnecting` state, a *swap*
    /// shows as "Connecting…". Defaults to `.connecting` before the
    /// client/connection exists.
    private var label: String {
        renderLocalizedText(connectionStateLabel(state: client?.connectionState ?? .connecting))
    }
}
