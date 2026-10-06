package com.fauna.app.core

import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.fauna_launch_machine.LaunchMachine
import uniffi.fauna_launch_machine.LaunchObserver
import uniffi.fauna_launch_machine.LaunchPhase
import uniffi.fauna_launch_machine.LaunchSnapshot
import uniffi.fauna_launch_machine.TokenStatus

/**
 * Adapter from the LaunchMachine's onChanged() callback to a Kotlin
 * StateFlow the Compose tree can collect.
 *
 * The flow's initial value is the synthetic "Boot / no token / no error"
 * snapshot the machine starts in; on every onChanged() we re-read
 * machine.snapshot() and emit. Because UniFFI's tokio-async-runtime fires
 * onChanged from a worker thread, MutableStateFlow.value (atomic in
 * coroutines-core 1.7+) is safe to write from any thread.
 *
 * The machine reference is set via attach() after construction — the
 * observer must be passable to LaunchMachine.new(observer, persistence)
 * before the machine exists. lateinit guards both the typing and the
 * "called before attach" failure mode.
 */
@Singleton
class LaunchObserverImpl @Inject constructor() : LaunchObserver {

    private val _snapshot = MutableStateFlow(initialSnapshot())
    val snapshot: StateFlow<LaunchSnapshot> = _snapshot.asStateFlow()

    private lateinit var machine: LaunchMachine

    fun attach(machine: LaunchMachine) {
        this.machine = machine
        _snapshot.value = machine.snapshot()
    }

    override fun onChanged() {
        if (::machine.isInitialized) {
            _snapshot.value = machine.snapshot()
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
