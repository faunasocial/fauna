package com.fauna.app.testing

import android.os.Handler
import android.os.Looper
import kotlinx.coroutines.suspendCancellableCoroutine
import org.json.JSONObject
import kotlin.coroutines.resume

/**
 * Android's leg of the cross-app `barrier` / `barrier_probe` TestAgent commands —
 * convention 14's causal anchor for negative asserts
 * (`docs/goal/architecture/e2e-latency-independent-assertions.md`; the contract
 * and the per-app rationale live in ONE home, `fauna_e2e_agent::BARRIER`).
 *
 * **Contract, identical on every app:** the agent acks `barrier` only after all
 * UI-thread work *enqueued before the command* has run. An agent that acks early
 * is worse than no barrier at all: every negative assert built on it silently
 * reverts to the race it was written to remove, and nothing downstream can tell.
 *
 * **Why the ack is ordered after [apply] returns.** [TestAgent]'s poll loop runs
 * each command inside `withContext(Dispatchers.Main)` and pushes
 * `last_command_id` only once that block returns — an in-process handler whose
 * rail acks after return, the same shape as apple's `BarrierTestCommand.swift`.
 * So suspending here until the barrier's hop has run is all the ordering the ack
 * needs; no second rail is built.
 *
 * Lives in the debug source set with the rest of the automation surface
 * (convention 15). Android cannot link the `fauna-e2e-agent` Rust crate, so —
 * as apple and web do — the constants are re-spelled against their one home;
 * `test_agent_barrier.py` spells the same values a third time across the wire.
 */
internal object BarrierTestCommand {
    /** Mirrors `fauna_e2e_agent::BARRIER`. */
    const val BARRIER_ACTION = "barrier"

    /** Mirrors `fauna_e2e_agent::BARRIER_PROBE`. */
    const val PROBE_ACTION = "barrier_probe"

    /** Mirrors `fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD` — the fused-probe flag. */
    private const val FUSE_FIELD = "barrier"

    /** Mirrors `fauna_e2e_agent::BARRIER_PROBE_DEFAULT_COUNT`. */
    private const val DEFAULT_COUNT = 64

    /** The live probe slot, `state.barrier_probe` (`fauna_e2e_agent::BARRIER_PROBE`'s docs). */
    const val PROBE_KEY = "barrier_probe"

    /** Mirrors `fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`. */
    const val ACK_PROBE_KEY = "barrier_ack_probe"

    /** Mirrors `fauna_e2e_agent::barrier_probe_value`. */
    private fun probeValue(token: String, i: Int) = "$token#$i"

    /**
     * The ONE queue both the probe's batch and the barrier's hop ride: plain
     * (synchronous) messages on the main `Looper`.
     *
     * ⚠ **Never `withContext(Dispatchers.Main)` instead** — [apply] already runs
     * on `Dispatchers.Main` (the poll loop's hop), and a nested hop to the
     * dispatcher it is already on runs INLINE rather than enqueuing: the probe's
     * items would be applied before it acks and the self-test would pass against
     * a barrier that does nothing (the windows leg's third vacuity trap, reached
     * from android's side). An explicit `Handler.post` always enqueues.
     *
     * ⚠ **And one discipline for both halves.** `Dispatchers.Main` posts
     * *asynchronous* messages, a plain `Handler.post` synchronous ones, and a
     * Choreographer sync barrier lets the former overtake the latter — so a
     * probe on one and a hop on the other could reorder. Both on this handler,
     * the `MessageQueue` runs them FIFO. Sync is also the conservative pick: a
     * sync message runs after every async message enqueued before it, whether or
     * not a sync barrier is up, so the hop orders after earlier coroutine work
     * on `Dispatchers.Main` too.
     */
    private val mainHandler by lazy { Handler(Looper.getMainLooper()) }

    /**
     * The probe's last applied value (`state.barrier_probe`), `null` until one
     * runs. Written from the queued work itself, on the main thread; read by
     * [TestAgent.serializeState] on the poll thread, hence `@Volatile`.
     */
    @Volatile
    private var probeToken: String? = null

    /**
     * What [probeToken] held when the last barrier acked, frozen
     * (`state.barrier_ack_probe`) — the ONLY key the self-test asserts.
     *
     * ⚠ The live token cannot carry that proof, and this is measured
     * (`fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`): the driver reads state a round
     * trip after the ack, by which time the queue has drained on its own, so a
     * `barrier` that did nothing would pass against the live key.
     */
    @Volatile
    private var ackProbe: String? = null

    /** Whether this action is one of ours, so the dispatch and this object agree. */
    fun handles(action: String) = action == BARRIER_ACTION || action == PROBE_ACTION

    /**
     * Publish both slots as TOP-level state keys (the cross-app test reads
     * `get_state("barrier_ack_probe")` at depth one). `JSONObject.NULL` keeps an
     * unset slot PRESENT as JSON `null`, so a failure reads "acked without
     * draining" rather than "android has no such key".
     */
    fun putState(into: JSONObject) {
        into.put(PROBE_KEY, probeToken ?: JSONObject.NULL)
        into.put(ACK_PROBE_KEY, ackProbe ?: JSONObject.NULL)
    }

    /**
     * Clear both slots — the `reset`/`logout` clear point, so a token cannot leak
     * into the next test of a reused app process (the self-test's precondition
     * asserts the frozen key reads `None` before it probes).
     */
    fun clear() {
        probeToken = null
        ackProbe = null
    }

    /** Test seam: the live probe slot, for the Robolectric pin. */
    internal val probeTokenForTest: String? get() = probeToken

    /** Test seam: the frozen ack slot, for the Robolectric pin. */
    internal val ackProbeForTest: String? get() = ackProbe

    /**
     * The barrier's whole mechanism — the ordering hop plus the ack-time freeze —
     * in ONE place, so the bare `barrier` and the fused `barrier_probe` cannot
     * drift apart (a mutant applied here is applied to both callers).
     *
     * Posts one message behind everything already on [mainHandler]'s queue and
     * suspends until it has run. The freeze happens INSIDE that message — at
     * exactly the queue position the barrier promises — so nothing enqueued
     * after the hop can leak into the frozen value.
     *
     * ⚠ A Looper hop is not a recomposition: work Compose schedules for the next
     * frame is not ordered by this. A negative assert needing "no recomposition
     * happened" needs its own observable (the `tiersReloadToken` precedent in
     * `ProfileOffersSection`), not a longer barrier. Deliberately NOT a
     * settle-sleep either — the contract bounds a barrier to work enqueued
     * *before* it.
     */
    private suspend fun runBarrier() {
        suspendCancellableCoroutine { cont ->
            mainHandler.post {
                ackProbe = probeToken
                cont.resume(Unit)
            }
        }
    }

    /**
     * Apply `barrier` / `barrier_probe`. Returns `null` when honoured, or the
     * reason it was refused — a convention-11 refusal the caller surfaces
     * loudly, never a silent no-op.
     */
    suspend fun apply(action: String, cmd: JSONObject): String? {
        if (action == BARRIER_ACTION) {
            runBarrier()
            return null
        }
        if (action != PROBE_ACTION) return "BarrierTestCommand: not my command: $action"

        // The probe queues UI work on the same queue real work rides and acks
        // WITHOUT waiting for it: only a correct barrier can make it observable.
        val token = cmd.opt("token") as? String
        if (token.isNullOrEmpty()) {
            // A token-less probe would ack green and prove nothing.
            return "$PROBE_ACTION: payload needs a non-empty `token`"
        }
        val count: Int
        if (cmd.has("count") && !cmd.isNull("count")) {
            val raw = cmd.opt("count")
            val n = (raw as? Number)?.toInt()
            if (n == null || n < 0) {
                return "$PROBE_ACTION: `count` must be a non-negative integer, got $raw"
            }
            count = n
        } else {
            count = DEFAULT_COUNT
        }
        // A BATCH, not one item — see `fauna_e2e_agent::BARRIER_PROBE` for why a
        // one-item probe is nearly vacuous on an app whose queue drains by itself.
        for (i in 0 until count) {
            val value = probeValue(token, i)
            mainHandler.post { probeToken = value }
        }

        // Fused form (`fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD`): barrier before
        // acking, inside this same command. Deleting the inter-command gap is what
        // makes the mechanism gradeable — with two commands the driver's round
        // trip between them drains the batch unaided (measured on linux and web).
        // An exact `true` only: a payload that merely *looks* truthy is not the
        // contract's boolean, and ignoring it would freeze `null` loudly anyway.
        if (cmd.opt(FUSE_FIELD) == true) {
            runBarrier()
        }
        return null
    }
}
