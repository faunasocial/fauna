import SwiftUI

/// Non-retry "update required" launch surface — version-compatibility.md Dim 4 /
/// onboarding.md § App-launch routing (the version-mismatch row).
///
/// Shown on the **direct silent-challenge path** when the nest authoritatively
/// reports it is outdated: a degraded "needs-update" nest answers
/// `fauna.nest.outdated` to the connect/auth handshake, which the shared FFI maps
/// to `FfiError.NestOutdated` (`libs/fauna-ffi/src/auth.rs`). Unlike the transient
/// retry surface, this is **not** a client-side guess — the nest *told us* it
/// cannot serve this client version, so retrying the same nest is futile and a
/// Retry CTA would just spin a doomed loop.
///
/// Renders the already-localized actionable `message` (from the nest's
/// `error.nest.outdated`, never re-localized) in the canonical `error-message`
/// element via the shared `ErrorBanner`, **omits `launch-retry-button`**, and keeps
/// only `launch-fallthrough-button` ("Use a different nest") — the sole escape,
/// since there is no retry. Shared by macOS (`MacAppState.LaunchPhase.needsUpdate`)
/// and iOS (`AppState.needsUpdateMessage`) per priority #2; mirrors linux
/// `views/launch.rs::LaunchPhase::NeedsUpdate` + android `LaunchNeedsUpdateScreen`
/// (same shape: message in `error-message`, no retry, keep fallthrough).
public struct LaunchNeedsUpdateView: View {
    private let message: String
    private let onUseDifferentNest: () -> Void

    public init(message: String, onUseDifferentNest: @escaping () -> Void) {
        self.message = message
        self.onUseDifferentNest = onUseDifferentNest
    }

    public var body: some View {
        VStack(spacing: 16) {
            ErrorBanner(message: message)
            Button(L.launch.useDifferentNest) { onUseDifferentNest() }
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier(Ids.launchFallthroughButton)
                .automationActivate(Ids.launchFallthroughButton) { onUseDifferentNest() }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
}
