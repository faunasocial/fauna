package com.fauna.app.testing

import android.net.Uri
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import com.fauna.app.ui.screen.media.MediaActions
import com.fauna.app.ui.screen.media.MediaContent
import com.fauna.app.ui.viewmodel.PickedFile
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
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_media_machine.FollowedScopeOption
import uniffi.fauna_media_machine.MediaPageSnapshot
import uniffi.fauna_media_machine.ShareLinksSnapshot

/**
 * `compose.file`[file-upload] — the harness door `MediaActions.upload_file`
 * reaches android's Media upload through (`drivers/android.py::set_input_files`
 * stages the host file on the device and sends `{"file", "target"}`;
 * `media.md` § User actions). android's `file-upload` is an `IconButton` that
 * launches the OS `GetContent()` picker, which no in-process agent can drive,
 * so while the upload row is shown it registers the very staging its picker
 * callback calls on `TestAgent.mediaFilePick` — the `ProfileEditForm` shape
 * ([TestAgentProfileEditPickTest]).
 *
 * These pins compose the REAL [MediaContent] under a harness holding the
 * picked Uri exactly as `MediaScreen` does, and the upload reads it through
 * the VM's own [PickedFile] helpers — so they assert what `MediaVM.upload`
 * would seal (name + bytes), never the arm's say-so.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentMediaFilePickTest {

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

    /** The exact command `set_input_files("file-upload", path)` sends. */
    private fun filePatch(path: String, appState: AppState) = runBlocking {
        val cmd = JSONObject().apply {
            put("action", "patch")
            put(
                "state",
                JSONObject().put("compose", JSONObject().put("file", path).put("target", "file-upload")),
            )
        }
        TestAgent.processCommand("patch", cmd, appState, storage(), null)
    }

    private fun snapshot(followedScope: FollowedScopeOption? = null) = MediaPageSnapshot(
        items = emptyList(),
        folders = listOf("photos"),
        folderOptions = emptyList(),
        sort = "name",
        descending = false,
        filter = null,
        filterLabel = null,
        viewGrid = false,
        followed = listOfNotNull(followedScope),
        followedScope = followedScope,
        error = null,
        loaded = true,
        shareCreate = null,
        shareExpiryOptions = emptyList(),
        shareLinks = ShareLinksSnapshot(open = false, loaded = false, rows = emptyList(), revokeConfirm = null),
    )

    private var shown by mutableStateOf(true)
    private var picked by mutableStateOf<Uri?>(null)

    /** What `upload-button` handed the VM: the member name and the bytes. */
    private val uploads = mutableListOf<Pair<String, String>>()

    /** Compose the page the way `MediaScreen` wires it: the pick lands in
     *  screen-held state, `upload-button` reads it through [PickedFile]. */
    private fun openPage(snap: MediaPageSnapshot = snapshot()) {
        val resolver = ApplicationProvider.getApplicationContext<android.content.Context>().contentResolver
        compose.setContent {
            if (shown) {
                MediaContent(
                    snapshot = snap,
                    actions = MediaActions(
                        onFilePicked = { picked = it },
                        onUpload = {
                            picked?.let { uri ->
                                val bytes = PickedFile.readBytes(resolver, uri)
                                uploads += PickedFile.displayName(resolver, uri) to String(bytes!!)
                            }
                        },
                    ),
                    pickedName = picked?.lastPathSegment,
                    formatSize = { "${it}B" },
                    formatDate = { "date" },
                    loadThumbnail = { null },
                    syncStateLabel = "Synced",
                )
            }
        }
        compose.waitForIdle()
    }

    private fun staged(name: String, text: String): String =
        tmp.newFile(name).apply { writeText(text) }.absolutePath

    @After
    fun unregister() {
        // `TestAgent` is a process-global `object` — never leak a handler.
        TestAgent.mediaFilePick = null
    }

    /** The load-bearing case: the staged file becomes the page's pick, and
     *  `upload-button` uploads it under its own name with its own bytes. */
    @Test
    fun theStagedFileUploadsThroughUploadButton() {
        openPage()
        assertNotNull("the upload row registers its pick handler", TestAgent.mediaFilePick)
        val appState = AppState()

        filePatch(staged("notes.txt", "hello media"), appState)
        compose.waitForIdle()

        assertNull("an honoured pick writes no refusal", errorFromStateProtocol(appState))
        compose.onNodeWithTag(Ids.UPLOAD_BUTTON).performClick()
        compose.waitForIdle()
        assertEquals(listOf("notes.txt" to "hello media"), uploads)
    }

    /** No Media page open → a named refusal, never a silent no-op
     *  (convention 11). */
    @Test
    fun aPickWithNoPageOpenIsRefusedByName() {
        val appState = AppState()

        filePatch(staged("notes.txt", "hello media"), appState)

        val shownError = errorFromStateProtocol(appState)
        assertNotNull("a pick with no page open must be loud", shownError)
        assertTrue("the reason must say the page is not open: $shownError", shownError!!.contains("not open"))
    }

    /** A followed (read-only) scope removes the upload row, so it holds no
     *  handler — the pick is refused rather than staged into a set the user
     *  cannot upload to (media.md § Followed public folders). */
    @Test
    fun aFollowedScopeHasNoUploadRowToPickInto() {
        val followed = FollowedScopeOption(value = "f:1", label = "pub", available = true)
        openPage(snapshot(followedScope = followed))
        val appState = AppState()

        filePatch(staged("notes.txt", "hello media"), appState)

        assertNull(TestAgent.mediaFilePick)
        assertNotNull(errorFromStateProtocol(appState))
        assertNull("nothing staged", picked)
    }

    /** Leaving the page unregisters the handler. */
    @Test
    fun thePageUnregistersWhenItCloses() {
        openPage()
        shown = false
        compose.waitForIdle()

        assertNull(TestAgent.mediaFilePick)
    }

    /** A missing staged file is refused by name, not handed to the page. */
    @Test
    fun aMissingStagedFileIsRefusedByName() {
        openPage()
        val appState = AppState()

        filePatch(tmp.root.resolve("absent.bin").absolutePath, appState)

        assertNotNull(errorFromStateProtocol(appState))
        assertNull("nothing staged", picked)
    }
}
