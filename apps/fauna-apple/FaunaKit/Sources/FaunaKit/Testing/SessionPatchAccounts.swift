import Foundation

// Compiled out of release artifacts (`e2e-conventions.md` convention 15), like
// the app shells' whole `applySessionPatch` surface that calls into it and like
// its `Testing/` siblings — the runtime `FAUNA_E2E_*` gates are the inner switch
// WITHIN a test-capable build, never the boundary.
#if DEBUG

/// The **account-registry** half of a TestAgent `session` patch.
///
/// A patch says "become this actor". The registry is the only store
/// (`long-term-store.md` § Downgrade mirror + abandoned-append recovery, RETIRED
/// 2026-09-24) and what every launch resolves the session identity from
/// (`FaunaAccounts.sessionMaterial`, `FaunaMacApp.completeAuthenticatedLaunch`:
/// `boundActorId ?? registry.active()`), so a patch is durable only if it lands
/// there: the patched actor is enrolled and made active, and the patch's
/// `node_url` / `handle` land in that actor's per-actor slot and server-data
/// cache. Anything less is convention 11's *"a half-applied command is a
/// dropped command"* in its most expensive form, because it does not fail: the
/// app answers correctly and **for the wrong actor**, so the caller reads a
/// correct-but-empty page and blames the feature (measured on
/// `test_identity_succession_ceremony.py --app macos`).
///
/// Shared by both shells so macOS and iOS cannot drift (priority #1/#2); the
/// windows twin is `App.xaml.cs`'s session patch (AddAccount + SetActive /
/// SetNestUrl / UpdateCache).
///
/// Owner: `docs/goal/architecture/apps/account-scoping.md` § Serialized switching.
public enum SessionPatchAccounts {

    /// The **registry + in-memory session** half of a TestAgent `session` patch —
    /// persisting whichever of `secret_hex`/`node_url`/`handle` the patch carries
    /// into both the observed `SessionState` and the registry, ahead of
    /// `adoptPatchedActor` below and each shell's own divergent
    /// `authenticated`/client-rebuild handling.
    ///
    /// `device_id` lands in the `SessionState` only: an injected id is either the
    /// forced 32-byte id (persisted per-actor by `adoptPatchedActor`) or the e2e
    /// un-forced placeholder the shells resolve through `deviceIdForActor` — never a
    /// value to persist verbatim, or the derivation would hand the placeholder back.
    ///
    /// An authenticated→authenticated patch to a DIFFERENT actor is the e2e
    /// agent's in-place account-switch shortcut — the same event the real
    /// UI's account switcher resets for (`tearDownSessionForSwitch`) before
    /// re-launching. Without `dropActorScopedState`, per-actor state it owns
    /// (Guardian Notify's pending accumulator, the DNS/subscriptions
    /// cadences, FeedVM's cached posts, …) from the outgoing actor survives
    /// into the incoming one's render — mirrors tui's `apply_session_patch`
    /// fix for the identical gap. Compared
    /// BEFORE any field below overwrites `s.actorId`; a `nil` outgoing actor
    /// is a first login, not a switch, and is a no-op.
    ///
    /// The cached handle is the ONLY source `AdminNestVM.factoryReset()` falls
    /// back to when the live `getAccount()` call fails (e.g. the nest is
    /// already unreachable), so an injected session that sets the in-memory
    /// handle but never persists it leaves that fallback with nothing to
    /// read — found chasing the `within_grace` crash-recovery journey,
    /// where a still-claimed box's factory-reset resume needs its cached
    /// handle.
    public static func applyPatchedFields(
        _ session: [String: Any],
        into s: SessionState,
        keychain: KeychainStore,
        dropActorScopedState: () -> Void
    ) {
        if let secret = session["secret_hex"] as? String,
           let incomingActorId = try? actor_id_from_secret(secret),
           let outgoingActorId = s.actorId,
           outgoingActorId != incomingActorId {
            dropActorScopedState()
        }

        let registry = FaunaAccounts.registry(keychain: keychain)
        if let secret = session["secret_hex"] as? String {
            s.secretHex = secret
            if let actorId = try? actor_id_from_secret(secret) {
                s.actorId = actorId
            }
            do {
                let actorId = try registry.addAccount(secretHex: secret, nestUrl: nil, deviceId: nil)
                try registry.setActive(actorId: actorId)
            } catch {
                logMessage(level: .error, target: "fauna.app", message: "[applySessionPatch] registry enrol failed: \(error)")
            }
        }
        if let nodeUrl = session["node_url"] as? String {
            s.nodeUrl = nodeUrl
            if let actorId = s.actorId {
                registry.setNestUrl(actorId: actorId, nestUrl: nodeUrl)
            }
        }
        if let deviceId = session["device_id"] as? String {
            s.deviceId = deviceId
        }
        if let handle = session["handle"] as? String {
            s.handle = handle
            if let actorId = s.actorId {
                // `updateCache` replaces all three fields — keep domain/tier.
                let entry = registry.list().first { $0.actorId == actorId }
                do {
                    try registry.updateCache(
                        actorId: actorId, handle: handle,
                        domain: entry?.domain, tier: entry?.tier)
                } catch {
                    logMessage(level: .error, target: "fauna.app", message: "[applySessionPatch] registry handle cache write failed: \(error)")
                }
            }
        }
    }

    /// Enrol the patched actor with its resolved (nest_url, device_id) and make it
    /// the active account — call BEFORE the `FaunaClient` is rebuilt: the rebuild
    /// must see a registry that already agrees with the client it is about to
    /// build — a launch that resolves actor A while the client speaks for actor B
    /// is refused by the nest at the WS handshake (`security.md` § Cross-connection
    /// binding) and retried forever, so every WS-RPC call hangs instead of failing.
    ///
    /// Not switch-only: with the single slot retired, a first login that left the
    /// registry empty would leave the next launch with no session at all.
    ///
    /// `addAccount` is an idempotent upsert touching only the slots given, so a
    /// successor the ceremony already persisted is updated rather than duplicated.
    /// Plain `setActive`, never `setActiveConfirmed`: the Stage-2 re-auth bit
    /// asserts a prompt just succeeded, and no prompt runs for a test-injected
    /// identity.
    ///
    /// - Parameters:
    ///   - secretHex: the patch's `secret_hex` — the identity to become.
    ///   - nestUrl: the patch's resolved `node_url`.
    ///   - deviceId: the patch's resolved `device_id` (the shells default it).
    /// - Returns: `true` iff the registry now names the patched actor as active.
    @discardableResult
    public static func adoptPatchedActor(
        secretHex: String,
        nestUrl: String,
        deviceId: String,
        keychain: KeychainStore = KeychainStore()
    ) -> Bool {
        do {
            let registry = FaunaAccounts.registry(keychain: keychain)
            let actorId = try registry.addAccount(
                secretHex: secretHex, nestUrl: nestUrl, deviceId: deviceId)
            try registry.setActive(actorId: actorId)
            return true
        } catch {
            // Loud, never silent: the shell is about to render as authenticated,
            // and a registry left on another actor is exactly the
            // correct-but-empty failure this function exists to remove.
            logMessage(
                level: .error, target: "fauna.accounts",
                message: "[session-patch] could not enrol/activate the patched actor: \(error) — the next launch will resolve a DIFFERENT actor and its session will 403")
            return false
        }
    }
}

#endif
