package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiNotifItem
import com.fauna.ffi.actorIdFromSecret
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class NotificationsVM @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
) : ViewModel() {

    val notifications = MutableStateFlow<List<FfiNotifItem>>(emptyList())
    val unreadCount = MutableStateFlow(0)
    val isLoading = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)
    private var cursor: Long? = null

    private fun actorId(): String? =
        secureStorage.secretHex?.let {
            try { HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes(it))) }
            catch (_: Exception) { null }
        }

    init {
        // Re-fetch notifications on each WS reconnect (transport.md § Push
        // events), part of the linux WsEvent::Reconnected re-fetch set.
        viewModelScope.launch {
            api.reconnectTick.collect { loadNotifications() }
        }
        // Re-fetch on each inbound `fauna.notification` push, so a MOUNTED page
        // grows the row with no navigation. This surface has no poll backstop on
        // any client, so the push is the ONLY thing that can satisfy it — the
        // reconnect arm above only covers pushes dropped across a socket gap
        // (a push is never replayed). Mirrors linux's central dispatch
        // (`app.rs`: PushEvent::Notification → fetch_notifications) and the web
        // page's `onPushEvent` arm.
        viewModelScope.launch {
            api.notificationTick.collect { loadNotifications() }
        }
    }

    fun loadNotifications() {
        val actor = actorId() ?: return
        viewModelScope.launch {
            isLoading.value = true
            errorMessage.value = null
            try {
                val response = api.getNotifications(actor)
                notifications.value = response.notifications
                cursor = response.cursor
                unreadCount.value = api.getUnreadNotificationCount(actor)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            isLoading.value = false
        }
    }

    fun loadMore() {
        val actor = actorId() ?: return
        val cur = cursor ?: return
        viewModelScope.launch {
            try {
                val response = api.getNotifications(actor, cursor = cur)
                notifications.value = notifications.value + response.notifications
                cursor = response.cursor
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    fun markAllRead() {
        val actor = actorId() ?: return
        viewModelScope.launch {
            try {
                api.markNotificationsRead(actor)
                notifications.value = notifications.value.map { it.copy(isRead = true) }
                unreadCount.value = 0
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }
}
