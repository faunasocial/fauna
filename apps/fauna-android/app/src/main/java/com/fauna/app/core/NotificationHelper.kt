package com.fauna.app.core

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import com.fauna.app.MainActivity
import com.fauna.app.R
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiKnock
import com.fauna.ffi.FfiNotificationText
import com.fauna.ffi.knockPushMutedFor
import com.fauna.ffi.knockTextFor
import dagger.hilt.android.qualifiers.ApplicationContext
import javax.inject.Inject
import javax.inject.Singleton
import uniffi.fauna_core.MutedKeyword

@Singleton
class NotificationHelper @Inject constructor(
    @ApplicationContext private val context: Context
) {
    /**
     * Raise the new-message banner for one conversation; returns whether the
     * notification was actually handed to [NotificationManagerCompat].
     *
     * The caller — [com.fauna.app.core.conversations.MessageBannerObserver] —
     * records a fire only on `true`, so the e2e fired-banner log never claims a
     * banner this process did not raise (apple's `postMessageNotification` and
     * windows' `ShowMessageNotification` return the same answer).
     *
     * **The POST_NOTIFICATIONS grant is deliberately not read here.** On API 33+
     * the platform itself drops a notification the user has not allowed, and
     * raises no prompt — the ask belongs to a user gesture (the shell's launch
     * effect), never to a message's arrival. Checking the grant first would add a
     * fourth when/for-whom rule in glue (`conversations.md` § Where logic lives)
     * and key the log on a platform setting rather than on what the app did; web
     * made the same call over `Notification.permission`. `false` means the call
     * itself failed ([SecurityException]).
     */
    fun postMessageNotification(from: String, subject: String, conversationId: String): Boolean {
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
            putExtra("route", "conversation/$conversationId")
        }
        val pendingIntent = PendingIntent.getActivity(
            context, conversationId.hashCode(), intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val notification = NotificationCompat.Builder(context, CHANNEL_MESSAGES)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(from)
            .setContentText(subject)
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .build()

        return try {
            NotificationManagerCompat.from(context)
                .notify(conversationId.hashCode(), notification)
            true
        } catch (_: SecurityException) {
            // Permission not granted
            false
        }
    }

    fun postGroupNotification(groupName: String, from: String, body: String, groupId: String) {
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
            putExtra("route", "group/$groupId")
        }
        val pendingIntent = PendingIntent.getActivity(
            context, groupId.hashCode(), intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val notification = NotificationCompat.Builder(context, CHANNEL_GROUPS)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(groupName)
            .setContentText("$from: $body")
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .build()

        try {
            NotificationManagerCompat.from(context)
                .notify("grp-$groupId".hashCode(), notification)
        } catch (_: SecurityException) {}
    }

    fun postGroupInviteNotification(from: String, groupName: String, groupId: String) {
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
            putExtra("route", "contacts")
        }
        val pendingIntent = PendingIntent.getActivity(
            context, "inv-$groupId".hashCode(), intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val notification = NotificationCompat.Builder(context, CHANNEL_GROUPS)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(context.getString(R.string.notifications_group_invite_title))
            .setContentText(context.getStringFmt(R.string.notifications_group_invite_body, from, groupName))
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .build()

        try {
            NotificationManagerCompat.from(context)
                .notify("inv-$groupId".hashCode(), notification)
        } catch (_: SecurityException) {}
    }

    /**
     * Raise the OS toast for an inbound knock (contact request) — the android
     * twin of linux's `notify_knock` and windows' knock toast. What it says is
     * the shared decision [knockTextFor] (`behavior/notifications.md`
     * § Localized body): the knock row's own sentence when the push carries a
     * body this build knows, else the knock toast's catalog sentence naming the
     * sender — never the knocker's raw message on its own. The message arg arrives
     * already sanitized to plain capped text. Coalesced under one id
     * ([KNOCK_NOTIFICATION_ID]). Opens Contacts, where the pending knock is.
     *
     * **None for a knock a muted word collapses** (`behavior/notifications.md`
     * § Muted words rule 6): the toast paints the same relayed text as the
     * knock's row and cannot be revealed, so it asks the shared
     * [knockPushMutedFor] over [mutedKeywords] first. Returns whether it fired.
     */
    fun postKnockNotification(knock: FfiKnock, mutedKeywords: List<MutedKeyword>): Boolean {
        if (knockPushMutedFor(knock, mutedKeywords)) return false
        val body = when (val text = knockTextFor(knock)) {
            is FfiNotificationText.Localized -> resolveLocalized(context, text.text).orEmpty()
            is FfiNotificationText.Verbatim -> text.text
        }
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
            putExtra("route", "contacts")
        }
        val pendingIntent = PendingIntent.getActivity(
            context, KNOCK_NOTIFICATION_ID, intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val notification = NotificationCompat.Builder(context, CHANNEL_CONTACTS)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(context.getString(R.string.notifications_knock_title))
            .setContentText(body)
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .build()

        return try {
            NotificationManagerCompat.from(context)
                .notify(KNOCK_NOTIFICATION_ID, notification)
            true
        } catch (_: SecurityException) {
            false
        }
    }

    /**
     * Raise the banner for one push message the nest sent while the app was
     * closed (`common.md` § Push Notifications) — the generic strings a
     * `web-push` delivery carries, on the messages channel. Coalesced under one
     * id ([PUSH_NOTIFICATION_ID]): the strings say only that something
     * arrived, so a stack of them says nothing more. A tap opens the app.
     */
    fun postPushNotification(title: String, body: String): Boolean {
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val pendingIntent = PendingIntent.getActivity(
            context, PUSH_NOTIFICATION_ID, intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val notification = NotificationCompat.Builder(context, CHANNEL_MESSAGES)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(title)
            .setContentText(body)
            .setAutoCancel(true)
            .setContentIntent(pendingIntent)
            .build()
        return try {
            NotificationManagerCompat.from(context).notify(PUSH_NOTIFICATION_ID, notification)
            true
        } catch (_: SecurityException) {
            false
        }
    }

    companion object {
        /** One id for every push banner ([postPushNotification]). */
        const val PUSH_NOTIFICATION_ID = 0x5055
        const val CHANNEL_MESSAGES = "fauna_messages"
        const val CHANNEL_GROUPS = "fauna_groups"
        const val CHANNEL_CONTACTS = "fauna_contacts"

        /**
         * One id for every knock toast, as linux's `NOTIF_ID_KNOCK`: a knock is a
         * stranger's and keys cost nothing, so a per-sender id would let a flood of
         * fresh keys stack unbounded heads-up alerts. Each new knock replaces the last.
         */
        const val KNOCK_NOTIFICATION_ID = 0x4B4E
    }
}
