import Foundation

/// The Settings Status sub-page's identity cell (`settings.md` § Layout & flow
/// item 1): `settings-handle-label` — the account's own address, `handle@domain`,
/// undecorated, the shared display twin `accountDisplayHandle` — and
/// `settings-tier-label`, the bare tier. Both read off the account registry's
/// server-data cache (`long-term-store.md` § Cross-app server-data cache), never
/// the quota reply and never the session's own handle, so they are on the
/// sub-page's first frame and stay while the nest is unreachable.
///
/// The twin of android's `readStatusAccount` and tui's `session_identity_cell`.
public struct IdentityCell: Equatable, Sendable {
    /// `handle@domain`, through the shared resolver.
    public let handle: String
    /// The cached tier; `nil` when the cache has none.
    public let tier: String?

    /// The cell for `actorId`'s registry entry — `nil` (the cell absent, not
    /// empty) while the cache holds no handle, or no account is in session.
    public static func read(actorId: String?, registry: FfiAccountRegistry) -> IdentityCell? {
        guard let actorId,
              let entry = registry.list().first(where: { $0.actorId == actorId }),
              let cached = entry.handle?.trimmingCharacters(in: .whitespaces), !cached.isEmpty,
              let handle = accountDisplayHandle(
                  sessionHandle: cached, entryHandle: cached, entryDomain: entry.domain)
        else { return nil }
        let tier = entry.tier?.trimmingCharacters(in: .whitespaces)
        return IdentityCell(handle: handle, tier: (tier?.isEmpty ?? true) ? nil : tier)
    }
}

extension SessionState {
    /// The session account's identity cell off this device's registry.
    ///
    /// Reads `handle` and `domain` first so a SwiftUI body that calls this is
    /// observed on them: every cache write (`applySelfIdentity`, the launch's
    /// material read) also writes those two mirrors, so the cell repaints on
    /// each identity refresh, while the PAINT itself comes from the registry.
    public var identityCell: IdentityCell? {
        _ = (handle, domain)
        return IdentityCell.read(actorId: actorId, registry: FaunaAccounts.registry())
    }
}

extension FaunaAccounts {
    /// `launch-identity-label` on `launch_progress` and `launch_retry`: the
    /// bound (else active) account's cached `handle@domain · tier`, composed in
    /// shared Rust (`FfiAccountRegistry.launchIdentityLine` →
    /// `fauna_client_accounts::identity_line`) — the call tui, linux and android
    /// make. `nil` while its cache holds no handle: the label is then absent
    /// (`onboarding.md` § App-launch routing → *The cached identity on the
    /// launch surfaces*).
    public static func launchIdentityLine(keychain: KeychainStore = KeychainStore()) -> String? {
        registry(keychain: keychain).launchIdentityLine()
    }
}
