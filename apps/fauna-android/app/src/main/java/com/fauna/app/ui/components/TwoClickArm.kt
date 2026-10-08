package com.fauna.app.ui.components

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import kotlinx.coroutines.delay

/**
 * The modal-less destructive control's arm state — `docs/goal/architecture/apps/
 * common.md` § Two-click confirm: the first press arms (the control relabels in
 * place), a second press while armed performs, and the arm drops by itself after
 * [DISARM_AFTER_MS]. One helper for every android control that follows the rule,
 * never a private copy per screen.
 *
 * Usage: `val arm = rememberTwoClickArm()`, `onClick = { arm.press(onConfirm) }`,
 * and label the control from [armed].
 */
@Stable
class TwoClickArm internal constructor() {
    var armed by mutableStateOf(false)
        private set

    /** First press arms; a press while armed disarms and runs [onConfirm]. */
    fun press(onConfirm: () -> Unit) {
        if (armed) {
            armed = false
            onConfirm()
        } else {
            armed = true
        }
    }

    /** Drop the arm without performing (cancel, a nav-away, an edited input). */
    fun disarm() {
        armed = false
    }

    companion object {
        /** The ratified disarm timeout (`common.md` § Two-click confirm). */
        const val DISARM_AFTER_MS = 4_000L
    }
}

/** Remember a [TwoClickArm] that disarms itself [TwoClickArm.DISARM_AFTER_MS] after arming. */
@Composable
fun rememberTwoClickArm(): TwoClickArm {
    val arm = remember { TwoClickArm() }
    LaunchedEffect(arm.armed) {
        if (arm.armed) {
            delay(TwoClickArm.DISARM_AFTER_MS)
            arm.disarm()
        }
    }
    return arm
}
