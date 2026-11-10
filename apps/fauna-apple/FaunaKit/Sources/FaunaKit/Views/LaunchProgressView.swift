import SwiftUI

/// The `launch_progress` launch surface — `onboarding.md` § App-launch routing:
/// the spinner held while the launch verdict is still out, before any retry
/// state is known (`launch_retry` models the failure case).
///
/// Shared by macOS (`MacAppState.LaunchGate.launching`) and iOS
/// (`AppState.launchPending`: the optimistic entry did not enter) per
/// priority #2. Elements per `ui.yaml` `onboarding.launch_progress`: the
/// "Signing in…" text in `launch-status` (a macOS + iOS `platform_elements`
/// entry), led by `launch-identity-label` — the bound account's cached
/// `handle@domain · tier` (`onboarding.md` § App-launch routing → *The cached
/// identity on the launch surfaces*), absent while the cache holds no handle.
public struct LaunchProgressView: View {
    /// Read once per surface from the shared registry
    /// (`FaunaAccounts.launchIdentityLine`, bound-else-active).
    @State private var identityLine: String? = FaunaAccounts.launchIdentityLine()

    public init() {}

    public var body: some View {
        VStack(spacing: 16) {
            if let identityLine {
                LaunchIdentityLabel(line: identityLine)
            }
            ProgressView()
            Text(L.launch.signingIn)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.launchStatus)
                .automationValue(Ids.launchStatus, text: { L.launch.signingIn })
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// The bare identity line carrying `launch-identity-label` — shared by
/// `LaunchProgressView` and `LaunchRetryView` (android's `LaunchIdentityLabel`).
struct LaunchIdentityLabel: View {
    let line: String

    var body: some View {
        automationText(Ids.launchIdentityLabel, line)
            .font(.callout)
            .multilineTextAlignment(.center)
    }
}
