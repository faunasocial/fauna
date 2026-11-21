package com.fauna.app.ui

import android.content.ClipboardManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider

/**
 * The current system-clipboard plain text, or `null` if empty. Robolectric backs
 * `ClipboardManager` with a working shadow, so a `CopyButton` / `CopyableRow`
 * click (which writes via `LocalClipboardManager`) is observable here — the
 * behavioral assertion that replaced the old "did the `onCopy` callback fire?"
 * checks after the copy idiom moved into the shared composables.
 */
fun currentClipboardText(): String? {
    val cm = ApplicationProvider.getApplicationContext<Context>()
        .getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    return cm.primaryClip?.getItemAt(0)?.text?.toString()
}

/**
 * A Compose clipboard whose every write throws [message] — provided as
 * `LocalClipboardManager` around a page so a test can drive the
 * `common.copy_failed` path a real clipboard never takes under Robolectric.
 */
class FailingClipboard(private val message: String = "clipboard unavailable") :
    androidx.compose.ui.platform.ClipboardManager {
    override fun getText(): androidx.compose.ui.text.AnnotatedString? = null

    override fun setText(annotatedString: androidx.compose.ui.text.AnnotatedString) {
        throw IllegalStateException(message)
    }
}
