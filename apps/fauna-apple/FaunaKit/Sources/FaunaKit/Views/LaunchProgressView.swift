import SwiftUI

/// The `launch_progress` launch surface — `onboarding.md` § App-launch routing:
/// the spinner held while the launch verdict is still out, before any retry
/// state is known (`launch_retry` models the failure case).
///
/// Shared by macOS (`MacAppState.LaunchGate.launching`) and iOS
/// (`AppState.launchPending`: the optimistic entry did not enter) per
/// priority #2. Elements per `ui.yaml` `onboarding.launch_progress`: the
/// "Signing in…" text in `launch-status` (a macOS + iOS `platform_elements`
/// entry).
public struct LaunchProgressView: View {
    public init() {}

    public var body: some View {
        VStack(spacing: 16) {
            ProgressView()
            Text(L.launch.signingIn)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.launchStatus)
                .automationValue(Ids.launchStatus, text: { L.launch.signingIn })
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
