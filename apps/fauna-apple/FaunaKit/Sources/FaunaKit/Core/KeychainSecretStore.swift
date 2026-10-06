import Foundation

/// Apple's implementation of `FfiSecretStore` — the **only** foreign seam the shared
/// account registry needs from this platform
/// (`docs/goal/architecture/long-term-store.md` § Multi-account evolution → Shared seam).
///
/// Everything that used to be typed Swift persistence logic — per-actor namespacing, the
/// pending-slot composition, the session material — lives in
/// `fauna_client_accounts` and reaches the Keychain through the three methods below. Apple
/// contributes a key/value store and no policy (priority #2). Its siblings are Windows over
/// Credential Manager, Android over `EncryptedSharedPreferences`, and web over
/// `LocalStorageSecretStore`; the registry cannot tell them apart.
///
/// **This class is a key→key map and nothing else.** If you find yourself adding a
/// condition here, it almost certainly belongs in `fauna-client-accounts` instead — that
/// asymmetry is the whole point of the seam.
///
/// `@unchecked Sendable` mirrors `NullLaunchObserver` and `ObserverBox` (FaunaKit's other
/// UniFFI callback conformances): the protocol is `Sendable`, and the only stored property
/// is a reference to the Keychain wrapper, whose operations are individually atomic
/// `SecItem*` calls under a lock.
public final class KeychainSecretStore: FfiSecretStore, @unchecked Sendable {
    private let keychain: KeychainStore

    public init(keychain: KeychainStore = KeychainStore()) {
        self.keychain = keychain
    }

    // Every logical key — `fauna/index`, `fauna/{actor_id}/secret`, and any key a future
    // registry version invents — is stored under its logical name **verbatim**. The
    // passthrough is what lets shared Rust grow new rows without an Apple change. There is
    // no mapping onto pre-registry native rows: the registry is the only store
    // (`long-term-store.md` § Downgrade mirror + abandoned-append recovery, RETIRED
    // 2026-09-24), so apple never asks for a single-slot key.

    // MARK: - FfiSecretStore

    public func get(key: String) -> String? {
        keychain.load(rawKey: key)
    }

    /// Best-effort by contract, on every platform: a locked keychain, a denied ACL, and a
    /// full disk all surface as a swallowed failure here (`FfiSecretStore` declares `set`
    /// infallible). That is not a gap — it is why the shared
    /// `mint_and_persist_pending_factory_reset` **reads the row back** and refuses to hand
    /// out a claim code it could not prove it persisted. A write that silently fails is
    /// caught by verification, never by a `throws` this seam does not have.
    public func set(key: String, value: String) {
        try? keychain.save(rawKey: key, value: value)
    }

    public func delete(key: String) {
        keychain.delete(rawKey: key)
    }
}
