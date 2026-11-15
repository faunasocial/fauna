package com.fauna.app.core

import com.fauna.app.testing.TestAgent
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/**
 * Headless coverage for [AccountReauth.e2eVerdict] — the e2e re-auth seam
 * (`long-term-store.md` § Multi-account evolution → Per-account re-auth). Pure
 * file I/O, so it needs no emulator, no activity, and no biometric hardware; the
 * native [android.hardware.biometrics] prompt itself is the emulator-only part.
 * Mirrors FaunaKit `AccountReauth`'s `{cred_dir}/reauth-result` contract and the
 * driver seam `_write_reauth_verdict`.
 */
class AccountReauthTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private fun writeVerdict(dir: File, text: String) {
        File(dir, "reauth-result").writeText(text)
    }

    @Test
    fun `no seam configured returns null (production prompts for real)`() {
        assertNull(AccountReauth.e2eVerdict(null))
        assertNull(AccountReauth.e2eVerdict(""))
    }

    @Test
    fun `literal approve returns true`() {
        val dir = tmp.newFolder()
        writeVerdict(dir, "approve")
        assertTrue(AccountReauth.e2eVerdict(dir.absolutePath) == true)
    }

    @Test
    fun `approve is trimmed of surrounding whitespace`() {
        val dir = tmp.newFolder()
        writeVerdict(dir, "  approve\n")
        assertTrue(AccountReauth.e2eVerdict(dir.absolutePath) == true)
    }

    @Test
    fun `decline verdict returns false`() {
        val dir = tmp.newFolder()
        writeVerdict(dir, "decline")
        assertEquals(false, AccountReauth.e2eVerdict(dir.absolutePath))
    }

    @Test
    fun `an absent file reads as decline (fail-closed)`() {
        val dir = tmp.newFolder()
        // seam is configured (dir set) but no verdict file written yet
        assertFalse(AccountReauth.e2eVerdict(dir.absolutePath) == true)
        assertEquals(false, AccountReauth.e2eVerdict(dir.absolutePath))
    }

    @Test
    fun `the seam dir is the e2e credential file's own directory`() {
        // The verdict sits BESIDE the credential file the bridge writes into the
        // app's filesDir — the one directory both the bridge (POST
        // /reauth-result) and the app agree on. Deriving it from anything else
        // (an env var nothing on android sets) left the seam permanently off.
        assertEquals(
            "/data/user/0/social.fauna.fauna/files",
            AccountReauth.e2eSeamDir("/data/user/0/social.fauna.fauna/files/e2e_credentials.json"),
        )
    }

    @Test
    fun `no e2e credential file means no seam (production prompts for real)`() {
        assertNull(AccountReauth.e2eSeamDir(null))
        assertNull(AccountReauth.e2eSeamDir(""))
    }

    @Test
    fun `the derived seam dir reads a verdict written beside the credential file`() {
        val dir = tmp.newFolder()
        val credFile = File(dir, "e2e_credentials.json").apply { writeText("{}") }
        writeVerdict(dir, "approve")
        assertEquals(true, AccountReauth.e2eVerdict(AccountReauth.e2eSeamDir(credFile.absolutePath)))
    }

    @Test
    fun `an activation gesture counts once whatever it decided`() {
        // `fauna_e2e_agent::ACTIVATION_GESTURES_KEY`: bumped when the handler
        // has returned — switched, declined, or no-op'd alike. The decline arm
        // changes nothing on screen, so this count is the only completion
        // observable `assert_no_relaunch` can anchor its barrier to.
        val before = TestAgent.activationGestures
        AccountReauth.activationGesture { /* a switch */ }
        AccountReauth.activationGesture { /* a decline: nothing happens */ }
        assertEquals(before + 2, TestAgent.activationGestures)
    }

    @Test
    fun `an activation gesture that throws still counts`() {
        // Counted in a `finally`: a gesture that failed still FINISHED, and a
        // count bumped only on the happy path would hang a negative assert
        // instead of failing it.
        val before = TestAgent.activationGestures
        runCatching { AccountReauth.activationGesture { error("switch refused") } }
        assertEquals(before + 1, TestAgent.activationGestures)
    }

    @Test
    fun `an empty verdict file reads as decline`() {
        val dir = tmp.newFolder()
        writeVerdict(dir, "")
        assertEquals(false, AccountReauth.e2eVerdict(dir.absolutePath))
    }
}
