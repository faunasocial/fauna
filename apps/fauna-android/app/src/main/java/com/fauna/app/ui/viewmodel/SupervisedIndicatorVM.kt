package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ScreenTimeStore
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Drives the global, permanent, non-dismissable **`supervised-indicator`**
 * chrome (family-safety.md § App surface — rendered only when
 * `fauna.family.status.supervised_by` is set). Activity-scoped like
 * [ConnectionStatusVM] (mounted outside any nav route's composable, alongside
 * `ConnectionStatusBar`/`MessageBanner`), so it must persist across
 * navigation rather than re-fetch per screen visit.
 *
 * One `familyStatus()` read at construction, re-run on every WS reconnect
 * (`api.reconnectTick`) — the same "re-resolve as the socket comes up" idiom
 * the web `+layout.svelte` gate uses. Seeded at construction from the
 * persisted last-known supervision snapshot (family-safety.md § Content
 * policy, clause 2 — supervision is never silent, a cold offline launch
 * included), and a FAILED refresh keeps the last-known value (clause 1:
 * "read failed" and "read says unsupervised" are different facts, and this
 * refresh fires exactly when a read is least likely to succeed — boot, WS
 * reconnect). Only a successful read — which may report unsupervised and
 * clear it — or the identity closer moves it, mirroring the web layout gate.
 *
 * The same read also feeds [ScreenTimeStore] (family-safety.md § Screen
 * time, Slice E) — this is the global, activity-scoped chrome poll, so it is
 * the closest android analogue of web's root-layout `$effect` that keeps the
 * ward's `screen-time-lock` inputs fresh independent of which page is
 * showing. [com.fauna.app.ui.viewmodel.FamilyVM]'s own read is the second,
 * page-scoped source of the same call.
 */
@HiltViewModel
class SupervisedIndicatorVM @Inject constructor(
    private val api: ApiClient,
    private val screenTimeStore: ScreenTimeStore,
    accountStores: AccountStores,
) : ViewModel() {

    private val _supervisedByHandle = MutableStateFlow<String?>(null)
    val supervisedByHandle: StateFlow<String?> = _supervisedByHandle.asStateFlow()

    /**
     * The actor this VM's current state was last seeded for — `null` before
     * the first seed. Same guard as [com.fauna.app.core.ContentPolicyStore
     * .seededActorHex]: tells a genuine switch (the active pointer now names
     * someone else) from a same-actor drop, so the closer below never
     * resurrects the outgoing actor's own guardian handle under what is
     * supposed to be a blank reset.
     */
    private var seededActorHex: String? = null

    init {
        // Keep-on-failure makes the identity drop load-bearing (the same trap
        // ContentPolicyStore documents): without it, a failed re-read after an
        // account switch would keep naming the PREVIOUS ward's guardian.
        accountStores.registerCloser("supervised-indicator") {
            _supervisedByHandle.value = null
            // Re-seed for whoever is active NOW (family-safety.md § Content
            // policy, clause 2) — without this an offline switch leaves
            // `supervised-indicator` blank for the incoming ward until a read
            // succeeds. Guarded on the actor actually having changed, same
            // reason as ContentPolicyStore's closer.
            val newActor = accountStores.activeActorHex()
            if (newActor != null && newActor != seededActorHex) {
                accountStores.supervisionSnapshot()?.let {
                    _supervisedByHandle.value = it.supervisedBy.handle
                }
            }
            seededActorHex = newActor
        }
        accountStores.supervisionSnapshot()?.let {
            _supervisedByHandle.value = it.supervisedBy.handle
        }
        seededActorHex = accountStores.activeActorHex()
        refresh()
        viewModelScope.launch {
            api.reconnectTick.collect { refresh() }
        }
    }

    private fun refresh() {
        viewModelScope.launch {
            try {
                val status = api.familyStatus()
                // Screen time moves from the reply's gated supervision fold,
                // never the raw `policy`: shared Rust gates it on
                // `supervised_by`, so a policy naming no guardian locks
                // nothing (family-client-enforcement.md § Implementation
                // status today).
                screenTimeStore.setWardScreenTime(
                    status.supervision?.screenTime,
                    status.supervision?.supervisedBy?.handle,
                    status.usageTodayMinutes,
                )
                _supervisedByHandle.value = status.supervisedBy?.handle
            } catch (_: Exception) {
                // Clause 1: no information — keep whatever the last successful
                // read (or the launch restore) established.
            }
        }
    }
}
