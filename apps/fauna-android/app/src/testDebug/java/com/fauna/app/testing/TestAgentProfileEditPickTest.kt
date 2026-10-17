package com.fauna.app.testing

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.ApiClient
import com.fauna.app.core.AppMessages
import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.screen.profile.ProfileEditFormSection
import com.fauna.app.ui.viewmodel.ProfileEditVM
import com.fauna.ffi.FfiProfileImageEdit
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.mockito.Mockito.RETURNS_DEFAULTS
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config

/**
 * `compose.file`[profile-edit-avatar|profile-edit-banner] — the harness door
 * `test_profile_edit_sets_and_clears_avatar_and_banner` reaches android's
 * picture pickers through (`drivers/http_bridge.py::set_input_files` sends
 * `{"file", "target"}`; `profile.md` § UI elements: "the value is a settable
 * path"). The OS `GetContent()` picker cannot be driven by an in-process agent,
 * so while the edit form is shown it registers the very pick handler its two
 * picker callbacks call (EXIF-strip, then `ProfileEditVM.stageAvatar`/
 * `stageBanner`) on `TestAgent.profileEditImagePick` — the
 * `ReactionPickerDialog` shape ([TestAgentReactionPickTest]).
 *
 * These pins compose the REAL [ProfileEditFormSection] over a REAL
 * [ProfileEditVM] whose [ApiClient] is faked only at the wire: the eager upload
 * returns a hash naming the bytes it received, and Save's signing call records
 * the image edits — so each pin asserts on what the VM would sign, never on
 * the arm's say-so.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentProfileEditPickTest {

    @get:Rule
    val compose = createComposeRule()

    @get:Rule
    val tmp = TemporaryFolder()

    private fun storage() = mock(SecureStorage::class.java)

    /** `messages.error` off the state protocol — what `error_text()` reads. */
    private fun errorFromStateProtocol(appState: AppState): String? {
        val state = TestAgent.serializeState(appState, storage(), null, appState.messages)
        if (state.isNull("messages")) return null
        val messages = state.getJSONObject("messages")
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    /** The exact command `set_input_files(<target>, path)` sends. */
    private fun filePatch(target: String, path: String, appState: AppState) = runBlocking {
        val cmd = JSONObject().apply {
            put("action", "patch")
            put(
                "state",
                JSONObject().put("compose", JSONObject().put("file", path).put("target", target)),
            )
        }
        TestAgent.processCommand("patch", cmd, appState, storage(), null)
    }

    /** An [ApiClient] faked at the wire: `uploadPublicPostBlob` answers
     *  `hash-<the bytes as text>`, `buildEditedProfileWithImages` records its
     *  avatar/banner edits. Everything else answers Mockito's defaults. */
    private class Fixture {
        val uploads = mutableListOf<String>()
        var signedAvatar: FfiProfileImageEdit? = null
        var signedBanner: FfiProfileImageEdit? = null
        val api: ApiClient = mock(ApiClient::class.java) { inv ->
            when (inv.method.name) {
                "uploadPublicPostBlob" -> {
                    val text = String(inv.arguments[0] as ByteArray)
                    uploads += text
                    "hash-$text"
                }
                "buildEditedProfileWithImages" -> {
                    signedAvatar = inv.arguments[4] as FfiProfileImageEdit
                    signedBanner = inv.arguments[5] as FfiProfileImageEdit
                    ByteArray(0)
                }
                else -> RETURNS_DEFAULTS.answer(inv)
            }
        }
        val vm = ProfileEditVM(api)
    }

    private var shown by mutableStateOf(true)

    /** Compose the form as the `profile-edit-button` click leaves it. */
    private fun openForm(f: Fixture) {
        compose.setContent {
            CompositionLocalProvider(LocalAppMessages provides AppMessages()) {
                if (shown) {
                    ProfileEditFormSection(actorIdHex = null, onSaved = {}, onCancel = {}, vm = f.vm)
                }
            }
        }
        compose.waitForIdle()
    }

    /** Save through the VM's real signing path and let it settle. */
    private fun save(f: Fixture) {
        f.vm.save("", "", emptyList()) {}
        compose.waitForIdle()
    }

    private fun picked(name: String, text: String): String =
        tmp.newFile(name).apply { writeText(text) }.absolutePath

    @After
    fun unregister() {
        // `TestAgent` is a process-global `object` — never leak a handler.
        TestAgent.profileEditImagePick = null
    }

    /** The load-bearing case: both arms stage their bytes on the live form's
     *  VM, and Save signs each uploaded hash into its own field. */
    @Test
    fun bothArmsStageTheirBytesOnTheOpenForm() {
        val f = Fixture()
        openForm(f)
        assertNotNull("the open form registers its pick handler", TestAgent.profileEditImagePick)
        val appState = AppState()

        filePatch("profile-edit-avatar", picked("avatar.bin", "avatar-bytes"), appState)
        filePatch("profile-edit-banner", picked("banner.bin", "banner-bytes"), appState)
        compose.waitForIdle()

        assertNull("an honoured pick writes no refusal", errorFromStateProtocol(appState))
        assertEquals("each pick uploads its own file", listOf("avatar-bytes", "banner-bytes"), f.uploads)
        save(f)
        assertEquals(FfiProfileImageEdit.Set("hash-avatar-bytes"), f.signedAvatar)
        assertEquals(FfiProfileImageEdit.Set("hash-banner-bytes"), f.signedBanner)
    }

    /** No form open → a named refusal, never a silent no-op (convention 11):
     *  the `profile-edit-button` click stays part of the journey. */
    @Test
    fun aPickWithNoFormOpenIsRefusedByName() {
        val f = Fixture()
        val appState = AppState()

        filePatch("profile-edit-avatar", picked("avatar.bin", "avatar-bytes"), appState)

        val shownError = errorFromStateProtocol(appState)
        assertNotNull("a pick with no open form must be loud", shownError)
        assertTrue("the reason must say the form is not open: $shownError", shownError!!.contains("not open"))
        assertTrue("nothing uploaded", f.uploads.isEmpty())
    }

    /** Closing the form unregisters its handler — never left dangling over a
     *  VM that outlives the form (it is scoped to the profile screen). */
    @Test
    fun theFormUnregistersWhenItCloses() {
        val f = Fixture()
        openForm(f)
        shown = false
        compose.waitForIdle()
        val appState = AppState()

        filePatch("profile-edit-banner", picked("banner.bin", "banner-bytes"), appState)

        assertNull(TestAgent.profileEditImagePick)
        assertNotNull(errorFromStateProtocol(appState))
        assertTrue("nothing uploaded", f.uploads.isEmpty())
    }
}
