package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogMachine
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogObserver
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogSnapshot
import javax.inject.Inject

/**
 * View-model for the shared page-level [LabelerCatalogMachine] (built over the
 * session's WS-RPC connection via [ApiClient.buildLabelerCatalogMachine]) —
 * backs BOTH the **Personalization** home's subscribed-labelers facet and the
 * **Community labelers** catalog page (`content-moderation-and-ranking.md` §
 * Tier-3 community models). Mirrors [MediaVM]'s observer→state pattern.
 *
 * Each Settings sub-page mounts its own [LabelerCatalogVM] instance
 * (`hiltViewModel()` scopes per `NavBackStackEntry`, not per machine kind —
 * mirrors [DevicesVM] backing both `settings/devices` and `settings/folders`
 * as two independent instances), each with its own machine bound to the same
 * live connection; `refresh()` re-fires on every re-entry to the destination
 * (`LaunchedEffect(Unit)`), so a subscribe on one page is visible on the other
 * once navigated back to. **No page logic client-side**: every read is a
 * snapshot getter and every gesture forwards to the machine (priority #2).
 */
@HiltViewModel
class LabelerCatalogVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val _snapshot = MutableStateFlow<LabelerCatalogSnapshot?>(null)
    /** The whole renderable labeler-catalog page; null until built + first refresh. */
    val snapshot: StateFlow<LabelerCatalogSnapshot?> = _snapshot

    private val observer = object : LabelerCatalogObserver {
        override fun onChanged() {
            _snapshot.value = machine?.snapshot()
        }
    }

    private var machine: LabelerCatalogMachine? = null

    /** Build the machine over the current connection if needed, then refresh. */
    fun start() {
        refresh()
    }

    fun refresh() {
        val m = ensureMachine() ?: return
        viewModelScope.launch { m.refresh() }
    }

    /** Open the inspect-before-subscribe panel for the catalog row at [index]. */
    fun inspect(index: Int) {
        val m = machine ?: return
        viewModelScope.launch { m.inspect(index.toUInt()) }
    }

    /** Close the inspect panel (`labeler-inspect-close-button`). */
    fun closeInspect() {
        machine?.closeInspect()
    }

    /** Subscribe to the catalog row at [index], then refresh. */
    fun subscribe(index: Int) {
        val m = machine ?: return
        viewModelScope.launch { m.subscribe(index.toUInt()) }
    }

    /** Unsubscribe from the catalog row at [index], then refresh. */
    fun unsubscribe(index: Int) {
        val m = machine ?: return
        viewModelScope.launch { m.unsubscribe(index.toUInt()) }
    }

    private fun ensureMachine(): LabelerCatalogMachine? {
        machine?.let { return it }
        val built = api.buildLabelerCatalogMachine(observer) ?: return null
        machine = built
        return built
    }
}
