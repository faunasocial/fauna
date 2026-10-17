package com.fauna.app.core

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * The loop/one-pass split every app makes at its post-auth hook
 * (`critical-alerts.md` § Implementation status today, the 2026-09-24 windows
 * entry; windows' `CriticalAlertsSweep.StartForIdentity`, apple's
 * `CriticalAlertsLoopClaim`): a first or identity-changing sign-in starts the
 * session-start + periodic re-sweep LOOP (§ Mechanism → *How often*); a
 * same-identity re-establish runs ONE pass, because that identity's loop is
 * still running and starting another would stack a second sweeper per re-auth.
 *
 * Pure Kotlin over an injected [scope] so `CriticalAlertSweepDispatcherTest`
 * pins the split without the native registry [CriticalAlertsHost] binds.
 */
internal class CriticalAlertSweepDispatcher(private val scope: CoroutineScope) {
    // The (actor, nest) the live loop covers and the token of the run that owns
    // it; null when no loop is live (none started, the identity was torn down,
    // or the loop itself ended).
    private var key: String? = null
    private var token: Any? = null

    /**
     * Run [loop] on [scope] when [identityKey] has no live loop, else [onePass].
     * "Identity" is the actor AND the nest: a re-point at another nest needs a
     * loop sweeping that nest.
     */
    fun start(identityKey: String, loop: suspend () -> Unit, onePass: suspend () -> Unit) {
        val owned = claim(identityKey)
        if (owned == null) {
            scope.launch { onePass() }
            return
        }
        scope.launch {
            try {
                loop()
            } finally {
                // Ended (a failed connect, or the teardown): it no longer covers
                // its identity, so the next sign-in must start one.
                release(owned)
            }
        }
    }

    /** The identity is gone: the next sign-in starts a fresh loop. Paired with
     *  the registry's `clearAll`, whose teardown-epoch bump is what stops the
     *  old loop, so the two can never disagree. */
    @Synchronized
    fun forget() {
        key = null
        token = null
    }

    @Synchronized
    private fun claim(identityKey: String): Any? {
        if (key == identityKey) return null
        val fresh = Any()
        key = identityKey
        token = fresh
        return fresh
    }

    /** Only if [ended] is still the recorded loop: a departed identity's loop
     *  ending late must not clear its successor's record. */
    @Synchronized
    private fun release(ended: Any) {
        if (token !== ended) return
        key = null
        token = null
    }
}
