import SwiftUI
import FaunaKit

/// The watch's session identity, held in the shared account registry like every
/// other apple surface (`long-term-store.md` § Downgrade mirror + abandoned-append
/// recovery, RETIRED 2026-09-24: the registry is the only store). The phone hands
/// the watch its identity; the watch enrols it and reads it back through
/// `FaunaAccounts.sessionMaterial`.
@MainActor @Observable
class WatchAppState {
    var isBootstrapped: Bool = false
    var secretHex: String?
    var actorId: String?
    var nodeUrl: String?
    var handle: String?
    var isConnected: Bool = false

    private let keychain = KeychainStore()

    /// Try to load credentials from the registry on launch
    func loadFromKeychain() {
        guard let material = FaunaAccounts.sessionMaterial(keychain: keychain),
              let nodeUrl = material.nestUrl else {
            isBootstrapped = false
            return
        }
        self.secretHex = material.secretHex
        self.nodeUrl = nodeUrl
        self.actorId = material.actorId
        self.handle = material.handle
        isBootstrapped = true
    }

    /// Store credentials received from iPhone
    func bootstrap(secret: String, nodeUrl: String, handle: String) {
        do {
            let registry = FaunaAccounts.registry(keychain: keychain)
            let actorId = try registry.addAccount(secretHex: secret, nestUrl: nodeUrl, deviceId: nil)
            try registry.setActive(actorId: actorId)
            try registry.updateCache(actorId: actorId, handle: handle, domain: nil, tier: nil)
            self.secretHex = secret
            self.nodeUrl = nodeUrl
            self.actorId = actorId
            self.handle = handle
            self.isBootstrapped = true
        } catch {
            print("WatchAppState bootstrap error: \(error)")
        }
    }

    /// Clear credentials on sign-out
    func signOut() {
        _ = FaunaAccounts.registry(keychain: keychain).clearAll()
        secretHex = nil
        nodeUrl = nil
        actorId = nil
        handle = nil
        isBootstrapped = false
        isConnected = false
    }
}
