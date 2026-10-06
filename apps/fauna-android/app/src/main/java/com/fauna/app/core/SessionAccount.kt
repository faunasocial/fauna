package com.fauna.app.core

/**
 * The identity material of the account this app is serving, read through the
 * multi-account registry on every access — never a cached copy and never a
 * platform-side single slot. The android twin of windows'
 * `FaunaApp.Core/Services/ISessionAccount.cs`.
 *
 * The registry is the only store (`long-term-store.md` § Downgrade mirror +
 * abandoned-append recovery, RETIRED 2026-09-24): the account's secret and nest
 * binding are its per-actor slots, its handle/domain/tier the index entry's
 * server-data cache. Every property resolves the session account's
 * `FfiSessionMaterial` at the moment it is read, so a view that reads after a
 * sign-in, a switch or a silent-sign-in cache refresh sees the account as it
 * stands now.
 *
 * Read-only by design. Every write goes through a shared moment on the registry
 * — `confirmIdentity`, `persistLoggedIn`, `persistAwaitingDns`,
 * `persistPendingInvite`, `updateCache`, the launch persistence's
 * `saveAuthenticated` — so no app-side write path can diverge from the other six
 * apps'. The wizard-resume rows are read through the same `LaunchPersistence`
 * the launch machine routes on, never here.
 */
interface SessionAccount {
    /** The account's identity secret (64-hex), or null when no account is in session. */
    val secretHex: String?

    /** The account's home nest, or null before its `LoggedIn` terminal recorded one. */
    val nestUrl: String?

    /**
     * The sync device id the session account registers under on this install —
     * its persisted per-account id, else derived and persisted through the
     * shared get-or-create ([deviceIdFor]).
     */
    val deviceId: String?

    /**
     * The sync device id the identity [secretHex] registers under — per
     * account, persisted or derived (`FfiAccountRegistry.deviceIdForActor`).
     * The form a caller holding an identity that is not (yet) the session
     * account uses: the append wizard's new account.
     */
    fun deviceIdFor(secretHex: String?): String?

    /**
     * Server-data cache (the index entry): the account's handle, sometimes
     * `@`-qualified. Read by the launch flow's known-nest re-entries so the user
     * doesn't re-type a handle they've already used.
     */
    val handle: String?

    /**
     * Server-data cache (the index entry): the identity domain (e.g.
     * "alice.example.com"), written by the silent challenge
     * (`long-term-store.md` § Cross-app server-data cache).
     */
    val domain: String?

    /** Server-data cache (the index entry): the billing/feature tier. */
    val tier: String?
}
