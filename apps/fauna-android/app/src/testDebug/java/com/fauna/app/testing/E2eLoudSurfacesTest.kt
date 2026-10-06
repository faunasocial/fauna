package com.fauna.app.testing

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiConnectionState
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config

/**
 * The tier_1 pins for android's loud-surface e2e seams ([E2eLoudSurfaces]) — what
 * the cross-app journeys (`test_connection_gap_rules.py`,
 * `test_critical_alert_lifetime.py`, the sweep feeders) read, proven here without
 * the emulator: the three state keys at the agent's top level in the shared
 * shapes, the connection pump's delegate counting repeats as reports, the
 * painted-frame read finding error surfaces in a real composition, and both
 * command seams refusing loudly when they cannot land (convention 11).
 *
 * The counting itself is shared Rust (`fauna_e2e_contract`), pinned there; these
 * pin android's wiring onto it.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class E2eLoudSurfacesTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    @Before
    fun setUp() {
        E2eLoudSurfaces.clearForTest()
        TestAgent.setApiClientForTest(null)
    }

    private fun state(appState: AppState = AppState()): JSONObject =
        TestAgent.serializeState(appState, mock(SecureStorage::class.java), null, appState.messages)

    private fun refusalOf(action: String, cmd: JSONObject): String? {
        val appState = AppState()
        runBlocking {
            TestAgent.processCommand(action, cmd, appState, mock(SecureStorage::class.java), null)
        }
        val messages = state(appState).opt("messages") as? JSONObject ?: return null
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    @Test
    fun theConnectionPumpsDelegateCountsEveryReportAndOnlyRealTransitions() {
        // The production call site's door, not the counter directly.
        listOf(
            FfiConnectionState.CONNECTING,
            FfiConnectionState.UNREACHABLE,
            FfiConnectionState.UNREACHABLE,
            FfiConnectionState.UNREACHABLE,
        ).forEach { TestAgent.observeConnectionReport(it) }

        val reports = state().getJSONObject("connection_reports")
        assertEquals(4, reports.getInt("reports"))
        assertEquals("a repeated word is a report, never a transition", 1, reports.getInt("transitions"))
        assertEquals("unreachable", reports.getString("word"))
    }

    @Test
    fun theSweepPassCountersArePublishedTopLevelAsAPairOfInts() {
        val passes = state().getJSONObject("alert_sweep_passes")
        assertEquals(setOf("started", "completed"), passes.keys().asSequence().toSet())
        val started = passes.get("started")
        val completed = passes.get("completed")
        assertTrue("ints, which helpers/waiting.py requires: $passes", started is Int || started is Long)
        assertTrue("ints, which helpers/waiting.py requires: $passes", completed is Int || completed is Long)
        assertTrue(
            "no pass completes before it starts: $passes",
            (completed as Number).toLong() <= (started as Number).toLong(),
        )
    }

    @Test
    fun aFreshProcessPublishesZeroedCountersNotAnAbsentKey() {
        val s = state()
        assertEquals(0, s.getJSONObject("connection_reports").getInt("reports"))
        val painted = s.getJSONObject("painted_errors")
        assertEquals(0, painted.getInt("count"))
        assertEquals(0, painted.getJSONArray("showing").length())
    }

    @Test
    fun thePaintedFrameReadFindsErrorSurfacesInTheComposition() {
        val showError = mutableStateOf(true)
        composeRule.setContent {
            Column {
                Text("posts", Modifier.testTag("feed-view"))
                if (showError.value) {
                    Text("boom", Modifier.testTag("error-message"))
                    // The tag on a container, the words on its child Text.
                    Box(Modifier.testTag("contact-find-error")) { Text("nope") }
                }
            }
        }
        composeRule.waitForIdle()
        val root = composeRule.activity.window.decorView

        composeRule.runOnUiThread {
            val elements = E2eLoudSurfaces.paintedElementsOf(root).associate { it.id to it.text }
            assertEquals("boom", elements["error-message"])
            assertEquals("nope", elements["contact-find-error"])
            assertEquals("posts", elements["feed-view"])
            E2eLoudSurfaces.observeFrameForTest(root)
            E2eLoudSurfaces.observeFrameForTest(root)
        }
        var painted = state().getJSONObject("painted_errors")
        assertEquals("two surfaces, each counted once while it stands: $painted", 2, painted.getInt("count"))
        assertEquals(2, painted.getJSONArray("showing").length())

        showError.value = false
        composeRule.waitForIdle()
        composeRule.runOnUiThread { E2eLoudSurfaces.observeFrameForTest(root) }
        painted = state().getJSONObject("painted_errors")
        assertEquals("a cleared error leaves the count: $painted", 2, painted.getInt("count"))
        assertEquals(0, painted.getJSONArray("showing").length())
    }

    @Test
    fun aWakeWithNoSweepLoopIsRefusedByNameNeverAcked() {
        val shown = refusalOf(
            E2eLoudSurfaces.ALERT_SWEEP_WAKE,
            JSONObject().put("action", E2eLoudSurfaces.ALERT_SWEEP_WAKE),
        )
        assertNotNull("no loop runs in this JVM, so the wake must be refused", shown)
        assertTrue(shown!!, shown.contains("alert_sweep_wake") && shown.contains("nothing to wake"))
    }

    @Test
    fun aReconnectPaceWithNoClientIsRefusedByNameNeverAcked() {
        val shown = refusalOf(
            E2eLoudSurfaces.RECONNECT_BACKOFF,
            JSONObject().put("action", E2eLoudSurfaces.RECONNECT_BACKOFF).put("initial_ms", 50),
        )
        assertNotNull(shown)
        assertTrue(shown!!, shown.contains("reconnect_backoff") && shown.contains("no fauna client"))
    }
}
