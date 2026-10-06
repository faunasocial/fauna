package com.fauna.app.core

import android.content.ActivityNotFoundException
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Pins [UrlOpener.openWith]'s e2e-harness suppression, mirroring linux's
 * `url_opener::open_with` unit tests: under e2e the OS launch never fires;
 * outside e2e it always does. Drives the gate directly with an injected flag
 * and launch lambda — same shape as linux — rather than through [UrlOpener.open]
 * itself, so this never needs a real `Context`/`Intent` resolution.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class UrlOpenerTest {

    @Test
    fun suppressesTheRealLaunchUnderE2e() {
        var called = false
        UrlOpener.openWith("https://example.test/verify?code=abc123", e2e = true) {
            called = true
        }
        assertFalse("the OS launch must not fire under e2e", called)
    }

    @Test
    fun launchesOutsideE2e() {
        var called = false
        UrlOpener.openWith("https://example.test/verify?code=abc123", e2e = false) {
            called = true
        }
        assertTrue("the OS launch must fire outside e2e", called)
    }

    @Test
    fun aLaunchFailureIsCaughtNotThrown() {
        // ActivityNotFoundException is the one launch failure `open` itself can
        // raise (no app registered for the scheme) — it must be logged, not
        // thrown, per this row's correction (2).
        UrlOpener.openWith("https://example.test/verify?code=abc123", e2e = false) {
            throw ActivityNotFoundException("no activity")
        }
        // Reaching this line means the exception did not escape openWith.
    }
}
