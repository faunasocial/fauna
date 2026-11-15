package com.fauna.app.testing

import com.fauna.app.core.ApiClient
import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when`
import org.robolectric.annotation.Config

/**
 * The co-present ceremony's two cross-app agent commands on android —
 * `offline_share_advance_clock` and `offline_share_drop_connections`, the
 * witnesses' only way to reach `p2p.md` § Offline share initiation's "a lapsed
 * receive window refuses the initiator like a stranger" and "a dropped
 * connection picks the same share up again" (offline-sharing outcomes 6 and 5).
 *
 * Both arms call the REAL `test-helpers` UniFFI doors (the host `.so` this
 * runner loads), so a pin here proves the arm reaches the shared Rust, not only
 * that the action name is recognised:
 *
 * - the clock arm acks with no refusal, both moving and resetting the clock;
 * - the drop arm, with a key whose session has no seat bound, comes back with
 *   the shared door's own "no ceremony seat is bound" refusal on the app's
 *   error surface — and leaves `machine_method_result` EMPTY, never a stale
 *   neighbour's value the driver would read as a count (convention 11).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentOfflineShareArmsTest {

    private fun storage() = mock(SecureStorage::class.java)

    private fun dispatch(action: String, cmd: JSONObject, appState: AppState) =
        runBlocking { TestAgent.processCommand(action, cmd, appState, storage(), null) }

    private fun state(appState: AppState): JSONObject =
        TestAgent.serializeState(appState, storage(), null, appState.messages)

    private fun error(appState: AppState): String? {
        val s = state(appState)
        if (s.isNull("messages")) return null
        val messages = s.getJSONObject("messages")
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    @After
    fun unwire() {
        TestAgent.setApiClientForTest(null)
        runBlocking {
            TestAgent.processCommand(
                "offline_share_advance_clock",
                JSONObject().put("now_offset_secs", 0),
                AppState(), storage(), null,
            )
        }
    }

    @Test
    fun theClockArmMovesAndResetsTheSharedAdmissionClockWithoutRefusing() {
        val appState = AppState()
        dispatch(
            "offline_share_advance_clock",
            JSONObject().put("action", "offline_share_advance_clock").put("now_offset_secs", 20 * 60),
            appState,
        )
        assertNull("moving the ceremony clock never refuses", error(appState))

        dispatch(
            "offline_share_advance_clock",
            JSONObject().put("action", "offline_share_advance_clock").put("now_offset_secs", 0),
            appState,
        )
        assertNull("resetting the ceremony clock never refuses", error(appState))
    }

    @Test
    fun theDropArmWithNoClientRefusesLoudly() {
        val appState = AppState()
        dispatch("offline_share_drop_connections", JSONObject().put("action", "offline_share_drop_connections"), appState)

        val shown = error(appState)
        assertNotNull("a drop with no client must refuse, never report zero", shown)
        assertTrue(shown!!, shown.contains("offline_share_drop_connections"))
        assertTrue(
            "no count may be published for a refused drop",
            state(appState).isNull("machine_method_result"),
        )
    }

    @Test
    fun theDropArmReachesTheSharedDoorAndPublishesNoStaleCount() {
        val api = mock(ApiClient::class.java)
        `when`(api.ownerSecretBytes()).thenReturn(ByteArray(32) { 7 })
        TestAgent.setApiClientForTest(api)
        val appState = AppState()

        dispatch("offline_share_drop_connections", JSONObject().put("action", "offline_share_drop_connections"), appState)

        val shown = error(appState)
        assertNotNull("a session with no bound seat must refuse the drop", shown)
        assertTrue(
            "the refusal is the shared door's own, so the arm reached the Rust: $shown",
            shown!!.contains("no ceremony seat is bound"),
        )
        assertEquals(
            "a refused drop leaves the result slot empty",
            true, state(appState).isNull("machine_method_result"),
        )
    }
}
