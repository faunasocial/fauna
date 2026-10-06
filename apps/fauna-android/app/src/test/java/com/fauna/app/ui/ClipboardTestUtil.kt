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
