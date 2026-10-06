package com.fauna.app.core

import com.fauna.ffi.criticalAlertsRegistry
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_alerts.CriticalAlertRow
import uniffi.fauna_client_alerts.CriticalAlertsObserver
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Process-wide owner of the cross-page critical-alerts registry
 * (`docs/goal/behavior/critical-alerts.md`) — android's twin of linux's
 * `apps/fauna-linux/src/critical_alerts.rs` registry() `OnceLock` and tui's
 * per-`App` `Arc<CriticalAlerts>`. `criticalAlertsRegistry()` (UniFFI,
 * `libs/fauna-ffi/src/critical_alerts.rs`) hands back the SAME process-wide
 * `Arc` every call — one native process = one registry, unlike web's
 * per-wasm-chunk aggregation layer — so every FFI-exposed machine that hosts
 * a feeder (today: `AtprotoSettingsMachine`'s S4-C custody check, via
 * [com.fauna.app.core.atproto.AtprotoSettingsHost]) posts to this instance
 * with no wiring on this class's part.
 *
 * Subscribes ONE observer for the app's lifetime and republishes [active] as
 * a `StateFlow` the shell banner (`FaunaNavHost.kt`'s `CriticalAlertsBanner`)
 * renders on every authenticated page.
 */
@Singleton
class CriticalAlertsHost @Inject constructor() {
    private val registry = criticalAlertsRegistry()

    // Process-wide, same shape as `ConversationsManagerHost.draftsScope` /
    // `ApiClient.connectionScope`: the re-sweep loop (`startSweepLoop` below)
    // must outlive whatever UI scope triggered it, since it only stops itself
    // on the identity-teardown boundary (`clearAll`), never earlier.
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val _active = MutableStateFlow<List<CriticalAlertRow>>(emptyList())

    /** Compose collects this; empty ⇒ the `critical-alerts` banner is absent
     *  (the whole presence rule — ui.yaml `global:` `critical-alerts`). */
    val active: StateFlow<List<CriticalAlertRow>> = _active.asStateFlow()

    private val observer = object : CriticalAlertsObserver {
        override fun onChanged() {
            _active.value = registry.active()
        }
    }

    init {
        registry.subscribe(observer)
        _active.value = registry.active()
    }

    /**
     * Drop every active alert — the identity-teardown boundary (sign-out,
     * account switch, factory reset; `critical-alerts.md` § Mechanism →
     * *Lifetime*). Called from [ApiClient.clearAuth], which is the ONE
     * teardown path every identity change on android already routes through.
     */
    fun clearAll() {
        registry.clearAll()
    }

    /**
     * Start [sweep] (`ApiClient.runCriticalAlertSweepLoop`) on this
     * singleton's own process-wide scope, not the caller's — the loop never
     * returns under normal operation (it stops itself on the first wake after
     * [clearAll] runs), so launching it from a UI-triggered scope (a
     * `LaunchedEffect`/ViewModel scope, which dies with its own lifecycle
     * rather than at identity teardown) would kill it early. Fire-and-forget,
     * same posture as every other universal post-auth hook on this class.
     */
    fun startSweepLoop(sweep: suspend () -> Unit) {
        scope.launch { sweep() }
    }
}
