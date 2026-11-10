package com.fauna.app.core

import java.util.concurrent.atomic.AtomicLong
import javax.inject.Inject
import javax.inject.Singleton
import uniffi.fauna_launch_machine.LaunchMachine

/**
 * Which launch is the current one — android's twin of tui's `launch::deliver` /
 * `launch::retire_in_flight`, linux's `launch_currency` and apple's
 * `LaunchMachineBox` (`onboarding.md` § App-launch routing → *A verdict renders
 * only while its own launch is still the current one*; multiple-accounts
 * outcome 17).
 *
 * A launch verdict is decided from a store read taken when the launch started and
 * rendered a round trip later. Every asynchronous launch drive
 * ([com.fauna.app.ui.viewmodel.AppLaunchVM]'s cold start and its re-runs on a
 * fresh machine — the account switcher's re-entry, the teardown re-entry, the
 * succession adopt — and Retry / Trust on the machine already on screen)
 * [begin]s here before it awaits, and asks [deliver] when it returns. A newer
 * launch replaces the held one; the e2e session patch, which builds a session
 * without a launch, clears it ([retireInFlight]).
 *
 * Android renders the launch surface continuously off the machine's observer
 * rather than once per delivered verdict, so the drop has two halves keyed on
 * the same ownership: [LaunchObserverImpl] forwards a machine's snapshots only
 * while that machine [isShown], so a superseded machine's verdict never reaches
 * the screen, and [deliver] accounts for every verdict — `routed` or
 * `superseded` — for the `launch_verdicts` barrier.
 *
 * It is an OWNERSHIP test (identity of the drive and its machine), never an "is a
 * session present?" one: the held launch's verdict still routes in full over a
 * live session — the `IdentityChanged` teardown depends on it.
 */
@Singleton
class LaunchCurrency @Inject constructor() {

    /** One asynchronous drive of [machine] — the token [deliver] is asked about. */
    class Launch internal constructor(val machine: LaunchMachine)

    /** The machine the launch surface reads, kept after its verdict settles so a
     *  later Retry / Trust and the machine's own refreshes keep painting. */
    private var shown: LaunchMachine? = null

    /** The drive whose verdict the app is waiting for. */
    private var inFlight: Launch? = null

    private val routed = AtomicLong(0)
    private val superseded = AtomicLong(0)

    /** [machine] is now the launch in flight and the one on screen: any earlier
     *  drive still waiting is superseded, and its verdict will be dropped. */
    @Synchronized
    fun begin(machine: LaunchMachine): Launch {
        shown = machine
        return Launch(machine).also { inFlight = it }
    }

    /** Does [machine]'s snapshot belong on the launch surface? */
    @Synchronized
    fun isShown(machine: LaunchMachine): Boolean = shown === machine

    /** The machine on screen, or `null` when no launch holds the surface. */
    @Synchronized
    fun shownMachine(): LaunchMachine? = shown

    /**
     * May [launch]'s verdict stand? `true` only while it is still the drive in
     * flight — which it then stops being (the verdict settles it). Counts the
     * verdict either way.
     */
    fun deliver(launch: Launch): Boolean {
        val current = synchronized(this) {
            (inFlight === launch && shown === launch.machine).also { if (it) inFlight = null }
        }
        if (current) {
            routed.incrementAndGet()
        } else {
            superseded.incrementAndGet()
            ShellLog.i("LaunchCurrency", "[launch] verdict superseded before it rendered — dropping it")
        }
        return current
    }

    /**
     * Retire a launch still in flight because a newer session has just been built
     * without it (the e2e session patch): its machine leaves the surface, so
     * nothing it decides paints. A settled launch holds nothing in flight, so this
     * is a no-op for it. Returns whether a launch was retired.
     */
    @Synchronized
    fun retireInFlight(): Boolean {
        if (inFlight == null) return false
        inFlight = null
        shown = null
        ShellLog.i("LaunchCurrency", "[launch] a newer session retired the launch still in flight")
        return true
    }

    /** `fauna_e2e_agent::LAUNCH_VERDICTS_KEY`'s two counters. */
    val routedCount: Long get() = routed.get()
    val supersededCount: Long get() = superseded.get()
}
