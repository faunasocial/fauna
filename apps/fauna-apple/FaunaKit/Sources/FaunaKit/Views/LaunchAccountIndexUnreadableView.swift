import SwiftUI

/// The `launch_account_index_unreadable` surface — the saved account index at
/// `fauna/index` is present and this build cannot use it
/// (`docs/goal/architecture/version-compatibility.md` § 5 item 9;
/// `docs/goal/behavior/onboarding.md` § App-launch routing — the row checked
/// before every other). Terminal and non-retryable: no retry reparses a blob,
/// and there is never a "use a different nest" fallthrough — the nest is not
/// the problem.
///
/// Two verdicts, told apart by `refusal`:
/// - `.newerBuild`: the accounts are intact and an update restores them, so
///   NOTHING else is offered — a start-over here would destroy exactly what
///   an update would have restored.
/// - `.malformed`: updating cannot help, so the documented client-side floor
///   (`long-term-store.md` § Cleanup contract) is reachable, but only through
///   a confirm that states the residual first — `confirming` gates that.
///
/// Reference implementation: tui `apps/fauna-tui/src/launch.rs`
/// `LaunchSurface::AccountIndexUnreadable` (`route()`'s element list,
/// `reveal_start_over`/`start_over`). Shared by macOS and iOS per priority #2,
/// same split `LaunchIdentityChangedView`/`LaunchNeedsUpdateView` already use.
public struct LaunchAccountIndexUnreadableView: View {
    private let refusal: AccountIndexRefusal
    private let confirming: Bool
    /// The confirm's refusal line while another instance serves an account
    /// the start-over would erase (`AccountIndexStartOver`) — painted as
    /// `error-message` beside the still-showing confirm (convention 2). iOS
    /// never sets it: it shares a store with nobody.
    private let error: String?
    private let onStartOver: () -> Void
    private let onConfirmStartOver: () -> Void

    public init(
        refusal: AccountIndexRefusal,
        confirming: Bool,
        error: String? = nil,
        onStartOver: @escaping () -> Void,
        onConfirmStartOver: @escaping () -> Void
    ) {
        self.refusal = refusal
        self.confirming = confirming
        self.error = error
        self.onStartOver = onStartOver
        self.onConfirmStartOver = onConfirmStartOver
    }

    public var body: some View {
        VStack(spacing: 16) {
            switch (refusal, confirming) {
            case (.newerBuild, _):
                warning(L.onboarding.launch.indexNewerBuild)
            case (.malformed, false):
                warning(L.onboarding.launch.indexMalformed)
                Button(L.onboarding.launch.indexMalformedReset) { onStartOver() }
                    .accessibilityIdentifier(Ids.accountIndexResetButton)
                    .automationActivate(Ids.accountIndexResetButton) { onStartOver() }
            case (.malformed, true):
                warning(L.onboarding.launch.indexMalformedResetResidual)
                Button(L.onboarding.launch.indexMalformedResetConfirm, role: .destructive) {
                    onConfirmStartOver()
                }
                .accessibilityIdentifier(Ids.accountIndexResetConfirmButton)
                .automationActivate(Ids.accountIndexResetConfirmButton) { onConfirmStartOver() }
                ErrorBanner(message: error ?? "")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }

    private func warning(_ text: String) -> some View {
        Text(text)
            .multilineTextAlignment(.center)
            .accessibilityIdentifier(Ids.accountIndexRefusalWarning)
            .automationValue(Ids.accountIndexRefusalWarning, text: { text })
    }
}
