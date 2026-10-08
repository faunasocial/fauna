package com.fauna.app.testing

import android.os.Looper
import org.robolectric.Shadows.shadowOf

/**
 * [FaunaRobolectricTestRunner]'s end-of-test drain, called reflectively in the
 * test's sandbox class loader (the runner itself lives outside the sandbox).
 * Runs the main looper's due work so a message Compose's `AndroidUiDispatcher`
 * posted is never dropped by Robolectric's between-test reset — see the runner's
 * doc. Due work only: no clock advance, so a test's own delayed tasks and any
 * self-reposting animation cannot turn this into an unbounded loop.
 */
object MainLooperDrain {
    @JvmStatic
    fun drain() {
        shadowOf(Looper.getMainLooper()).idle()
    }
}
