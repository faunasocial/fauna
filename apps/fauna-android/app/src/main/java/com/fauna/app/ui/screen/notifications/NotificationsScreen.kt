package com.fauna.app.ui.screen.notifications

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.NotificationsVM
import com.fauna.ffi.FfiNotifItem
import com.fauna.ffi.FfiNotificationText
import com.fauna.ffi.notificationTextFor
import social.fauna.generated.Ids

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NotificationsScreen(
    vm: NotificationsVM = hiltViewModel(),
) {
    val notifications by vm.notifications.collectAsState()
    val unreadCount by vm.unreadCount.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val error by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val listState = rememberLazyListState()

    LaunchedEffect(Unit) { vm.loadNotifications() }

    LaunchedEffect(error) {
        appMessages.showError(error)
    }

    val shouldLoadMore by remember {
        derivedStateOf {
            val lastVisible = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            lastVisible >= listState.layoutInfo.totalItemsCount - 3 && !isLoading
        }
    }
    LaunchedEffect(shouldLoadMore) {
        if (shouldLoadMore) vm.loadMore()
    }

    Column(modifier = Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(16.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(
                text = stringResource(R.string.common_notifications),
                style = MaterialTheme.typography.titleLarge
            )

            Badge(modifier = Modifier.testTag(Ids.NOTIFICATION_COUNT_BADGE)) {
                Text("$unreadCount")
            }
        }

        OutlinedButton(
            onClick = { vm.markAllRead() },
            enabled = unreadCount > 0,
            modifier = Modifier
                .padding(horizontal = 16.dp)
                .testTag(Ids.NOTIFICATION_MARK_READ)
        ) {
            Text(stringResource(R.string.bridges_mark_all_read))
        }

        Spacer(Modifier.height(16.dp))

        Box(modifier = Modifier.fillMaxSize()) {
            when {
                isLoading && notifications.isEmpty() -> {
                    CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
                }
                notifications.isEmpty() -> {
                    Box(
                        modifier = Modifier
                            .fillMaxSize()
                            .testTag(Ids.NOTIFICATION_ITEM),
                        contentAlignment = Alignment.Center
                    ) {
                        Text(
                            text = stringResource(R.string.bridges_no_notifications),
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                else -> {
                    LazyColumn(state = listState, modifier = Modifier.fillMaxSize()) {
                        items(notifications, key = { it.id }) { notif ->
                            NotificationRow(notification = notif)
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun NotificationRow(notification: FfiNotifItem) {
    val (icon, description) = typeToIcon(notification.notifType)
    ListItem(
        leadingContent = {
            Icon(
                icon, contentDescription = description,
                modifier = Modifier.testTag(Ids.NOTIFICATION_TYPE_ICON),
                tint = if (!notification.isRead) MaterialTheme.colorScheme.primary
                else MaterialTheme.colorScheme.onSurfaceVariant
            )
        },
        headlineContent = {
            Text(
                notificationRowText(notification),
                style = MaterialTheme.typography.bodyMedium,
                color = if (!notification.isRead) MaterialTheme.colorScheme.onSurface
                else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        },
        trailingContent = {
            if (!notification.isRead) {
                Box(
                    modifier = Modifier
                        .size(8.dp)
                        .clip(CircleShape)
                        .background(MaterialTheme.colorScheme.primary)
                )
            }
        },
        // Informational only, per notifications.md § Don't do these ("Don't
        // deep-link via per-app routing tables") — tap-to-navigate needs a
        // shared-Rust destination resolver first (§ Where logic lives, not
        // built yet), same as the other 6 clients.
        modifier = Modifier.testTag(Ids.NOTIFICATION_ITEM),
    )
}

/**
 * The sentence a row paints: the shared decision (`notification_text_for` —
 * localized body, English `summary`, or the default; `behavior/notifications.md`
 * § Localized body), its localized arm resolved through this app's own string
 * resources. Never `summary` on the row's own say-so: a body key this build
 * lacks must lose to it, and only the shared decision knows which keys exist.
 */
@Composable
private fun notificationRowText(notification: FfiNotifItem): String =
    when (val text = remember(notification) { notificationTextFor(notification) }) {
        is FfiNotificationText.Localized -> localized(text.text).orEmpty()
        is FfiNotificationText.Verbatim -> text.text
    }

private fun typeToIcon(type: String): Pair<ImageVector, String> = when (type) {
    "like" -> Icons.Default.Favorite to "Like"
    "repost" -> Icons.Default.Repeat to "Repost"
    "follow" -> Icons.Default.PersonAdd to "Follow"
    "mention" -> Icons.Default.AlternateEmail to "Mention"
    "reply" -> Icons.Default.Reply to "Reply"
    "quote" -> Icons.Default.FormatQuote to "Quote"
    else -> Icons.Default.Notifications to "Notification"
}
