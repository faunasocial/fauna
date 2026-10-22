package com.fauna.app.ui.screen.notifications

import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.core.UrlOpener
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.NotificationsVM
import com.fauna.app.ui.viewmodel.SettingsFamilyGateVM
import com.fauna.ffi.FfiNotifItem
import com.fauna.ffi.FfiNotificationDestination
import com.fauna.ffi.FfiNotificationText
import com.fauna.ffi.notificationDestinationFor
import com.fauna.ffi.notificationMutedFor
import com.fauna.ffi.notificationRetainedFor
import com.fauna.ffi.notificationTextFor
import java.net.URLEncoder
import social.fauna.generated.Ids
import uniffi.fauna_core.MutedKeyword

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NotificationsScreen(
    navController: NavController,
    vm: NotificationsVM = hiltViewModel(),
    familyGateVm: SettingsFamilyGateVM = hiltViewModel(),
) {
    // The `family-tab` gate Settings reads: a `Family` row is a control only
    // while that entry is reachable (notifications.md § Deep-link destinations).
    val hasFamilyRelationship by familyGateVm.hasFamilyRelationship.collectAsState()
    val context = LocalContext.current
    val notifications by vm.notifications.collectAsState()
    val unreadCount by vm.unreadCount.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val error by vm.errorMessage.collectAsState()
    val mutedKeywords by vm.mutedKeywords.collectAsState()
    // Session-local reveal of muted-collapsed rows (notifications.md § Muted
    // words rule 3) — the mute itself stays. The conversation's and the feed's
    // `revealedMuted` precedent.
    val revealedMuted = remember { mutableStateListOf<Long>() }
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

        Row(
            modifier = Modifier.padding(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedButton(
                onClick = { vm.markAllRead() },
                enabled = unreadCount > 0,
                modifier = Modifier.testTag(Ids.NOTIFICATION_MARK_READ)
            ) {
                Text(stringResource(R.string.bridges_mark_all_read))
            }
            OutlinedButton(
                onClick = { vm.clearAll() },
                enabled = notifications.isNotEmpty(),
                modifier = Modifier.testTag(Ids.NOTIFICATION_CLEAR)
            ) {
                Text(stringResource(R.string.notifications_clear_all))
            }
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
                            val dest = remember(notif, hasFamilyRelationship) {
                                navigableDestination(notif, hasFamilyRelationship)
                            }
                            NotificationListRow(
                                notification = notif,
                                mutedKeywords = mutedKeywords,
                                revealed = notif.id in revealedMuted,
                                onReveal = { if (notif.id !in revealedMuted) revealedMuted.add(notif.id) },
                                onOpen = dest?.let { d -> { openDestination(d, navController, context) } },
                                onDismiss = { vm.dismiss(notif.id) },
                            )
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }
}

/**
 * The destination a row offers as a control, or null when it paints as plain
 * text: the shared decision's answer (`notification_destination_for`; never a
 * local type table — notifications.md § Don't do these) minus the app-local
 * subtractions: `Family` only while the family tab is gated in, and — in the
 * kids build, which compiles the feed and every outbound link out — neither
 * `Post` nor `External`.
 */
internal fun navigableDestination(
    notification: FfiNotifItem,
    hasFamilyRelationship: Boolean,
    kids: Boolean = BuildConfig.KIDS,
): FfiNotificationDestination? = when (val d = notificationDestinationFor(notification)) {
    null -> null
    is FfiNotificationDestination.Family -> d.takeIf { hasFamilyRelationship }
    is FfiNotificationDestination.Post, is FfiNotificationDestination.External -> d.takeUnless { kids }
    is FfiNotificationDestination.Knock -> d
}

/**
 * Route a destination into the target page's own navigation, as a direct click
 * there would: the post-detail route a search hit uses (it resolves a post the
 * timeline never loaded), Contacts (the pending knocks list there), Settings →
 * Family, and the ordinary external-link opener for a bridged post.
 */
private fun openDestination(
    dest: FfiNotificationDestination,
    navController: NavController,
    context: Context,
) {
    when (dest) {
        is FfiNotificationDestination.Post ->
            navController.navigate("feed/post/${URLEncoder.encode(dest.postId, "UTF-8")}?source=")
        is FfiNotificationDestination.Knock -> navController.navigate("contacts")
        is FfiNotificationDestination.Family -> navController.navigate("settings/family")
        is FfiNotificationDestination.External -> UrlOpener.open(context, dest.url)
    }
}

/**
 * One row of the list: collapsed when the shared decision says a muted word
 * hides it (`notification_muted_for` — the relayed text only, Hide only, never
 * a security notice; notifications.md § Muted words) and the user has not
 * revealed it this session, else the full row.
 */
@Composable
internal fun NotificationListRow(
    notification: FfiNotifItem,
    mutedKeywords: List<MutedKeyword>,
    revealed: Boolean,
    onReveal: () -> Unit,
    onOpen: (() -> Unit)?,
    onDismiss: () -> Unit = {},
) {
    val muted = remember(notification, mutedKeywords) {
        notificationMutedFor(notification, mutedKeywords)
    }
    if (muted && !revealed) {
        MutedNotificationRow(notification, onReveal, onDismiss)
    } else {
        NotificationRow(notification, onOpen, onDismiss)
    }
}

/**
 * `notification-dismiss` — on every row, collapsed or open (§ Muted words rule
 * 4). Disabled on a security notice inside its retention window, by the shared
 * predicate, not a local type check (notifications.md § Retention rule 2); the
 * nest's refusal stays the authority.
 */
@Composable
private fun DismissButton(notification: FfiNotifItem, onDismiss: () -> Unit) {
    TextButton(
        onClick = onDismiss,
        enabled = !notificationRetainedFor(notification, System.currentTimeMillis() * 1000L),
        modifier = Modifier.testTag(Ids.NOTIFICATION_DISMISS),
    ) {
        Text(stringResource(R.string.notifications_dismiss))
    }
}

/**
 * A muted-collapsed row (§ Muted words rule 3): the type icon and the unread
 * mark stay, the placeholder stands in for the sentence, and the one gesture is
 * the reveal. Not clickable and not a `notification-item`, so a muted sender's
 * name never paints as a control. The unread badge still counts it (rule 4).
 * A plain [Row], not a [ListItem]: a ListItem merges its children's semantics,
 * which would fold the placeholder's id into the row.
 */
@Composable
private fun MutedNotificationRow(
    notification: FfiNotifItem,
    onReveal: () -> Unit,
    onDismiss: () -> Unit,
) {
    val (icon, description) = typeToIcon(notification.notifType)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Icon(
            icon, contentDescription = description,
            modifier = Modifier.testTag(Ids.NOTIFICATION_TYPE_ICON),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            stringResource(R.string.muted_words_collapsed),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.weight(1f).testTag(Ids.NOTIFICATION_MUTED),
        )
        TextButton(
            onClick = onReveal,
            modifier = Modifier.testTag(Ids.NOTIFICATION_MUTED_REVEAL_BUTTON),
        ) {
            Text(stringResource(R.string.muted_words_reveal))
        }
        DismissButton(notification, onDismiss)
        if (!notification.isRead) {
            Box(
                modifier = Modifier
                    .size(8.dp)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.primary)
            )
        }
    }
}

@Composable
private fun NotificationRow(
    notification: FfiNotifItem,
    onOpen: (() -> Unit)?,
    onDismiss: () -> Unit,
) {
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
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (!notification.isRead) {
                    Box(
                        modifier = Modifier
                            .size(8.dp)
                            .clip(CircleShape)
                            .background(MaterialTheme.colorScheme.primary)
                    )
                }
                DismissButton(notification, onDismiss)
            }
        },
        // A row the shared decision places is a real control; one it does not is
        // plain text — never a dead control (notifications.md § Deep-link
        // destinations).
        modifier = Modifier
            .testTag(Ids.NOTIFICATION_ITEM)
            .then(if (onOpen != null) Modifier.clickable(onClick = onOpen) else Modifier),
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
