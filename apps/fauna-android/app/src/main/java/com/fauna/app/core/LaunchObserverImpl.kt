package com.fauna.app.core

import java.lang.ref.WeakReference
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.fauna_launch_machine.LaunchMachine
import uniffi.fauna_launch_machine.LaunchObserver
import uniffi.fauna_launch_machine.LaunchPersistence
import uniffi.fauna_launch_machine.LaunchPhase
import uniffi.fauna_launch_machine.LaunchSnapshot
import uniffi.fauna_launch_machine.TokenStatus

/**
 * Adapter from each launch machine's onChanged() callback to the ONE Kotlin
 * StateFlow the Compose tree collects.
 *
 * Launch machines are per-launch, as on tui, linux and apple: every cold start
 * and re-entry runs on a fresh machine ([newMachine]), and a machine's
 * snapshots reach the flow only while [LaunchCurrency] says it is the machine
 * on screen — so a superseded launch's verdict never paints (`onboarding.md`
 * § App-launch routing → *A verdict renders only while its own launch is still
 * the current one*). On every forwarded onChanged() we re-read
 * machine.snapshot() and emit. Because UniFFI's tokio-async-runtime fires
 * onChanged from a worker thread, MutableStateFlow.value (atomic in
 * coroutines-core 1.7+) is safe to write from any thread.
 *
 * The flow's initial value is the synthetic "Boot / no token / no error"
 * snapshot a machine starts in.
 */
@Singleton
class LaunchObserverImpl @Inject constructor(
    private val currency: LaunchCurrency,
) {

    private val _snapshot = MutableStateFlow(initialSnapshot())
    val snapshot: StateFlow<LaunchSnapshot> = _snapshot.asStateFlow()

    /**
     * A fresh machine over [persistence], observed by its own relay. The relay
     * holds its machine WEAKLY: the machine holds the relay, and a strong
     * reference back across the FFI boundary would keep every superseded
     * launch's machine alive for the life of the process.
     */
    fun newMachine(persistence: LaunchPersistence): LaunchMachine {
        val relay = Relay()
        val machine = LaunchMachine(relay, persistence)
        relay.machine = WeakReference(machine)
        return machine
    }

    /** Paint [machine]'s current snapshot — the moment it takes the surface. */
    fun show(machine: LaunchMachine) {
        if (currency.isShown(machine)) _snapshot.value = machine.snapshot()
    }

    private inner class Relay : LaunchObserver {
        @Volatile var machine: WeakReference<LaunchMachine>? = null

        override fun onChanged() {
            val machine = machine?.get() ?: return
            if (currency.isShown(machine)) _snapshot.value = machine.snapshot()
        }
    }

    companion object {
        private fun initialSnapshot(): LaunchSnapshot = LaunchSnapshot(
            phase = LaunchPhase.Boot,
            token = TokenStatus.None,
            lastError = null,
            // Set only when the launch flow is refused with
            // `fauna.auth.superseded` — the identity was succeeded and the
            // account belongs to that actor id now. Never at Boot.
            supersededSuccessor = null,
            // The nest-confirmed <handle>@<domain>@tier, absent until a silent
            // challenge resolves one. Never at Boot.
            identity = null,
            // Set only when a silent challenge is refused because the nest's
            // identity FORKED (a rotation this device never saw). Never at Boot
            // — `false` in every other case, per the field's own contract.
            identityFork = false,
            // Absent until the launch machine's first persistence read refuses
            // the account index (newer than this build, or malformed). Never
            // at Boot.
            accountIndexRefusal = null,
        )
    }
}
