package com.fauna.app.testing

import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.focus.FocusManager
import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.robolectric.annotation.Config

/**
 * Android's leg of convention 17 layer (c) — `e2e-systematic-ui-walks.md` §
 * The convention. Mirrors `apps/fauna-apple/.../FocusWalkTestCommandTests.swift`
 * and `fauna_e2e_agent`'s own `focus_move_request`/`switch_pane_target` tests:
 * the payload vocabulary is re-derived in Kotlin (android cannot link the Rust
 * crate), so its rulings are pinned here independently rather than trusted by
 * inspection.
 *
 * `focus_move` is verified against a [mock] [FocusManager] — proving the arm
 * calls [FocusManager.moveFocus] the right number of times, in the right
 * direction, is the whole point (a manager left unwired, or a loop that runs
 * the wrong count, both look identical to "some command ran" without this).
 * `switch_pane` needs no such manager: it always refuses.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FocusWalkTestCommandTest {

    private fun storage() = mock(SecureStorage::class.java)

    private fun errorFromStateProtocol(appState: AppState): String? {
        val state = TestAgent.serializeState(appState, storage(), null, appState.messages)
        if (state.isNull("messages")) return null
        val messages = state.getJSONObject("messages")
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    private fun command(action: String, vararg pairs: Pair<String, Any?>): JSONObject =
        JSONObject().apply {
            put("action", action)
            pairs.forEach { (k, v) -> put(k, v) }
        }

    private fun dispatch(action: String, cmd: JSONObject, appState: AppState) =
        runBlocking { TestAgent.processCommand(action, cmd, appState, storage(), null) }

    @After
    fun unwireFocusManager() {
        // `TestAgent` is a process-global `object` — leaving a mock wired here
        // would silently corrupt an unrelated test's `focus_move` refusal pin.
        TestAgent.focusManager = null
    }

    // ── focus_move ────────────────────────────────────────────────────────────

    @Test
    fun focusMoveNextDrivesTheRealFocusManagerOnce() {
        val manager = mock(FocusManager::class.java)
        TestAgent.focusManager = manager
        val appState = AppState()

        dispatch("focus_move", command("focus_move", "direction" to "next"), appState)

        assertNull("an honoured command writes no refusal", errorFromStateProtocol(appState))
        verify(manager, times(1)).moveFocus(FocusDirection.Next)
        verify(manager, never()).moveFocus(FocusDirection.Previous)
    }

    @Test
    fun focusMovePrevWithExplicitTimesDrivesTheManagerThatManyTimes() {
        val manager = mock(FocusManager::class.java)
        TestAgent.focusManager = manager
        val appState = AppState()

        dispatch(
            "focus_move",
            command("focus_move", "direction" to "prev", "times" to 3),
            appState,
        )

        assertNull(errorFromStateProtocol(appState))
        verify(manager, times(3)).moveFocus(FocusDirection.Previous)
        verify(manager, never()).moveFocus(FocusDirection.Next)
    }

    @Test
    fun focusMoveWithNoTimesDefaultsToOne() {
        val manager = mock(FocusManager::class.java)
        TestAgent.focusManager = manager
        dispatch("focus_move", command("focus_move", "direction" to "next"), AppState())
        verify(manager, times(1)).moveFocus(FocusDirection.Next)
    }

    @Test
    fun focusMoveRefusesAMalformedDirectionByName() {
        val manager = mock(FocusManager::class.java)
        TestAgent.focusManager = manager
        val appState = AppState()

        dispatch("focus_move", command("focus_move", "direction" to "sideways"), appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue(
            "the reason must name the field and the bad value: $shown",
            shown!!.contains("direction") && shown.contains("sideways"),
        )
        verify(manager, never()).moveFocus(FocusDirection.Next)
        verify(manager, never()).moveFocus(FocusDirection.Previous)
    }

    @Test
    fun focusMoveRefusesAnAbsentDirectionAsAbsentNotNull() {
        TestAgent.focusManager = mock(FocusManager::class.java)
        val appState = AppState()

        dispatch("focus_move", command("focus_move"), appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue(
            "a genuinely missing field must read as \"absent\", not \"null\": $shown",
            shown!!.contains("absent"),
        )
    }

    @Test
    fun focusMoveRefusesAMalformedTimes() {
        val manager = mock(FocusManager::class.java)
        TestAgent.focusManager = manager
        val appState = AppState()

        dispatch(
            "focus_move",
            command("focus_move", "direction" to "next", "times" to "three"),
            appState,
        )

        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue(
            "a present-but-malformed `times` is a refusal, never a silent " +
                "fallback to 1 — the exact disguise a `.unwrap_or(1)` read " +
                "would give it: $shown",
            shown!!.contains("times"),
        )
        verify(manager, never()).moveFocus(FocusDirection.Next)
    }

    @Test
    fun focusMoveRefusesANegativeTimes() {
        TestAgent.focusManager = mock(FocusManager::class.java)
        val appState = AppState()

        dispatch(
            "focus_move",
            command("focus_move", "direction" to "next", "times" to -1),
            appState,
        )

        assertTrue(
            errorFromStateProtocol(appState)!!.contains("times"),
        )
    }

    @Test
    fun focusMoveRefusesATimesAboveTheCapRatherThanClampingOrRunningIt() {
        val manager = mock(FocusManager::class.java)
        TestAgent.focusManager = manager
        val appState = AppState()

        dispatch(
            "focus_move",
            command("focus_move", "direction" to "next", "times" to 257),
            appState,
        )

        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue(
            "the cap and its reason must be named: $shown",
            shown!!.contains("257") && shown.contains("256"),
        )
        // The refusal must be BEFORE the loop runs — never a silent clamp that
        // executes a command nobody asked for.
        verify(manager, never()).moveFocus(FocusDirection.Next)
    }

    @Test
    fun focusMoveAtExactlyTheCapIsHonoured() {
        val manager = mock(FocusManager::class.java)
        TestAgent.focusManager = manager
        val appState = AppState()

        dispatch(
            "focus_move",
            command("focus_move", "direction" to "next", "times" to 256),
            appState,
        )

        assertNull(
            "the cap is inclusive — exactly 256 must be honoured, not refused",
            errorFromStateProtocol(appState),
        )
        verify(manager, times(256)).moveFocus(FocusDirection.Next)
    }

    @Test
    fun focusMoveBeforeFaunaNavHostHasComposedNamesTheMissingWindow() {
        // `unwireFocusManager` already guarantees null here, but state it
        // explicitly: this is the "FaunaNavHost has not composed yet" case.
        TestAgent.focusManager = null
        val appState = AppState()

        dispatch("focus_move", command("focus_move", "direction" to "next"), appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull("an unwired manager must be loud, not a silent no-op", shown)
        assertTrue(shown!!.contains("focus_move"))
    }

    // ── switch_pane ───────────────────────────────────────────────────────────

    @Test
    fun switchPaneCarriesADeliberateDocumentedRefusal() {
        val appState = AppState()

        dispatch("switch_pane", command("switch_pane", "pane" to "sidebar"), appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue(
            "a deliberate refusal must be distinguishable from an un-built one: $shown",
            !shown!!.contains("no arm"),
        )
        assertTrue(
            "the reason must name the structural cause, not just decline: $shown",
            shown.contains("ModalNavigationDrawer") || shown.contains("declared platform"),
        )
    }

    @Test
    fun switchPaneRefusesRegardlessOfWhichPaneIsAsked() {
        val appState = AppState()
        dispatch("switch_pane", command("switch_pane", "pane" to "page"), appState)
        assertTrue(!errorFromStateProtocol(appState)!!.contains("no arm"))
    }
}
