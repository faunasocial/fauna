package com.fauna.app.testing

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.requiredWidth
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.unit.dp
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
    fun theKnockToastLogIsPublishedTopLevelInTheSharedShape() {
        // The production call site's doors (`NotificationHelper.postKnockNotification`).
        TestAgent.observeKnockNotice()
        TestAgent.observeKnockNotice()
        TestAgent.recordKnockNotice("notifications.row_knock", "a knock", 1003)

        val log = state().getJSONObject("knock_notices")
        assertEquals("every push is seen, muted or not", 2, log.getInt("seen"))
        val fired = log.getJSONArray("fired")
        assertEquals(1, fired.length())
        val entry = fired.getJSONObject(0)
        assertEquals("notifications.row_knock", entry.getString("key"))
        assertEquals("a knock", entry.getString("text"))
        assertEquals(1003, entry.getInt("replace_id"))
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

    /**
     * Convention 17's `painted_text_area`: the room the layout gave each tagged
     * text, never its measured size alone. A `weight(1f)` label beside a sibling
     * that fills the row is starved; a text measured wider than a 0-wide parent
     * is still painted (Compose centres the overflow) unless that parent clips;
     * and a text scrolled out of a scroll container's viewport keeps its box.
     */
    @Test
    fun thePaintedTextAreaReadMeasuresTheRoomNotTheMeasuredSize() {
        composeRule.setContent {
            Column {
                Row(Modifier.width(200.dp)) {
                    Text("@alice:example.org", Modifier.weight(1f).testTag("account-item-handle"))
                    Box(Modifier.width(200.dp)) { Text("actions", Modifier.testTag("fits")) }
                }
                Box(Modifier.width(0.dp).clipToBounds()) {
                    Text("clipped", Modifier.requiredWidth(80.dp).testTag("page-heading"))
                }
                Box(Modifier.width(0.dp)) {
                    Text("overflowing", Modifier.requiredWidth(80.dp).testTag("feed-post-text"))
                }
                Column(Modifier.height(100.dp).verticalScroll(rememberScrollState())) {
                    Spacer(Modifier.height(1000.dp))
                    Text("far below", Modifier.testTag("feed-view"))
                }
                // The tag on a container, the words on its child: no text of its own.
                Box(Modifier.width(0.dp).testTag("wrapper")) { Text("x") }
            }
        }
        composeRule.waitForIdle()
        val root = composeRule.activity.window.decorView
        composeRule.runOnUiThread { E2eLoudSurfaces.observeFrameForTest(root) }

        val record = state().getJSONObject("painted_text_area")
        val starved = record.getJSONArray("starved")
        val byId = (0 until starved.length()).associate {
            starved.getJSONObject(it).let { e -> e.getString("id") to e }
        }
        assertEquals("only the starved and the clipped: $record", setOf("account-item-handle", "page-heading"), byId.keys)
        val title = byId.getValue("account-item-handle")
        assertEquals("@alice:example.org", title.getString("text"))
        assertTrue("the weighted handle got no width: $title", title.getDouble("width") < 1.0)
        assertTrue("its row had area: $title", title.getJSONObject("pane").getDouble("width") >= 1.0)
        val clipped = byId.getValue("page-heading")
        assertTrue("a clip cuts the overflow to nothing: $clipped", clipped.getDouble("width") < 1.0)
    }

    @Test
    fun aFreshProcessPublishesAnEmptyPaintedTextAreaNotAnAbsentKey() {
        assertEquals(0, state().getJSONObject("painted_text_area").getJSONArray("starved").length())
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

    /**
     * The `root_surface` journal (`fauna_e2e_agent::ROOT_SURFACE_KEY`): each
     * frame journals the name the root branch wrote through [RootSurface], or
     * `null` when it painted nothing — and the launch progress view with no
     * cached handle (a first sign-in) paints something, so it is never `null`
     * (`onboarding.md` § Wizard exit handling → *The wizard becomes the app in
     * place*).
     */
    @Test
    fun theRootSurfaceJournalNamesEachFrameAndABlankOneIsNothing() {
        val surface = mutableStateOf("wizard")
        composeRule.setContent {
            com.fauna.app.ui.navigation.RootSurface(surface.value)
            when (surface.value) {
                "wizard" -> Text("Welcome", Modifier.testTag("page-heading"))
                "launch_progress" -> com.fauna.app.ui.screen.LaunchProgressScreen(identityLine = null)
                "blank" -> Box(Modifier)
                else -> Text("posts", Modifier.testTag("feed-view"))
            }
        }
        val root = composeRule.activity.window.decorView
        for (name in listOf("wizard", "launch_progress", "blank", "app")) {
            surface.value = name
            composeRule.waitForIdle()
            composeRule.runOnUiThread { E2eLoudSurfaces.observeFrameForTest(root) }
        }
        val journal = state().getJSONObject("root_surface").getJSONArray("journal")
        val shown = (0 until journal.length()).map { journal.getJSONObject(it).opt("showing") }
        assertEquals(listOf("wizard", "launch_progress", JSONObject.NULL, "app"), shown)
    }

    /** Every launch target names the surface tui names for it; the session
     *  start after the challenge is the progress view, never the app. */
    @Test
    fun everyLaunchTargetCarriesTuisRootSurfaceName() {
        assertEquals("launch_progress", com.fauna.app.ui.navigation.rootSurfaceName(null))
        assertEquals(
            "launch_progress",
            com.fauna.app.ui.navigation.rootSurfaceName(com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.Authenticated),
        )
        assertEquals(
            "wizard",
            com.fauna.app.ui.navigation.rootSurfaceName(
                com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.Wizard("onboarding/identity-choice"),
            ),
        )
        val locked = com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.AccountLocked(1u)
        assertEquals("launch_account_locked", com.fauna.app.ui.navigation.rootSurfaceName(locked))
        assertEquals(
            "identity_stolen_entry",
            com.fauna.app.ui.navigation.rootSurfaceName(locked, stolenEntry = true),
        )
        val retry = com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget.TransientError
        assertEquals("launch_retry", com.fauna.app.ui.navigation.rootSurfaceName(retry))
        assertEquals("nest_retire", com.fauna.app.ui.navigation.rootSurfaceName(retry, retiring = true))
    }
}
