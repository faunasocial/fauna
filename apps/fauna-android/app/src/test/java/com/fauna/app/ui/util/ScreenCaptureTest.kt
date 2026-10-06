package com.fauna.app.ui.util

import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * android's leg of `docs/goal/architecture/security.md` § On-screen secret
 * exposure (screen capture), rule 2.
 *
 * Two halves, deliberately: the **seam** (does the effect acquire and — the half
 * that fails silently — *release*) against a fake guard, and the **mechanism**
 * (does the real guard actually flip `FLAG_SECURE` on a real window) against a
 * real Activity. Testing only the seam would let the production path be wired to
 * nothing; testing only the flag would not reach the dispose and refcount paths,
 * which are where a stuck-on flag comes from.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ScreenCaptureTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private class FakeGuard : CaptureGuard {
        var acquires = 0
        var releases = 0
        val held: Int get() = acquires - releases

        override fun acquire() {
            acquires++
        }

        override fun release() {
            releases++
        }
    }

    /**
     * The whole contract in one test: suppression starts when a secret is
     * revealed and — the half that goes wrong silently — **ends when it is
     * hidden**. A leaked hold is not a security bug but a usability one: the
     * device stops taking screenshots everywhere, with nothing on screen to
     * explain why.
     */
    @Test
    fun suppressionFollowsTheRevealAndIsReleasedWhenItEnds() {
        val guard = FakeGuard()
        var revealed by mutableStateOf(false)

        composeTestRule.setContent {
            CompositionLocalProvider(LocalCaptureGuard provides guard) {
                if (revealed) {
                    SuppressScreenCapture()
                }
            }
        }

        composeTestRule.runOnIdle {
            assertEquals("nothing revealed yet — capture must not be suppressed", 0, guard.held)
        }

        revealed = true
        composeTestRule.runOnIdle {
            assertEquals("a revealed credential must suppress capture", 1, guard.held)
        }

        revealed = false
        composeTestRule.runOnIdle {
            assertEquals("hiding the secret must lift suppression", 0, guard.held)
            assertTrue("the release path never ran", guard.releases > 0)
        }
    }

    /**
     * Two credentials revealed at once hold the guard twice and lift it once — the
     * property that makes a navigation transition (two screens briefly in
     * composition) and the AT Protocol page's multi-reveal map safe. Without
     * refcounting the first disposer would clear the flag while a secret is still
     * painted.
     */
    @Test
    fun twoConcurrentRevealsHoldSuppressionUntilBothEnd() {
        val guard = FakeGuard()
        var first by mutableStateOf(true)
        var second by mutableStateOf(true)

        composeTestRule.setContent {
            CompositionLocalProvider(LocalCaptureGuard provides guard) {
                if (first) SuppressScreenCapture()
                if (second) SuppressScreenCapture()
            }
        }

        composeTestRule.runOnIdle { assertEquals(2, guard.held) }

        first = false
        composeTestRule.runOnIdle {
            assertEquals("one reveal ending must NOT lift suppression for the other", 1, guard.held)
        }

        second = false
        composeTestRule.runOnIdle { assertEquals(0, guard.held) }
    }
}

/**
 * The mechanism half: the guard production actually resolves — no injected fake —
 * sets and clears the real `FLAG_SECURE` bit on the hosting Activity's window.
 * Separate class because it needs an Activity-backed rule.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ScreenCaptureWindowFlagTest {

    @get:Rule
    val composeTestRule = createAndroidComposeRule<ComponentActivity>()

    private fun flagIsSet(): Boolean =
        composeTestRule.activity.window.attributes.flags and
            WindowManager.LayoutParams.FLAG_SECURE != 0

    @Test
    fun theResolvedGuardFlipsFlagSecureOnTheRealWindow() {
        var revealed by mutableStateOf(false)

        composeTestRule.setContent {
            if (revealed) {
                SuppressScreenCapture()
            }
        }

        composeTestRule.runOnIdle {
            assertFalse("FLAG_SECURE must be off with no secret on screen", flagIsSet())
        }

        revealed = true
        composeTestRule.runOnIdle {
            assertTrue(
                "SuppressScreenCapture resolved no window guard — the production path " +
                    "is wired to nothing and every reveal is capturable",
                flagIsSet(),
            )
        }

        revealed = false
        composeTestRule.runOnIdle {
            assertFalse("FLAG_SECURE stuck on after the reveal ended", flagIsSet())
        }
    }
}
