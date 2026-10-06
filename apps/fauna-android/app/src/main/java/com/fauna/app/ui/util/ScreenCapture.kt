package com.fauna.app.ui.util

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.view.Window
import android.view.WindowManager
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalContext
import java.util.WeakHashMap

/**
 * android's implementation of `docs/goal/architecture/security.md` § On-screen
 * secret exposure (screen capture), **rule 2 only**: while a *minted, revocable*
 * credential is actually revealed on screen, the host window carries
 * [WindowManager.LayoutParams.FLAG_SECURE], which blocks screenshots, screen
 * recording, and the recents-screen thumbnail (the concrete harm the finding
 * named).
 *
 * ⚠ **Rule 1 outranks rule 2 and forbids the obvious "completion" of this
 * feature.** `secret-key-display` (the identity secret) and
 * `recovery-kit-secret-display` are **client-only-resident root secrets**: a user
 * who loses one loses the account outright. Users screenshot a recovery kit
 * precisely because that copy is what saves them, so suppressing capture there
 * would trade a shoulder-surfing risk for an account-loss risk — the irreversible
 * one. **Do not mount [SuppressScreenCapture] on those screens.** The mail /
 * ATProto credential reveals are safe to suppress because losing one costs a
 * revoke-and-re-add and nothing else.
 *
 * **Why the flag is refcounted rather than set-and-cleared per call site.** It is
 * a property of the *window*, not of a composable, and more than one reveal can be
 * on screen at once (the AT Protocol page keeps a map of session-revealed secrets, and
 * a navigation transition briefly holds two screens in composition). A plain
 * set-on-enter / clear-on-exit pair would let the last disposer clear the flag out
 * from under a reveal that is still visible. Refcounting per window makes both
 * failure directions unrepresentable — and the direction that would otherwise be
 * silent is the *stuck-on* one, which users experience as a phone that has stopped
 * taking screenshots rather than as a security feature.
 */
interface CaptureGuard {
    /** One more reason to suppress capture on this window. */
    fun acquire()

    /** One fewer. Suppression lifts when the last holder releases. */
    fun release()
}

/**
 * Test seam. Production leaves this null and resolves the real per-window guard
 * from [LocalContext]; a test provides a fake to observe acquire/release —
 * including the release-on-dispose path, which is the half that goes wrong
 * silently.
 */
val LocalCaptureGuard = staticCompositionLocalOf<CaptureGuard?> { null }

/**
 * Suppress screen capture on the host window for as long as this call site is in
 * composition. Mount it *conditionally*, gated on a secret actually being
 * revealed — `if (revealedSecret != null) SuppressScreenCapture()` — so the
 * suppression window is the reveal window and nothing wider.
 *
 * A no-op when there is no Activity window to flag (a `@Preview`, or a unit test
 * that composes without one). That default is the safe direction: it under-applies
 * a defense-in-depth control rather than leaving a device unable to screenshot.
 */
@Composable
fun SuppressScreenCapture() {
    val injected = LocalCaptureGuard.current
    val context = LocalContext.current
    val guard = injected ?: remember(context) { context.activityWindow()?.let(::captureGuardFor) }

    DisposableEffect(guard) {
        guard?.acquire()
        onDispose { guard?.release() }
    }
}

/** The real guard: one per [Window], so the refcount is shared across call sites. */
private class WindowCaptureGuard(private val window: Window) : CaptureGuard {
    private var holders = 0

    @Synchronized
    override fun acquire() {
        if (holders++ == 0) {
            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
    }

    @Synchronized
    override fun release() {
        if (holders == 0) return // unbalanced release; never let the count go negative
        if (--holders == 0) {
            window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
    }
}

private val guardsByWindow = WeakHashMap<Window, CaptureGuard>()

private fun captureGuardFor(window: Window): CaptureGuard = synchronized(guardsByWindow) {
    guardsByWindow.getOrPut(window) { WindowCaptureGuard(window) }
}

/** Unwrap the Compose [Context] to the hosting Activity's window, if there is one. */
private tailrec fun Context.activityWindow(): Window? = when (this) {
    is Activity -> window
    is ContextWrapper -> baseContext.activityWindow()
    else -> null
}
