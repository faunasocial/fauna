import SwiftUI

/// The `launch_sign_in_refused` launch surface — `onboarding.md` § App-launch
/// routing, the previously-signed-in row.
///
/// The saved nest answered the silent challenge with the opaque
/// `fauna.auth.not_registered` for an identity this app signed in with before
/// (`LaunchSnapshot.signInRefused`, carried on the terminal
/// `Offline { transient: false }` snapshot's side channel). The account was
/// suspended or removed; the app cannot tell which, and the copy deliberately
/// leaves it unsaid. The one terminal launch surface WITH Retry: the remedy (the
/// admin's restore) happens off this device, and the user's way back in is
/// retrying the challenge. Plus "Use a different nest".
///
/// Shared by macOS (`MacAppState.LaunchGate.signInRefused`) and iOS
/// (`AppState.signInRefusedMessage`) per priority #2; twin of tui's
/// `LaunchSurface::SignInRefused`, android's `LaunchSignInRefusedScreen`, linux's
/// `LaunchPhase::SignInRefused` and web's onboarding page. The sentence gets its
/// OWN element (`launch-sign-in-refused-notice`), not the generic
/// `error-message`, so an e2e asserting "the user was told this nest no longer
/// signs them in" cannot be satisfied by any old error text.
public struct LaunchSignInRefusedView: View {
    private let message: String
    private let onRetry: () -> Void
    private let onUseDifferentNest: () -> Void

    /// `message` is the machine's `last_error` (the same localized sentence the
    /// strings table carries as `onboarding.launch.sign_in_refused`); an empty one
    /// falls back to the table, so the notice is never blank.
    public init(
        message: String,
        onRetry: @escaping () -> Void,
        onUseDifferentNest: @escaping () -> Void
    ) {
        self.message = message.isEmpty ? L.onboarding.launch.signInRefused : message
        self.onRetry = onRetry
        self.onUseDifferentNest = onUseDifferentNest
    }

    public var body: some View {
        VStack(spacing: 16) {
            Text(L.onboarding.launch.signInRefusedTitle)
                .font(.title3)
            Text(message)
                .multilineTextAlignment(.center)
                .accessibilityIdentifier(Ids.launchSignInRefusedNotice)
                .automationValue(Ids.launchSignInRefusedNotice, text: { message })

            Button(L.launch.retryButton) { onRetry() }
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier(Ids.launchRetryButton)
                .automationActivate(Ids.launchRetryButton) { onRetry() }

            Button(L.launch.useDifferentNest) { onUseDifferentNest() }
                .accessibilityIdentifier(Ids.launchFallthroughButton)
                .automationActivate(Ids.launchFallthroughButton) { onUseDifferentNest() }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
}
