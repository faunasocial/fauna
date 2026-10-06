#if canImport(WatchConnectivity)
import WatchConnectivity
import SwiftUI

@MainActor @Observable
class IOSWatchConnectivityManager: NSObject, WCSessionDelegate {
    var isPaired: Bool = false
    var isWatchAppInstalled: Bool = false

    private var session: WCSession?

    func activate() {
        guard WCSession.isSupported() else { return }
        let session = WCSession.default
        session.delegate = self
        session.activate()
        self.session = session
    }

    func sendCredentials(secret: String, nodeUrl: String, handle: String) {
        guard let session, session.isWatchAppInstalled else { return }
        session.transferUserInfo([
            "secret": secret,
            "nodeUrl": nodeUrl,
            "handle": handle,
        ])
    }

    func sendSignOut() {
        guard let session, session.isWatchAppInstalled else { return }
        session.transferUserInfo(["signOut": true])
    }

    /// Both `WCSessionDelegate` callbacks below refresh the same two fields
    /// off the same `WCSession` — one shared body instead of two copies.
    private func refreshPairingState(from session: WCSession) {
        isPaired = session.isPaired
        isWatchAppInstalled = session.isWatchAppInstalled
    }

    // MARK: - WCSessionDelegate

    nonisolated func session(_ session: WCSession,
                             activationDidCompleteWith activationState: WCSessionActivationState,
                             error: Error?) {
        Task { @MainActor in refreshPairingState(from: session) }
    }

    nonisolated func sessionDidBecomeInactive(_ session: WCSession) {}
    nonisolated func sessionDidDeactivate(_ session: WCSession) {
        session.activate()
    }

    nonisolated func sessionWatchStateDidChange(_ session: WCSession) {
        Task { @MainActor in refreshPairingState(from: session) }
    }
}
#else
// Platforms without WatchConnectivity (e.g. the macOS host that builds — but
// never runs — the iOS app for `swift test`): a no-op stub so the iOS settings
// views typecheck. Mirrors the public surface used by `SettingsView`.
import SwiftUI

@MainActor @Observable
class IOSWatchConnectivityManager {
    var isPaired: Bool = false
    var isWatchAppInstalled: Bool = false

    func activate() {}
    func sendCredentials(secret: String, nodeUrl: String, handle: String) {}
    func sendSignOut() {}
}
#endif
