package com.fauna.app.testing

import com.fauna.ffi.FfiServeTally
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The serve hold and the serve tally arm (`share-serve-tally` in
 * `tests/e2e-unified/surfaces.toml`): [TestAgent.serveTallyToJson] publishes
 * the shape `actions/backups.py::share_serve_tally` reads — the twin of tui's
 * and linux's `share_serve_tally_json` and apple's `serveTallyState` — and the
 * `offline_share_hold_serves` arm's door turns the shared recorder's hold on
 * and off, read back through the same `shareServeTally` door the state key
 * calls (`fauna_sync_engine::share_serve_tally`, `p2p.md` § Implementation
 * status today, the resumed-transfer paragraph). Robolectric-run: `org.json`
 * is the android.jar stub on a plain JVM.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentServeTallyTest {

    @After
    fun liftTheHold() {
        // Process-wide, and nothing auto-resets it.
        com.fauna.ffi.offlineShareHoldServes(false)
    }

    @Test
    fun serveTallyToJsonPublishesTheSharedShapeAsNumbers() {
        val tally = FfiServeTally(
            manifests = mapOf("set/a.txt" to 2uL),
            chunks = mapOf("set/a.txt" to 5uL),
            parked = 1uL,
            held = true,
        )
        val json = TestAgent.serveTallyToJson(tally)
        assertEquals(2L, json.getJSONObject("manifests").getLong("set/a.txt"))
        assertEquals(5L, json.getJSONObject("chunks").getLong("set/a.txt"))
        assertEquals(1, json.getInt("parked"))
        assertTrue(json.getBoolean("held"))
        assertTrue("a count is a JSON number", json.get("parked") is Number)
    }

    @Test
    fun theHoldDoorTurnsTheSharedRecordersHoldOnAndOff() {
        com.fauna.ffi.offlineShareHoldServes(true)
        assertTrue(
            "the hold reads on through the tally door",
            TestAgent.serveTallyToJson(com.fauna.ffi.shareServeTally()).getBoolean("held"),
        )
        com.fauna.ffi.offlineShareHoldServes(false)
        assertFalse(
            "and off again",
            TestAgent.serveTallyToJson(com.fauna.ffi.shareServeTally()).getBoolean("held"),
        )
    }
}
