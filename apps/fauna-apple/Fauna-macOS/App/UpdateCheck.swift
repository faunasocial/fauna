import SwiftUI
import FaunaKit

/// The newer-version check — the macOS leg of `installers/README.md` § Knowing a
/// newer version is out: the app answers when you ask, and looks once per
/// sign-in, and only ever TELLS you. It never polls on a timer, never downloads
/// and never replaces itself, and has no toggle (the 2026-10-03 ruling retired
/// the old automatic check, background download and in-place replacement).
///
/// Everything that decides the answer is shared Rust: the round trip, the
/// feed's shape, the semver rule and the release page all come through the
/// `fauna-ffi` face of `fauna_client::update_look` (`checkForNewerRelease`,
/// `lookAtSignIn`, `releaseFeedOrigin`) — the same calls linux and tui make
/// directly. This type only holds what the page paints.
///
/// One instance per process (`MacAppState.updates`), shared by both doors onto
/// the asked check — Settings → General's About block and the app menu's
/// "Check for Updates" — and by the sign-in look, so all three paint the same
/// `update-available-notice` in the same place.
@MainActor
@Observable
final class UpdateCheck {
    /// The check's state, carried in the button's own label — the walk observes
    /// resolution as the label leaving "Checking…" (`SettingsActions.check_for_updates`).
    enum Phase: Equatable {
        case idle
        case checking
        case newer(version: String)
        case upToDate
        case failed
    }

    private(set) var phase: Phase = .idle

    /// The notice's text once a check or the sign-in look found a newer release
    /// (session-local, never persisted); `nil` paints nothing.
    private(set) var notice: String?

    /// The version this build is — the one workspace product version every app
    /// and the nest share (`product-version.md` § The model).
    let runningVersion: String = faunaFfiBuildVersion()

    /// Built by `MacAppState`'s own (nonisolated) initializer.
    nonisolated init() {}

    /// The button's label for the current phase, in the shared strings.
    var buttonLabel: String {
        switch phase {
        case .idle: L.settings.checkForUpdates
        case .checking: L.common.checking
        case .newer(let version): L.settings.generalPage.updateAvailable(version: version)
        case .upToDate: L.settings.upToDate
        case .failed: L.settings.checkFailed
        }
    }

    var isChecking: Bool { phase == .checking }

    /// The asked check. The label holds its answer for three seconds, then the
    /// button is ready to ask again (linux's shape).
    func check() {
        guard !isChecking else { return }
        phase = .checking
        Task { @MainActor in
            switch await checkForNewerRelease(feedOrigin: Self.feedOrigin, userAgent: Self.userAgent) {
            case .newer(let release):
                phase = .newer(version: release.version)
                show(release)
            case .upToDate:
                phase = .upToDate
            case .failed:
                phase = .failed
            }
            try? await Task.sleep(for: .seconds(3))
            phase = .idle
        }
    }

    /// The once-per-sign-in look — called from each path that builds a
    /// signed-in session. Silent unless a newer release is out.
    func lookOnceAtSignIn() {
        Task { @MainActor in
            if let release = await lookAtSignIn(feedOrigin: Self.feedOrigin, userAgent: Self.userAgent) {
                show(release)
            }
        }
    }

    private func show(_ release: NewerRelease) {
        notice = L.settings.updateAvailableNotice(version: release.version, url: release.releasePageUrl)
    }

    /// Production's feed origin, or — only in a test-capable build under e2e
    /// automation — the harness's stub feed (convention 15: `E2eEnv`'s release
    /// twin answers `nil` with no variable name compiled in).
    private static var feedOrigin: String {
        if FaunaE2E.isActive, let stub = E2eEnv.releaseFeedUrl {
            return stub
        }
        return releaseFeedOrigin()
    }

    private static var userAgent: String { "fauna-macos/\(faunaFfiBuildVersion())" }
}

/// The app menu's "Check for Updates…" — a second door onto the same asked
/// check Settings → General carries.
struct CheckForUpdatesMenuItem: View {
    let updates: UpdateCheck

    var body: some View {
        Button("\(L.settings.checkForUpdates)…", action: updates.check)
            .disabled(updates.isChecking)
    }
}
