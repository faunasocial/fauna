package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiRestoreDivergenceRow
import com.fauna.ffi.FfiRestoreHistoryRow
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Drives the read-only restore surface on the Backups page
 * (`docs/goal/ui/backups.md` §§ Restore history / Restore divergence) over the
 * shared `fauna-client-snapshots` crate via [ApiClient]'s `FfiSnapshotsClient`
 * seam (priority #2 — no Kotlin reimplementation of the kind-composition).
 *
 * `refresh()` loads the bearer's `restore_history` rows, then fetches the
 * forensic divergence rows for each (one `list_restore_divergence` per row, like
 * the linux lead `apps/fauna-linux/src/views/backups/restore.rs`). A per-row
 * divergence fetch failure degrades to "no banner" rather than failing the whole
 * load. The FFI calls live here so the Compose Content stays Robolectric-safe.
 */
@HiltViewModel
class RestoreHistoryVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val _history = MutableStateFlow<List<FfiRestoreHistoryRow>>(emptyList())
    val history: StateFlow<List<FfiRestoreHistoryRow>> = _history.asStateFlow()

    /** Forensic divergence rows keyed by `snapshot_id`; a key is absent until its
     *  per-row fetch lands, and empty when the snapshot diverged on nothing. */
    private val _divergence = MutableStateFlow<Map<Long, List<FfiRestoreDivergenceRow>>>(emptyMap())
    val divergence: StateFlow<Map<Long, List<FfiRestoreDivergenceRow>>> = _divergence.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    fun refresh() {
        viewModelScope.launch {
            try {
                val rows = api.listRestoreHistory()
                _history.value = rows
                // Fetch divergence per row; a single owner-scoped read failure
                // (e.g. a transient nest hiccup) shouldn't blank the history.
                val map = mutableMapOf<Long, List<FfiRestoreDivergenceRow>>()
                for (row in rows) {
                    map[row.snapshotId] = runCatching {
                        api.listRestoreDivergence(row.snapshotId)
                    }.getOrDefault(emptyList())
                }
                _divergence.value = map
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }
}
