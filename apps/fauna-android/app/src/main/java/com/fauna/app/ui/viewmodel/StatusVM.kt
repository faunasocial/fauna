package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ShellLog
import com.fauna.app.data.db.CachedAccount
import com.fauna.app.data.db.CachedAccountDao
import com.fauna.ffi.FfiQuotaGetReply
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class StatusVM @Inject constructor(
    private val api: ApiClient,
    private val accountDao: CachedAccountDao
) : ViewModel() {

    private val _account = MutableStateFlow<CachedAccount?>(null)
    val account: StateFlow<CachedAccount?> = _account

    private val _quota = MutableStateFlow<FfiQuotaGetReply?>(null)
    val quota: StateFlow<FfiQuotaGetReply?> = _quota

    fun refresh() {
        viewModelScope.launch {
            _account.value = accountDao.get()
            try {
                _quota.value = api.fetchQuota()
            } catch (e: Exception) {
                ShellLog.w("StatusVM", "quota fetch failed: ${e.message}")
            }
        }
    }
}
