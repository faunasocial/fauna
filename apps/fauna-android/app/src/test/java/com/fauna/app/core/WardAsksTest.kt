package com.fauna.app.core

import com.fauna.ffi.FfiException
import com.fauna.ffi.FfiFamilyBlockedPeer
import com.fauna.ffi.FfiFamilyContactRequest
import com.fauna.ffi.FfiFamilyFeedRequest
import com.fauna.ffi.FfiFeedRequestState
import com.fauna.ffi.FfiFeedSourceOperation
import com.fauna.ffi.feedSourceOperationWire
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The ward-ask rules behind android's four family-safety surfaces
 * (family-safety.md § Child-initiated contact requests → *App affordance*,
 * § Feed-source approvals, § The bridge-DM gate → *The un-deny surface*) —
 * the arms tui pins in `contacts.rs` / `profile/mod.rs` / `bridges.rs` /
 * `family.rs`, and linux in `ward_asks.rs` / `guardian_ask.rs`.
 *
 * The two ask-matching rules are the shared Rust ones reached over UniFFI
 * (`wardContactAskPending` / `wardFeedRequestState`), so this runs under
 * `just android-host-test` (JNA pointed at the host `libfauna_ffi.so`).
 */
class WardAsksTest {

    private val peer = "cd".repeat(32)

    /** The shared feed-source operation names (no app spells them). */
    private val opLink = feedSourceOperationWire(FfiFeedSourceOperation.LINK)
    private val opFollow = feedSourceOperationWire(FfiFeedSourceOperation.FOLLOW)
    private val other = "ef".repeat(32)

    private fun contactAsk(byte: Int) =
        FfiFamilyContactRequest(peerActorId = ByteArray(32) { byte.toByte() }, peerHandle = "", createdAt = 0)

    private fun feedAsk(op: String, target: String, approved: Boolean, bridge: String = "activitypub") =
        FfiFamilyFeedRequest(
            bridgeId = bridge,
            operation = op,
            target = target,
            label = "",
            createdAt = 1,
            approvedAt = if (approved) 2L else null,
        )

    // ── The durable store (rule (c)) ────────────────────────────────────────

    /** A graduated account's read drops the leftovers: no guardian to be
     *  waiting on (the client half of the nest's own graduation drop). */
    @Test
    fun anUnsupervisedReadDropsStaleAsks() {
        val asks = WardAsks()
        asks.setFromStatus(true, listOf(contactAsk(0xcd)), listOf(feedAsk("follow", "npub1abc", true)))
        assertTrue(asks.contactAskPending(peer))
        asks.setFromStatus(false, listOf(contactAsk(0xcd)), listOf(feedAsk("follow", "npub1abc", true)))
        assertFalse(asks.contactAskPending(peer))
        assertTrue(asks.feedRequests.value.isEmpty())
    }

    /** An empty re-read is a FAILED re-read, not "no asks": keep what we hold;
     *  an identity change drops everything, refusals included. */
    @Test
    fun anEmptyRereadKeepsTheHeldAsks_andClearDropsThem() {
        val asks = WardAsks()
        asks.setFromStatus(true, listOf(contactAsk(0xcd)), listOf(feedAsk("follow", "npub1abc", false)))
        asks.noteFeedRefusal(FeedTriple("activitypub", opFollow, "npub1abc"))
        asks.replaceContactRequests(emptyList())
        asks.replaceFeedRequests(emptyList())
        assertTrue(asks.contactAskPending(peer))
        assertEquals(1, asks.feedRequests.value.size)

        asks.clear()
        assertFalse(asks.contactAskPending(peer))
        assertTrue(asks.feedRequests.value.isEmpty())
        assertTrue(asks.refusedFeed.value.isEmpty())
    }

    /** Re-refusing the same triple adds nothing — the refusals are a set. */
    @Test
    fun aRefusalIsRecordedOnce() {
        val asks = WardAsks()
        val t = FeedTriple("activitypub", opFollow, "npub1abc")
        asks.noteFeedRefusal(t)
        asks.noteFeedRefusal(t)
        assertEquals(listOf(t), asks.refusedFeed.value)
    }

    // ── The contact ask (contacts Find User + profile) ──────────────────────

    /** Rule (a): the ask is offered on the exception TYPE, never on message
     *  text — a plain failure whose text happens to name the gate stays a plain
     *  failure (an unsupervised user must never be told they are supervised). */
    @Test
    fun onlyTheTypedRefusalClassifiesAsTheGuardianGate() = runBlocking {
        assertEquals(KnockSend.Sent, classifyKnock { })
        assertEquals(
            KnockSend.RefusedByGuardian,
            classifyKnock { throw FfiException.GuardianApprovalRequired("approval required") },
        )
        assertEquals(
            KnockSend.Failed("guardian_approval_required: spoofed"),
            classifyKnock { throw IllegalStateException("guardian_approval_required: spoofed") },
        )
    }

    /** Nothing paints until the nest actually refused; a transport failure
     *  offers nothing; the typed refusal offers the ask and STAYS an error
     *  (rule (b)); the landed ask reads pending. Mirrors tui's
     *  `a_guardian_refused_knock_offers_the_ask_and_then_shows_it_pending`. */
    @Test
    fun aGuardianRefusedKnockOffersTheAskAndThenShowsItPending() {
        var state = KnockAskState.forPeer(peer)
        assertNull(contactAskRender(false, state))

        val (afterFail, failEffect) = foldKnockReply(state.copy(knockInFlight = true), peer, KnockSend.Failed("inbox send: boom"))
        assertNull("a transport failure must not imply supervision", contactAskRender(false, afterFail))
        assertEquals(KnockErrorEffect.Failed("inbox send: boom"), failEffect)
        assertFalse("a failed knock can be retried", afterFail.knockInFlight)

        val (afterRefusal, refusalEffect) = foldKnockReply(afterFail, peer, KnockSend.RefusedByGuardian)
        state = afterRefusal
        assertEquals(ContactAskRender.ASK, contactAskRender(false, state))
        assertEquals("the refusal stays a real error on error-message", KnockErrorEffect.Guardian, refusalEffect)
        assertFalse("the knock button does not read Sent", state.knockSent)

        state = foldContactAsked(state, peer)
        assertEquals(ContactAskRender.PENDING, contactAskRender(false, state))
    }

    /** A landed knock reads "Sent" (and clears `error-message`) — only once the
     *  nest accepted it, never optimistically. */
    @Test
    fun aSentKnockReadsSent() {
        val (state, effect) = foldKnockReply(KnockAskState.forPeer(peer).copy(knockInFlight = true), peer, KnockSend.Sent)
        assertTrue(state.knockSent)
        assertFalse(state.knockInFlight)
        assertEquals(KnockErrorEffect.Clear, effect)
    }

    /** The durable `status.contact_requests` paints pending on an open that
     *  never saw the refusal (a restart or a return visit) — and only for its
     *  own peer (the shared compare, case-insensitive). */
    @Test
    fun anOutstandingAskRendersPendingWithoutARefusal() {
        val asks = WardAsks()
        asks.setFromStatus(true, listOf(contactAsk(0xcd)), emptyList())
        assertEquals(ContactAskRender.PENDING, contactAskRender(asks.contactAskPending(peer), KnockAskState.forPeer(peer)))
        assertEquals(ContactAskRender.PENDING, contactAskRender(asks.contactAskPending(peer.uppercase()), KnockAskState.forPeer(peer)))
        assertNull(
            "another actor's ask is not this one's",
            contactAskRender(asks.contactAskPending(other), KnockAskState.forPeer(other)),
        )
    }

    /** Rule (g): a knock reply that outlived its lookup / open answers the
     *  actor you LEFT — it must neither flip this page's button to "Sent" nor
     *  offer the ask about the wrong person (the error still lands: the send to
     *  the left actor genuinely failed). Mirrors tui's
     *  `a_knock_reply_for_a_previous_open_does_not_paint_on_this_one`. */
    @Test
    fun aKnockReplyForAPreviousOpenDoesNotPaintOnThisOne() {
        val now = KnockAskState.forPeer(other)
        val (afterRefusal, effect) = foldKnockReply(now, peer, KnockSend.RefusedByGuardian)
        assertEquals(now, afterRefusal)
        assertNull(contactAskRender(false, afterRefusal))
        assertEquals(KnockErrorEffect.Guardian, effect)

        val (afterSent, _) = foldKnockReply(afterRefusal, peer, KnockSend.Sent)
        assertFalse(afterSent.knockSent)
        assertEquals(now, foldContactAsked(afterSent, peer))
    }

    // ── The feed-source ask (bridges) ───────────────────────────────────────

    /** An account that never hit the gate shows NEITHER element — which is
     *  also the unsupervised case, since both inputs are supervised-only. */
    @Test
    fun noRefusalAndNoAskRendersNoSourceRows() {
        assertTrue(sourceAskRows(emptyList(), emptyList(), "activitypub").isEmpty)
    }

    /** A typed refusal offers the ask for the refused triple only (keyed on the
     *  ask data, never the add-follow buffers — rule (d)), on its own bridge. */
    @Test
    fun aGuardianRefusalOffersTheAskForItsOwnTriple() {
        val mine = FeedTriple("activitypub", opFollow, "npub1abc")
        val rows = sourceAskRows(emptyList(), listOf(mine, FeedTriple("other", opLink, "")), "activitypub")
        assertEquals(emptyList<FfiFeedRequestState>(), rows.states)
        assertEquals(listOf(mine), rows.asks)
    }

    /** The durable list wins over the session refusal: a landed ask shows its
     *  verdict instead of offering the button again. */
    @Test
    fun aLandedAskReplacesTheButtonWithItsState() {
        val rows = sourceAskRows(
            listOf(feedAsk("follow", "npub1abc", approved = false)),
            listOf(FeedTriple("activitypub", opFollow, "npub1abc")),
            "activitypub",
        )
        assertEquals(listOf(FfiFeedRequestState.PENDING), rows.states)
        assertTrue(rows.asks.isEmpty())
    }

    /** ⚠ Rule (e): an APPROVED grant is a state (the "try again" prompt),
     *  never an ask button — nothing here retries the operation. */
    @Test
    fun anApprovedAskPromptsTheRetryAndIsNeverAButton() {
        val rows = sourceAskRows(
            listOf(feedAsk("follow", "npub1abc", approved = true), feedAsk("link", "", approved = false, bridge = "other")),
            listOf(FeedTriple("activitypub", opFollow, "npub1abc")),
            "activitypub",
        )
        assertEquals(listOf(FfiFeedRequestState.APPROVED), rows.states)
        assertTrue(rows.asks.isEmpty())
    }

    // ── The un-deny surface (family) ────────────────────────────────────────

    /** Rule (f): the un-deny carries ONE row's own peer record and ward, and
     *  two rows' un-denies never compare equal. */
    @Test
    fun anUndenyAddressesItsOwnRowsPeer() {
        val ward = ByteArray(32) { 3 }
        val mine = FfiFamilyBlockedPeer(bridgeId = "nostr", peerId = "npub1bbb")
        val d = UndenyDecide(ward, mine)
        assertEquals(mine, d.peer)
        assertTrue(d.supervisedActorId.contentEquals(ward))
        assertEquals(UndenyDecide(ByteArray(32) { 3 }, mine.copy()), d)
        assertFalse(d == UndenyDecide(ward, FfiFamilyBlockedPeer(bridgeId = "nostr", peerId = "npub1aaa")))
        assertFalse(d == UndenyDecide(ward, FfiFamilyBlockedPeer(bridgeId = "matrix", peerId = "npub1bbb")))
        assertFalse(d == UndenyDecide(ByteArray(32) { 4 }, mine))
    }
}
