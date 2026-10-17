package com.fauna.app.testing

import com.fauna.app.core.AppState
import com.fauna.app.core.ConsentHandoff
import com.fauna.app.core.SecureStorage
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config

/**
 * The `open_route` automation command — the e2e seam of the `fauna://` in-app
 * routes (`docs/goal/architecture/apps/android.md` § Navigation → *In-app
 * routes*). It feeds the SAME door `MainActivity`'s intent intake takes; an
 * unparseable URI is a loud convention-11 refusal here, never the silent drop
 * the intent intake gives it.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentOpenRouteTest {

    private val requestUri =
        "urn:ietf:params:oauth:request_uri:handoff-fixture"

    private fun dispatch(uri: String, appState: AppState) = runBlocking {
        val cmd = JSONObject().apply { put("action", "open_route"); put("uri", uri) }
        TestAgent.processCommand("open_route", cmd, appState, mock(SecureStorage::class.java), null)
    }

    /** The door is process-wide: take back whatever a case left in it. */
    @After
    fun drainTheDoor() {
        val door = ConsentHandoff.shared
        door.takeHeld()
        door.pending.value?.let { door.finishOpen(it) }
    }

    @Test
    fun aConsentRouteSignedOutIsHeldAndAcksAtOnce() {
        val appState = AppState()
        dispatch("fauna://consent/$requestUri", appState)
        assertNull(appState.messages.refusedAgentCommand.value)
        assertEquals(requestUri, ConsentHandoff.shared.held)
    }

    @Test
    fun aRouteThisAppDoesNotTakeIsRefusedLoudly() {
        val appState = AppState()
        dispatch("fauna://consent/https://evil.example/", appState)
        val refusal = appState.messages.refusedAgentCommand.value
        assertTrue("$refusal", refusal?.contains("not a route this app takes") == true)
        assertNull(ConsentHandoff.shared.held)
    }
}
