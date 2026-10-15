package com.fauna.app.ui.screen.conversations

import android.app.Activity
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.platform.LocalContext
import uniffi.fauna_conversations.ConversationsManager

/**
 * Closes the open thread when the thread screen leaves composition
 * (`docs/goal/ui/conversations.md` § State & data shape → *When a thread is
 * read*): "open" means selected, and the shared manager reads whatever arrives
 * in the selected thread, so a single-pane shell must `clear_selection()` on
 * the way back to the list or every later arrival is read on sight and the row
 * never paints `dm-unread-indicator` (or raises a banner) again. iOS's
 * `.onDisappear { vm.clearSelection() }` and tui's `show_list` are the prior art.
 *
 * A configuration change (rotation) disposes the composition too, but the user
 * is still looking at the thread — only a real leave clears.
 */
@Composable
fun ClearSelectionOnLeave(manager: ConversationsManager) {
    val activity = LocalContext.current as? Activity
    DisposableEffect(manager) {
        onDispose {
            if (activity?.isChangingConfigurations != true) manager.clearSelection()
        }
    }
}
