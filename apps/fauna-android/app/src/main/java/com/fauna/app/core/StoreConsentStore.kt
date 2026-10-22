package com.fauna.app.core

import android.content.Context
import android.util.Log
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import com.fauna.app.age.PrefsStoreConsentStateStore
import com.fauna.app.age.StoreConsentGlue
import com.fauna.app.age.StoreConsentRound
import com.fauna.app.age.StoreConsentStateStore
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiStoreSignal
import dagger.hilt.android.qualifiers.ApplicationContext
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * The store-consent lock state (`account-age-band.md` § The account age band →
 * the store-law developer duties bullet, item (3); ui.yaml
 * `store-consent-lock` / `store-consent-lock-message`) — the
 * [ScreenTimeStore] twin: one [lockLines] the global lock renders, so whether
 * it shows and what it says come from the same round.
 *
 * The round runs at launch and on every return to the foreground (the
 * process's `ON_RESUME`, which also fires on the first launch) — the
 * foreground re-check is how a parent's Play approval lifts the lock, since
 * android has no in-app ask. **Every decision is shared Rust**
 * ([StoreConsentRound] → `fauna_ffi::store_consent_round`); this class only
 * schedules the round and resolves its message lines.
 *
 * Device-scoped, never account-scoped: the lock is about the person the store
 * knows on this device, so no account switch or sign-out touches it.
 */
@Singleton
class StoreConsentStore @Inject constructor(
    @ApplicationContext private val context: Context,
) {
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private val stateStore: StoreConsentStateStore = PrefsStoreConsentStateStore(context)
    private val roundLock = Mutex()

    /**
     * Test-only stand-in for the Play read (testing.md convention 15): when
     * set, rounds use it in place of [StoreConsentGlue]. Written only by the
     * debug-only `TestAgent`'s `store_consent_inject` command; stays unset in
     * production.
     */
    @Volatile private var testSignal: TestSignal? = null

    /** A test-injected signal; `signal == null` stands for "Play reported nothing". */
    class TestSignal(val signal: FfiStoreSignal?)

    private val _lockLines = MutableStateFlow<List<String>?>(null)

    /** The lock's message lines, resolved, or `null` for "not locked". */
    val lockLines: StateFlow<List<String>?> = _lockLines.asStateFlow()

    init {
        scope.launch(Dispatchers.Main) {
            ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
                override fun onResume(owner: LifecycleOwner) {
                    scope.launch { refresh() }
                }
            })
        }
    }

    /** Run one round now and publish its verdict. */
    suspend fun refresh() = roundLock.withLock {
        val injected = testSignal
        val signal = if (injected != null) {
            injected.signal
        } else {
            StoreConsentRound.readSignal(StoreConsentGlue, context) { Log.w(TAG, it) }
        }
        val round = StoreConsentRound.run(signal, stateStore)
        _lockLines.value = round.message?.mapNotNull { resolveLocalized(context, it) }
    }

    /**
     * Test-only: replace the Play read with [signal] (or, with `null`, return
     * to the real read) and run one round — the agent's stand-in for a
     * foreground re-check. Driven only by the debug-only `TestAgent`.
     */
    suspend fun injectTestSignal(signal: TestSignal?) {
        testSignal = signal
        refresh()
    }

    private companion object {
        const val TAG = "StoreConsentStore"
    }
}
