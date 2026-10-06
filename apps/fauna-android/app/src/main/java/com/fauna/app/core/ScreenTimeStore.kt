package com.fauna.app.core

import android.content.Context
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiScreenTimePolicy
import com.fauna.ffi.FfiUsageHeartbeat
import com.fauna.ffi.screenLockMessage
import dagger.hilt.android.qualifiers.ApplicationContext
import java.time.LocalTime
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * The ward's screen-time lock state (`family-safety.md` § Screen time,
 * Slice E) — the android twin of linux `screen_lock.rs` / web
 * `screenTime.svelte.ts`. Holds the [FfiUsageHeartbeat] handle for the
 * session and exposes the one [lockMessage] the global `screen-time-lock`
 * overlay renders, so the gate and its wording can never drift apart.
 *
 * **Client-enforced by construction.** The nest cannot see when a child's
 * device is in use and deliberately does not gate on it, so this store IS
 * the enforcement. **Every decision is shared Rust** — this class holds no
 * policy logic at all: whether to lock, and what the lock says, are one call
 * to [screenLockMessage], which folds the policy, the device's local clock
 * and the day's cross-device total.
 */
@Singleton
class ScreenTimeStore @Inject constructor(
    @ApplicationContext private val context: Context,
    private val api: ApiClient,
    accountStores: AccountStores,
) {
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private val heartbeat = FfiUsageHeartbeat()

    private var policy: FfiScreenTimePolicy? = null
    private var guardianHandle: String? = null

    /**
     * Test-only clock skew in seconds (testing.md § convention 14's fake
     * clock), the android twin of linux `advance_test_clock` / web
     * `testClockSkewSecs`. Stays 0 in production; only the debug-only
     * `TestAgent`'s `screen_time_heartbeat` command writes it via
     * [advanceTestClockAndTick].
     */
    @Volatile private var testClockSkewSecs: Long = 0

    private val _lockMessage = MutableStateFlow<String?>(null)

    /** The current lock verdict, resolved to display text, or `null` for "not
     *  locked" — the single decision point the global overlay renders from. */
    val lockMessage: StateFlow<String?> = _lockMessage.asStateFlow()

    /**
     * The actor this store's current state was last seeded for — `null`
     * before the first seed. Lets the closer below tell a genuine switch (the
     * active pointer now names someone else) from a **same-actor drop** (the
     * pointer is unchanged — the debug reset/logout arms below), so it never
     * resurrects the outgoing actor's own snapshot under what is supposed to
     * be a blank reset. See [ContentPolicyStore.seededActorHex], the same
     * guard.
     */
    private var seededActorHex: String? = null

    init {
        // Drop this ward's screen-time state on an account switch or
        // sign-out — without this a new account would inherit the previous
        // ward's lock, naming a guardian the user does not have (linux
        // `clear_for_identity_change`).
        accountStores.registerCloser("screen-time") {
            clearForIdentityChange()
            // Re-seed for whoever is active NOW (family-safety.md § Content
            // policy, clause 2): the switch closer runs AFTER
            // `AccountStores`'s active pointer already names the incoming
            // actor, so without this an offline switch leaves the incoming
            // ward's bedtime lock/budget unenforced until a read succeeds.
            // Guarded on the actor actually having changed — a same-actor
            // drop (the debug `TestAgent` `reset`/`logout` arms under
            // `FileSecretBackend` e2e mode, where `SecureStorage.clear()`
            // does not reach the registry's own backing file) must land
            // blank, not the previous ward's own last-known window.
            val newActor = accountStores.activeActorHex()
            if (newActor != null && newActor != seededActorHex) {
                accountStores.supervisionSnapshot()?.let { snap ->
                    setWardScreenTime(snap.screenTime, snap.supervisedBy.handle, null)
                }
            }
            seededActorHex = newActor
        }
        // Seed policy + guardian from the persisted last-known supervision
        // snapshot BEFORE the first read lands (family-safety.md § Content
        // policy, clause 2): the window half is pure local clock, so without
        // this restore a supervised ward's bedtime lock is exactly one
        // airplane-mode toggle from being bypassed at every cold launch. The
        // day's usage total is deliberately NOT persisted — it is cross-device
        // state that re-arrives with the first successful read (the ratified
        // fail-open on the budget arm). A later successful read supersedes
        // this via the same setWardScreenTime path.
        accountStores.supervisionSnapshot()?.let { snap ->
            setWardScreenTime(snap.screenTime, snap.supervisedBy.handle, null)
        }
        seededActorHex = accountStores.activeActorHex()
        scope.launch {
            while (true) {
                delay(TICK_INTERVAL_MS)
                tick()
            }
        }
    }

    private fun nowSecs(): Long = System.currentTimeMillis() / 1000 + testClockSkewSecs

    /** The device's local clock as minutes from local midnight — the unit
     *  [FfiScreenTimePolicy] stores its window bounds in. The window half is
     *  pure local clock, so this needs no wire traffic at all. */
    private fun localMinutesFromMidnight(): Int {
        val now = LocalTime.now()
        return now.hour * 60 + now.minute
    }

    /**
     * Record the ward's own screen-time policy, guardian, and the day's
     * cross-device usage total, from `fauna.family.status`. Called on every
     * status read — [com.fauna.app.ui.viewmodel.FamilyVM]'s own read and the
     * global supervised-indicator poll — so a guardian's policy edit takes
     * effect on the ward's next read rather than needing a restart. `null`
     * [guardianHandle] (unsupervised) clears the lock.
     *
     * [usageTodayMinutes] seeds the heartbeat so the very first paint can
     * already evaluate the budget, instead of leaving an over-budget ward
     * unlocked until the first heartbeat round-trip completes.
     */
    fun setWardScreenTime(policy: FfiScreenTimePolicy?, guardianHandle: String?, usageTodayMinutes: UInt?) {
        this.policy = policy
        this.guardianHandle = guardianHandle
        heartbeat.setPolicy(policy)
        heartbeat.seedTotal(usageTodayMinutes)
        recompute()
    }

    /** The ward's own usage figure for their read-only summary — the same
     *  number the guardian sees (§ Screen time transparency rule). `null`
     *  when no total has been heard yet. */
    fun usedTodayMinutes(): UInt? = heartbeat.usedTodayMinutes(nowSecs())

    private fun clearForIdentityChange() {
        policy = null
        guardianHandle = null
        heartbeat.reset()
        recompute()
    }

    /** Re-evaluate the lock verdict and publish it. The single decision
     *  point: both the overlay's visibility and its text come from here. */
    private fun recompute() {
        val guardian = guardianHandle
        val text = if (guardian == null) {
            null
        } else {
            screenLockMessage(
                policy,
                localMinutesFromMidnight().toUShort(),
                heartbeat.usedTodayMinutes(nowSecs()),
                guardian,
            )
        }
        _lockMessage.value = resolveLocalized(context, text)
    }

    /**
     * Drive the heartbeat one step and, if the shared engine says a report is
     * due, send `fauna.family.usage_report` and land the reply. `active` is
     * whether the app is being used right now: foregrounded AND the lock is
     * not showing — lock-screen time is not use, crediting it would inflate
     * the guardian's readout with minutes the child never spent. A failure
     * re-credits the minutes rather than forgiving them (the delta is
     * defined against the last *successful* report).
     */
    private suspend fun heartbeatStep(active: Boolean) {
        val now = nowSecs()
        heartbeat.setActive(active, now)
        val minutes = heartbeat.takeDue(now) ?: return
        val offset = DeviceOffset.utcOffsetMinutes()
        try {
            val reply = api.familyUsageReport(minutes, offset)
            heartbeat.reportSucceeded(reply.day, reply.dayTotalMinutes, nowSecs())
        } catch (_: Exception) {
            heartbeat.reportFailed()
        }
        recompute()
    }

    /** The one-minute tick (also the shared engine's own accrual-step
     *  requirement of a caller — a slower tick would silently under-count). */
    private suspend fun tick() {
        if (!heartbeat.isAccounting()) return
        val foregrounded = ProcessLifecycleOwner.get().lifecycle.currentState
            .isAtLeast(Lifecycle.State.STARTED)
        heartbeatStep(foregrounded && lockMessage.value == null)
    }

    /**
     * Test-only: advance the clock by [minutes] of foreground use, in the
     * accrual steps a real caller would tick in, then run one production
     * heartbeat step — the android twin of linux `advance_test_clock` +
     * `flush_usage_report(true)` / web `advanceTestClock` +
     * `tickUsageHeartbeat`. `active = true` throughout: the poke asserts what
     * a ward actively using the app accrues. Driven only by the debug-only
     * `TestAgent`'s `screen_time_heartbeat` command (testing.md § convention
     * 14's fake clock + `run_now` poke; § convention 15 keeps this whole path
     * out of release artifacts via [TestAgent]'s own build-type gate).
     */
    suspend fun advanceTestClockAndTick(minutes: Int) {
        val step = 120L // fauna_core::screen_time::MAX_ACCRUAL_STEP_SECS
        var remaining = (minutes * 60).toLong().coerceAtLeast(0)
        // Prime the engine's reference point before advancing — it accrues
        // from the GAP between calls, so with no prior call the first step
        // would credit nothing and the poke would silently deliver less use
        // than it was asked for.
        heartbeat.setActive(true, nowSecs())
        while (remaining > 0) {
            val bump = minOf(remaining, step)
            testClockSkewSecs += bump
            remaining -= bump
            heartbeat.setActive(true, nowSecs())
        }
        heartbeatStep(true)
    }

    companion object {
        private const val TICK_INTERVAL_MS = 60_000L
    }
}
