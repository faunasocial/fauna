package com.fauna.app.testing

import android.os.Looper
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.DelicateCoroutinesApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode

/**
 * The tier_1 pin for android's `barrier` / `barrier_probe` leg ([BarrierTestCommand]).
 *
 * Robolectric's PAUSED main `Looper` makes the ordering deterministic: posted
 * messages run only when the test idles the looper, so this pins what the e2e
 * (`tests/e2e-unified/tests/test_agent_barrier.py`) cannot — that the probe
 * applies NOTHING before it acks. That early ack is what keeps the e2e honest,
 * and it is exactly the property an inline "already on the main thread? just run
 * it" hop would break: the test thread here IS the main thread, as the agent's
 * command block is in production (`withContext(Dispatchers.Main)`).
 *
 * The command runs in an UNDISPATCHED, `Dispatchers.Unconfined` coroutine so a
 * suspension shows as an incomplete [Job] and resumes inline when the looper
 * delivers the barrier's message.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
@LooperMode(LooperMode.Mode.PAUSED)
@OptIn(DelicateCoroutinesApi::class)
class BarrierTestCommandTest {

    @Before
    fun setUp() = BarrierTestCommand.clear()

    @After
    fun tearDown() = BarrierTestCommand.clear()

    private fun run(action: String, payload: JSONObject): Pair<Job, Array<String?>> {
        val refusal = arrayOf<String?>(null)
        val job = GlobalScope.launch(Dispatchers.Unconfined, start = CoroutineStart.UNDISPATCHED) {
            refusal[0] = BarrierTestCommand.apply(action, payload)
        }
        return job to refusal
    }

    private fun idleMain() = shadowOf(Looper.getMainLooper()).idle()

    private fun probe(token: String, count: Int, fused: Boolean = false) =
        JSONObject().put("token", token).put("count", count).apply { if (fused) put("barrier", true) }

    @Test
    fun theProbeAcksWithoutApplyingItsBatch() {
        val (job, refusal) = run("barrier_probe", probe("t", 64))

        assertTrue("the plain probe acks at once", job.isCompleted)
        assertNull(refusal[0])
        assertNull("nothing applied before the queue runs — an inline hop would fail here",
            BarrierTestCommand.probeTokenForTest)

        idleMain()
        assertEquals("t#63", BarrierTestCommand.probeTokenForTest)
        assertNull("a plain probe never freezes the ack slot", BarrierTestCommand.ackProbeForTest)
    }

    @Test
    fun theFusedProbeAcksOnlyAfterItsWholeBatchAndFreezesTheLastItem() {
        val (job, refusal) = run("barrier_probe", probe("f", 64, fused = true))

        assertFalse("the fused probe must not ack before its barrier hop ran", job.isCompleted)
        assertNull(BarrierTestCommand.ackProbeForTest)

        idleMain()
        assertTrue(job.isCompleted)
        assertNull(refusal[0])
        assertEquals("f#63", BarrierTestCommand.ackProbeForTest)
    }

    @Test
    fun aSeparateBarrierFreezesWhatWasQueuedBeforeIt() {
        run("barrier_probe", probe("o", 64))
        val (job, _) = run("barrier", JSONObject())
        assertFalse(job.isCompleted)

        idleMain()
        assertTrue(job.isCompleted)
        assertEquals("o#63", BarrierTestCommand.ackProbeForTest)
    }

    @Test
    fun workQueuedAfterTheBarrierDoesNotLeakIntoTheFrozenValue() {
        run("barrier_probe", probe("a", 4))
        run("barrier", JSONObject())
        run("barrier_probe", probe("b", 4))

        idleMain()
        assertEquals("a#3", BarrierTestCommand.ackProbeForTest)
        assertEquals("b#3", BarrierTestCommand.probeTokenForTest)
    }

    @Test
    fun aBarrierOnAnIdleQueueReturnsAfterOneHop() {
        val (job, refusal) = run("barrier", JSONObject())
        idleMain()
        assertTrue(job.isCompleted)
        assertNull(refusal[0])
        assertNull(BarrierTestCommand.ackProbeForTest)
    }

    @Test
    fun clearEmptiesBothSlots() {
        run("barrier_probe", probe("c", 2, fused = true))
        idleMain()
        assertNotNull(BarrierTestCommand.ackProbeForTest)

        BarrierTestCommand.clear()
        assertNull(BarrierTestCommand.probeTokenForTest)
        assertNull(BarrierTestCommand.ackProbeForTest)
    }

    @Test
    fun bothSlotsArePublishedAsPresentTopLevelKeys() {
        val state = JSONObject()
        BarrierTestCommand.putState(state)
        assertTrue(state.has("barrier_probe") && state.isNull("barrier_probe"))
        assertTrue(state.has("barrier_ack_probe") && state.isNull("barrier_ack_probe"))
    }

    @Test
    fun aMalformedProbeIsRefusedLoudly() {
        val (_, noToken) = run("barrier_probe", JSONObject().put("count", 3))
        assertNotNull(noToken[0])

        val (_, badCount) = run("barrier_probe", JSONObject().put("token", "x").put("count", "3"))
        assertNotNull(badCount[0])
        idleMain()
        assertNull("a refused probe enqueues nothing", BarrierTestCommand.probeTokenForTest)
    }
}
