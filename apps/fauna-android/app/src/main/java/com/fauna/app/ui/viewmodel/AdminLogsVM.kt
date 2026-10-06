package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_log.LogEntry
import javax.inject.Inject

/**
 * View-model for the admin Logs page (`admin-logs`, observability.md § Surfaces):
 * fetches the nest's in-memory `fauna-log` ring **once** over the admin-scoped
 * `fauna.admin.logs` WS-RPC (via [ApiClient.adminLogs] → `FfiAdminClient.logs`)
 * and holds it as the page source. The page filters this held list client-side
 * (no refetch — so "All" deterministically restores the full set), and renders
 * it with the SAME widget as the client's own Settings → Logs page. No clear:
 * there is no admin RPC to wipe the nest ring. Mirrors the linux reference
 * (`views/admin.rs` `build_admin_logs_page` + `update_admin_logs`).
 */
@HiltViewModel
class AdminLogsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    /** The fetched nest ring (oldest-first), the page's filter source. */
    private val _entries = MutableStateFlow<List<LogEntry>>(emptyList())
    val entries: StateFlow<List<LogEntry>> = _entries

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    fun load() {
        viewModelScope.launch {
            _error.value = null
            try {
                _entries.value = api.adminLogs()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }
}
