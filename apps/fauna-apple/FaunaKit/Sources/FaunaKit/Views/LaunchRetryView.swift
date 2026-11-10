import SwiftUI

/// The `launch_retry` launch surface — `onboarding.md` § App-launch routing, the
/// *Network failure or nest unreachable* row: the launch verdict was
/// `Offline { transient: true }` and no session was entered.
///
/// Shared by macOS (`MacAppState.LaunchGate.retrying`) and iOS
/// (`AppState.launchRetryError`: a launch whose optimistic entry did not enter,
/// or an entered session left by `LostNestOffer`) per priority #2. Elements per `ui.yaml`
/// `onboarding.launch_retry`: the cached identity in `launch-identity-label`
/// (absent while the cache holds no handle), the error sentence in `launch-transient-error`,
/// Retry, "Use a different nest", *Retire a server* (always shown —
/// `nest-retirement.md` § Layout & flow — when the host passes `onRetire`; the
/// Fauna Kids build compiles nest setup out and passes `nil`), and the
/// surviving-device `launch-recover-button`, revealed only once the launch-time
/// box read resolves ≥1 box (`box-recovery.md` § Recovery UI) — after the
/// surface has painted, so a slow or dead saved nest never delays it. macOS
/// pairs its own title (`launch-error-title`, a macOS `platform_elements` entry).
public struct LaunchRetryView: View {
    private let error: String
    private let onRetry: () -> Void
    private let onUseDifferentNest: () -> Void
    private let loadRecoverableBoxes: (() async -> [String])?
    private let onRecover: ([String]) -> Void
    private let onRetire: (() -> Void)?

    /// `launch-identity-label`: the bound account's cached identity
    /// (`FaunaAccounts.launchIdentityLine`); `nil` → absent.
    @State private var identityLine: String? = FaunaAccounts.launchIdentityLine()
    /// The custodied boxes the launch-time read found.
    @State private var recoverableBoxes: [String] = []
    /// This instance's claim on the error slot (`AppMessages.bannerAppeared`).
    @State private var owner = UUID()

    public init(
        error: String,
        onRetry: @escaping () -> Void,
        onUseDifferentNest: @escaping () -> Void,
        loadRecoverableBoxes: (() async -> [String])?,
        onRecover: @escaping ([String]) -> Void,
        onRetire: (() -> Void)?
    ) {
        self.error = error
        self.onRetry = onRetry
        self.onUseDifferentNest = onUseDifferentNest
        self.loadRecoverableBoxes = loadRecoverableBoxes
        self.onRecover = onRecover
        self.onRetire = onRetire
    }

    public var body: some View {
        VStack(spacing: 16) {
            if let identityLine {
                LaunchIdentityLabel(line: identityLine)
            }
            #if os(macOS)
            Text(L.launch.retryTitle)
                .font(.title3)
                .accessibilityIdentifier(Ids.launchErrorTitle)
                .automationValue(Ids.launchErrorTitle, text: { L.launch.retryTitle })
            #endif
            // Its own element, not the generic `error-message`, as on every other
            // app — but still published into `AppMessages.error`, as `ErrorBanner`
            // does, so this pre-authenticated surface stays reachable through the
            // driver's `error_text()` like any authenticated page.
            Text(error)
                .foregroundStyle(.red)
                .multilineTextAlignment(.center)
                .accessibilityIdentifier(Ids.launchTransientError)
                .automationValue(Ids.launchTransientError, text: { error })
                .onAppear {
                    Task { @MainActor in AppMessages.bannerAppeared(.error, owner: owner, message: error) }
                }
                .onDisappear {
                    Task { @MainActor in AppMessages.bannerDisappeared(.error, owner: owner, message: error) }
                }
                .id(error)
            Button(L.launch.retryButton) { onRetry() }
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier(Ids.launchRetryButton)
                .automationActivate(Ids.launchRetryButton) { onRetry() }
            Button(L.launch.useDifferentNest) { onUseDifferentNest() }
                .accessibilityIdentifier(Ids.launchFallthroughButton)
                .automationActivate(Ids.launchFallthroughButton) { onUseDifferentNest() }
            if !recoverableBoxes.isEmpty {
                Button(L.onboarding.launch.recoverLostBox) { onRecover(recoverableBoxes) }
                    .accessibilityIdentifier(Ids.launchRecoverButton)
                    .automationActivate(Ids.launchRecoverButton) { onRecover(recoverableBoxes) }
            }
            #if !FAUNA_KIDS
            // Nest retirement is excised from the kids build (its id sits in the
            // kids-gated `Ids` extension).
            if let onRetire {
                Button(L.onboarding.launch.retireServer) { onRetire() }
                    .accessibilityIdentifier(Ids.launchRetireButton)
                    .automationActivate(Ids.launchRetireButton) { onRetire() }
            }
            #endif
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
        .task { recoverableBoxes = await loadRecoverableBoxes?() ?? [] }
    }
}
