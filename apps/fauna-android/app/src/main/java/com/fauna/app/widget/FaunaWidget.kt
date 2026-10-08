package com.fauna.app.widget

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.glance.*
import androidx.glance.action.actionStartActivity
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetManager
import androidx.glance.appwidget.provideContent
import androidx.glance.appwidget.state.updateAppWidgetState
import androidx.glance.layout.*
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.text.TextStyle
import com.fauna.app.MainActivity
import com.fauna.app.R
import com.fauna.app.core.ShellLog

val UNREAD_COUNT_KEY = intPreferencesKey("unread_count")

/**
 * The home-screen widget. It never computes its number: it paints the count
 * [WidgetUnreadPublisher] last published (`apps/android.md` § App Widgets),
 * copied into each widget instance's own Glance state, which is what
 * recomposes a running widget session.
 */
class FaunaWidget : GlanceAppWidget() {

    override suspend fun provideGlance(context: Context, id: GlanceId) {
        // A widget placed after the last publish has no state of its own yet:
        // seed it from the published snapshot, the one source of the number.
        val published = UnreadSnapshotStore.forApp(context).load() ?: 0
        updateAppWidgetState(context, id) { prefs -> prefs[UNREAD_COUNT_KEY] = published }
        provideContent {
            GlanceTheme {
                WidgetContent(context)
            }
        }
    }

    @Composable
    private fun WidgetContent(context: Context) {
        val prefs = currentState<androidx.datastore.preferences.core.Preferences>()
        val unreadCount = prefs[UNREAD_COUNT_KEY] ?: 0

        Column(
            modifier = GlanceModifier
                .fillMaxSize()
                .padding(12.dp)
                .background(GlanceTheme.colors.surface)
                .clickable(actionStartActivity<MainActivity>()),
            verticalAlignment = Alignment.CenterVertically,
            horizontalAlignment = Alignment.CenterHorizontally
        ) {
            Text(
                text = "Fauna",
                style = TextStyle(
                    fontWeight = FontWeight.Bold,
                    fontSize = 14.sp,
                    color = GlanceTheme.colors.primary
                )
            )

            Spacer(modifier = GlanceModifier.height(8.dp))

            Text(
                text = "$unreadCount",
                style = TextStyle(
                    fontWeight = FontWeight.Bold,
                    fontSize = 32.sp,
                    color = GlanceTheme.colors.onSurface
                )
            )

            Text(
                text = context.getString(R.string.widget_unread_label),
                style = TextStyle(
                    fontSize = 12.sp,
                    color = GlanceTheme.colors.onSurfaceVariant
                )
            )

            Spacer(modifier = GlanceModifier.height(8.dp))

            Text(
                text = context.getString(R.string.conversations_compose_title),
                style = TextStyle(
                    fontWeight = FontWeight.Medium,
                    fontSize = 14.sp,
                    color = GlanceTheme.colors.primary
                ),
                modifier = GlanceModifier
                    .padding(horizontal = 16.dp, vertical = 6.dp)
                    .clickable(actionStartActivity<MainActivity>())
            )
        }
    }

    companion object {
        /**
         * Paint [count] on every placed widget. No widget placed → nothing to do;
         * the published snapshot still seeds one placed later.
         */
        suspend fun renderAll(context: Context, count: Int) {
            try {
                val manager = GlanceAppWidgetManager(context)
                for (glanceId in manager.getGlanceIds(FaunaWidget::class.java)) {
                    updateAppWidgetState(context, glanceId) { prefs -> prefs[UNREAD_COUNT_KEY] = count }
                    FaunaWidget().update(context, glanceId)
                }
            } catch (e: Exception) {
                ShellLog.w("FaunaWidget", "widget render failed: ${e.message}")
            }
        }
    }
}
