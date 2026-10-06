package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

data class NestStats(
    val totalUsers: Int,
    val suspendedUsers: Int,
    val totalInboxBytes: Long,
    val totalStorageBytes: Long,
    val wsConnections: Int,
    val usersByTier: Map<String, Int>
)

@HiltViewModel
class AdminDashboardVM @Inject constructor(
    private val api: ApiClient
) : ViewModel() {

    val stats = MutableStateFlow<NestStats?>(null)
    val isLoading = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)

    fun loadStats() {
        viewModelScope.launch {
            isLoading.value = true
            errorMessage.value = null
            try {
                val s = api.fetchAdminStats()
                stats.value = NestStats(
                    totalUsers = s.totalUsers.toInt(),
                    suspendedUsers = s.suspendedUsers.toInt(),
                    totalInboxBytes = s.totalInboxBytes,
                    totalStorageBytes = s.totalStorageBytes,
                    wsConnections = s.wsConnections.toInt(),
                    usersByTier = s.usersByTier.associate { it.tier to it.count.toInt() }
                )
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            isLoading.value = false
        }
    }
}
