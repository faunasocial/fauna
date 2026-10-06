package com.fauna.app.core.atproto

import com.fauna.app.core.ApiClient
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.fauna_atproto_settings_machine.AtprotoSettingsMachine
import uniffi.fauna_atproto_settings_machine.AtprotoSettingsObserver
import com.fauna.ffi.atprotoSettingsPrefetchSnapshot
import uniffi.fauna_atproto_settings_machine.AtprotoSettingsSnapshot
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Process-wide owner of the observer→snapshot bridge for the shared `bluesky`
 * page machine ([AtprotoSettingsMachine]). Mirrors
 * [com.fauna.app.core.feed.FeedManagerHost] exactly, for the same reason: the
 * default `hiltViewModel()` scoping [AtprotoVM][com.fauna.app.ui.viewmodel.AtprotoVM]
 * uses is scoped to the `settings/atproto` back-stack entry, so a machine
 * built inside the VM (the pre-fix shape) is rebuilt every time the user
 * leaves and re-enters the page.
 *
 * That rebuild is not just hygiene — it silently disables the S4-C
 * genesis-seniority custody alarm (`docs/goal/behavior/critical-alerts.md`
 * feeder #1). The check debounces on `custody_suspect`, a field living INSIDE
 * the machine instance (`fauna-atproto-settings-machine`'s `machine.rs`): it
 * alarms only on the SECOND consecutive status convergence that sees the same
 * contradiction (a sibling device's fresh re-mint key can look like a
 * mismatch for one pass before the `fauna.state.atproto-identity` plane syncs it; a real compromise
 * survives into the next convergence). A fresh machine per page visit resets
 * that debounce every time, so two consecutive checks can never land on the
 * same instance and the alarm can never confirm — web hit this exact bug
 * before its own singleton fix
 * (`apps/fauna-web/src/lib/wasm-atproto-settings.ts::getAtprotoSettingsMachine`).
 *
 * The machine itself is a connection-bound singleton owned by [ApiClient]
 * (built lazily over the post-auth WS-RPC socket with ONE observer — this
 * host's — and torn down on sign-out); this host owns only the observer +
 * the [snapshot] `StateFlow` every `settings/atproto` mount renders off,
 * so a later mount's gestures notify the SAME observer no earlier mount could
 * go stale on (unlike a naive per-mount observer, where only the first
 * mount's observer is ever wired into the machine).
 */
@Singleton
class AtprotoSettingsHost @Inject constructor(
    private val api: ApiClient,
) {
    private val _snapshot = MutableStateFlow(EMPTY_SNAPSHOT)

    /** Compose screens `collectAsState()` this; every machine notification
     *  (refresh, and every gesture's own internal refresh) republishes it. */
    val snapshot: StateFlow<AtprotoSettingsSnapshot> = _snapshot.asStateFlow()

    /** The machine whose `snapshot()` [observer] republishes — held so the
     *  arbitrary-Rust-thread `onChanged` callback reads the *current* machine
     *  even across a sign-out→sign-in rebuild. */
    private var seen: AtprotoSettingsMachine? = null

    private val observer = object : AtprotoSettingsObserver {
        override fun onChanged() {
            _snapshot.value = seen?.snapshot() ?: EMPTY_SNAPSHOT
        }
    }

    /**
     * The shared machine, or null until the nest is connected / no secret yet
     * (the caller renders the empty-snapshot page and retries on the next
     * gesture). Reseeds [snapshot] when [ApiClient] hands back a freshly-built
     * machine (the sign-out→sign-in case).
     */
    fun machine(): AtprotoSettingsMachine? {
        val m = api.buildAtprotoSettingsMachine(observer) ?: return null
        if (m !== seen) {
            seen = m
            _snapshot.value = m.snapshot()
        }
        return m
    }

    /** Force a re-pull of the current machine's snapshot (after a gesture the
     *  observer may not have fired for yet, or to seed the page on mount). */
    fun refreshSnapshot() {
        seen?.let { _snapshot.value = it.snapshot() }
    }

    companion object {
        /**
         * The pre-fetch page state — what the page renders after mount and
         * before [machine] resolves. Reached from the SHARED Rust default
         * across the UniFFI seam
         * (`fauna_atproto_settings_machine::AtprotoSettingsSnapshot::default()`).
         *
         * This was a hand-rolled Kotlin record literal until 2026-08-14, and
         * it drifted from the Rust default TWICE — first `hostedGateReason`
         * (leaving the two hosted rungs greyed with no reason, which
         * `ui/README.md` § Copy comprehensibility rule 5 forbids), then
         * `showDidMethodRadio`. Both were invisible to every test, because a
         * hand-rolled stand-in is only ever checked against itself. UniFFI
         * generates no constructor for a Record's `Default`, which is why the
         * seam is an explicit free function; do not reintroduce a literal.
         */
        private val EMPTY_SNAPSHOT: AtprotoSettingsSnapshot
            get() = atprotoSettingsPrefetchSnapshot()
    }
}
