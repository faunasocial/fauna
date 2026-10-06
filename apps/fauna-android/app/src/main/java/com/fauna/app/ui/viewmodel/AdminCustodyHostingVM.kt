package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiAdminHostingRow
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * View-model for the admin Held-Custody page (`admin-custody-hosting`,
 * account-data-plane.md § Two-sided bounds): the nest-wide custody-hosting registry, fetched
 * once over `fauna.admin.custody_hosting.list` ([ApiClient.adminHostingList] →
 * `FfiAdminClient.custodyHostingList`, already folded by the shared
 * `admin_hosting_rows` projection so every lift app renders the same rows in
 * the same order) and re-fetched after a remove. Mirrors the linux reference
 * (`apps/fauna-linux/src/client.rs`'s `fetch_custody_hosting`/
 * `remove_custody_hosting`) and tui's lead (`apps/fauna-tui/src/admin/mod.rs`'s
 * `load_custody_hosting_snapshot`).
 *
 * `rows == null` is the pre-hydrate state — distinct from an answered empty
 * list. "Nobody has asked this nest to hold anything" and "the read has not
 * answered yet" are different facts, and the page must never render the
 * reassuring one for the unknown one.
 */
@HiltViewModel
class AdminCustodyHostingVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val _rows = MutableStateFlow<List<FfiAdminHostingRow>?>(null)
    val rows: StateFlow<List<FfiAdminHostingRow>?> = _rows

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    /** The remove's own verdict (Removed / Removed-and-store-freed /
     *  already-gone) — chrome text, not an error (`removed = false` is an
     *  honest no-op). No ui.yaml id of its own, mirroring tui's
     *  `Element::chrome(status)`. */
    private val _status = MutableStateFlow<String?>(null)
    val status: StateFlow<String?> = _status

    private val _working = MutableStateFlow(false)
    val working: StateFlow<Boolean> = _working

    fun load() {
        viewModelScope.launch {
            _error.value = null
            try {
                _rows.value = api.adminHostingList()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    /** Drop one row, keyed by the `(host, grant)` pair a [rows] entry carries
     *  (never a painted index — a re-read can reorder rows), then re-fetch. */
    fun remove(hostActorId: String, grantId: ByteArray, removedText: String, removedWithStoreText: String, missingText: String) {
        viewModelScope.launch {
            _working.value = true
            _status.value = null
            try {
                val reply = api.adminHostingRemove(hostActorId, grantId)
                _status.value = when {
                    !reply.removed -> missingText
                    reply.storeDropped -> removedWithStoreText
                    else -> removedText
                }
                _rows.value = api.adminHostingList()
            } catch (e: Exception) {
                _error.value = e.message
            } finally {
                _working.value = false
            }
        }
    }
}
