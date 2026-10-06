import WatchConnectivity
import SwiftUI

class WatchConnectivityDelegate: NSObject, WCSessionDelegate {
    var onCredentialsReceived: ((String, String, String) -> Void)?
    var onSignOut: (() -> Void)?

    func session(_ session: WCSession,
                 activationDidCompleteWith activationState: WCSessionActivationState,
                 error: Error?) {
        // Ready
    }

    func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        if userInfo["signOut"] as? Bool == true {
            Task { @MainActor in
                onSignOut?()
            }
            return
        }

        guard let secret = userInfo["secret"] as? String,
              let nodeUrl = userInfo["nodeUrl"] as? String,
              let handle = userInfo["handle"] as? String else { return }

        Task { @MainActor in
            onCredentialsReceived?(secret, nodeUrl, handle)
        }
    }
}

@MainActor
class WatchSessionManager {
    static let shared = WatchSessionManager()

    let delegate = WatchConnectivityDelegate()
    private var session: WCSession?

    func activate(appState: WatchAppState) {
        guard WCSession.isSupported() else { return }
        let session = WCSession.default

        delegate.onCredentialsReceived = { secret, nodeUrl, handle in
            appState.bootstrap(secret: secret, nodeUrl: nodeUrl, handle: handle)
        }

        delegate.onSignOut = {
            appState.signOut()
        }

        session.delegate = delegate
        session.activate()
        self.session = session
    }
}
