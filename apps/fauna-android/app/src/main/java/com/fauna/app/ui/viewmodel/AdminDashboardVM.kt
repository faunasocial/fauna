package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiAdminDashboardSnapshot
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The admin dashboard's read model: the shared card set
 * (`admin_dashboard_snapshot()` — the five reads and the one fold,
 * `docs/goal/behavior/admin.md` § 1. Dashboard → *The card set*). Android
 * renders its cards and chooses none of its own.
 */
@HiltViewModel
class AdminDashboardVM @Inject constructor(
    private val api: ApiClient
) : ViewModel() {

    val snapshot = MutableStateFlow<FfiAdminDashboardSnapshot?>(null)
    val isLoading = MutableStateFlow(false)

    /** The detail of the first failed read (the snapshot's own `error`), or of
     *  a call that never reached the nest; the screen shows it through
     *  `admin.dashboard.load_error` while the other reads' cards still paint. */
    val errorMessage = MutableStateFlow<String?>(null)

    fun loadStats() {
        viewModelScope.launch {
            isLoading.value = true
            try {
                val s = api.adminDashboardSnapshot()
                snapshot.value = s
                errorMessage.value = s.error
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            isLoading.value = false
        }
    }
}
