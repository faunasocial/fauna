package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.app.core.SecureStorage
import com.fauna.app.ui.util.answerRefusal
import com.fauna.ffi.FfiNotifItem
import com.fauna.ffi.actorIdFromSecret
import com.fauna.ffi.notificationsClearAllPossibility
import com.fauna.ffi.notificationsMarkAllReadPossibility
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject
import uniffi.fauna_core.MutedKeyword
import uniffi.fauna_core.Possibility
import uniffi.fauna_core.ReadState

@HiltViewModel
class NotificationsVM @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    @ApplicationContext private val context: Context,
) : ViewModel() {

    val notifications = MutableStateFlow<List<FfiNotifItem>>(emptyList())
    val unreadCount = MutableStateFlow(0)
    val isLoading = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)
    private var cursor: Long? = null

    /**
     * Whether the page's list read has answered at least once (`ui/README.md`
     * § List pages: loading is not empty) — what *Mark all read* and *Clear
     * all* wait on (`notifications.md` § Errors & edge cases, *The page-wide
     * actions wait on the first read*; tui's `NotificationsState::read`).
     * `FAILED` after a first read that threw, until a later one succeeds; a
     * failure after a resolved read keeps the rows shown actionable.
     */
    private var read = ReadState.PENDING

    private val _markAllReadAnswer = MutableStateFlow(notificationsMarkAllReadPossibility(read, 0u))

    /** `notification-mark-read`'s answer — the shared
     *  `mark_all_read_possibility` over [read] and [unreadCount]
     *  (`notifications.nothing_unread` at zero); [markAllRead] asks it again. */
    val markAllReadAnswer: StateFlow<Possibility> = _markAllReadAnswer

    private val _clearAllAnswer = MutableStateFlow(notificationsClearAllPossibility(read, 0u))

    /** `notification-clear`'s answer — the shared `clear_all_possibility` over
     *  [read] and the rows the page holds (`notifications.nothing_to_clear`
     *  on an empty page); [clearAll] asks it again. */
    val clearAllAnswer: StateFlow<Possibility> = _clearAllAnswer

    /** Re-ask both page-wide answers after the read state, the rows or the
     *  unread count moved. */
    private fun repaintAnswers() {
        _markAllReadAnswer.value = notificationsMarkAllReadPossibility(read, unreadCount.value.coerceAtLeast(0).toUInt())
        _clearAllAnswer.value = notificationsClearAllPossibility(read, notifications.value.size.toUInt())
    }

    /** The page's list read landed ([rows]) or threw ([rows] `null`) — folds
     *  the read state and the rows, then re-asks the answers. Internal so the
     *  VM tests drive it without a nest. */
    internal fun applyRead(rows: List<FfiNotifItem>?) {
        if (rows != null) {
            notifications.value = rows
            read = ReadState.RESOLVED
        } else if (read != ReadState.RESOLVED) {
            read = ReadState.FAILED
        }
        repaintAnswers()
    }

    /** Rule 6: the refusal an action's own answer gives, on `error-message`;
     *  `true` when it refused (nothing is sent). */
    private fun refuses(answer: Possibility): Boolean {
        val refusal = answerRefusal(context, answer) ?: return false
        errorMessage.value = refusal.reason
        return true
    }

    /** The process-held muted-word list ([ApiClient.mutedWords]) the rows'
     *  collapse matches against (notifications.md § Muted words). */
    val mutedKeywords: StateFlow<List<MutedKeyword>> get() = api.mutedWords.words

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
                applyRead(response.notifications)
                cursor = response.cursor
                unreadCount.value = api.getUnreadNotificationCount(actor)
                repaintAnswers()
            } catch (e: Exception) {
                applyRead(null)
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
                repaintAnswers()
                cursor = response.cursor
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /**
     * `notification-dismiss` — delete one row, then refetch (the reply is a bare
     * echo; the fresh list is the observable effect). A `retained` refusal at the
     * security window's edge lands on the shared error banner.
     */
    fun dismiss(id: Long) {
        viewModelScope.launch {
            try {
                api.dismissNotification(id)
                loadNotifications()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /**
     * `notification-clear` — delete every row up to the newest one loaded (so a
     * row that arrived after the user looked survives), then refetch. Retained
     * security notices stay listed.
     */
    fun clearAll() {
        if (refuses(notificationsClearAllPossibility(read, notifications.value.size.toUInt()))) return
        val upTo = notifications.value.maxOfOrNull { it.createdAt } ?: return
        viewModelScope.launch {
            try {
                api.clearNotifications(upTo)
                loadNotifications()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    fun markAllRead() {
        if (refuses(notificationsMarkAllReadPossibility(read, unreadCount.value.coerceAtLeast(0).toUInt()))) return
        val actor = actorId() ?: return
        viewModelScope.launch {
            try {
                api.markNotificationsRead(actor)
                notifications.value = notifications.value.map { it.copy(isRead = true) }
                unreadCount.value = 0
                repaintAnswers()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }
}
