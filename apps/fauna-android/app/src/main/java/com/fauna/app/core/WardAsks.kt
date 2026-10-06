package com.fauna.app.core

import com.fauna.ffi.FfiException
import com.fauna.ffi.FfiFamilyBlockedPeer
import com.fauna.ffi.FfiFamilyContactRequest
import com.fauna.ffi.FfiFamilyFeedRequest
import com.fauna.ffi.FfiFeedRequestState
import com.fauna.ffi.wardContactAskPending
import com.fauna.ffi.wardFeedRequestState
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

// The supervised ward's in-place asks and the guardian's un-deny list — the
// android lift of four family-safety surfaces (family-safety.md
// § Child-initiated contact requests → *App affordance*, § Feed-source
// approvals, § The bridge-DM gate → *The un-deny surface*):
//
//   * `contact-request-guardian-button` / `contact-request-pending` on the
//     contacts page's Find User result and on another actor's profile;
//   * `bridge-source-request-button` / `bridge-source-request-state` inside a
//     `bridge-card`;
//   * `family-blocked-peer-item` / `family-blocked-peer-allow-button` in the
//     guardian's per-ward editor.
//
// tui leads (`apps/fauna-tui/src/{contacts,profile/mod,bridges,family}.rs`);
// linux (`apps/fauna-linux/src/ward_asks.rs`) and web
// (`apps/fauna-web/src/lib/ward-asks.ts`) carry the same shape. The rules every
// app copies rather than re-derives:
//   (a) the ask is offered ONLY on the typed refusal
//       (`FfiException.GuardianApprovalRequired`) — never on message text;
//   (b) the refusal stays on `error-message` — the page's job;
//   (c) pending is durable — read from `status.contact_requests` /
//       `status.feed_requests`, gated on `supervised_by` ([WardAsks]); a
//       just-asked flag only makes the render answer before the re-read lands;
//   (d) rows are keyed on the ask data, never on compose buffers;
//   (e) an approved feed-source ask is a PROMPT to retry, never an auto-retry;
//   (f) each allow button addresses ITS OWN row's (bridge_id, peer_id);
//   (g) a knock reply carries the peer it was sent to, and paints only on that
//       peer's open;
//   (h) supervision is not re-tested at render — the inputs are
//       supervised-only by construction.
//
// The two ask-matching rules are NOT restated here: they are the shared
// `fauna_client_family::ward_asks::{contact_ask_pending, feed_request_state}`,
// reached over their UniFFI faces [wardContactAskPending] /
// [wardFeedRequestState].

/**
 * The supervised caller's OWN pending asks — `status.contact_requests` and
 * `status.feed_requests` off `fauna.family.status` — plus this session's typed
 * feed-source refusals, held for the three refused-send surfaces that read them
 * back. One instance, owned by [ApiClient]: [ApiClient.familyStatus] is
 * android's one choke point for the status read, so every successful read
 * (the supervised-indicator poll, the family page, the content-policy store)
 * refreshes it, and [ApiClient.clearAuth] — which every identity teardown
 * routes through — drops it. The linux twin is `crate::ward_asks`; tui's is
 * `FamilyState::own_contact_requests` / `own_feed_requests`.
 */
class WardAsks {
    private val _contactRequests = MutableStateFlow<List<FfiFamilyContactRequest>>(emptyList())
    private val _feedRequests = MutableStateFlow<List<FfiFamilyFeedRequest>>(emptyList())
    private val _refusedFeed = MutableStateFlow<List<FeedTriple>>(emptyList())

    /** The ward's own outstanding contact asks (durable, rule (c)). */
    val contactRequests: StateFlow<List<FfiFamilyContactRequest>> = _contactRequests.asStateFlow()

    /** The ward's own live feed-source asks (durable, rule (c)). */
    val feedRequests: StateFlow<List<FfiFamilyFeedRequest>> = _feedRequests.asStateFlow()

    /**
     * The `(bridge_id, operation, target)` triples this session saw refused by
     * the guardian gate — the LOCAL half of the feed-source surface (what
     * turns a refusal the ward just hit into a visible
     * `bridge-source-request-button`); tui's `BridgesState::guardian_refused`.
     * Filled only on the TYPED refusal, so supervised-only by construction.
     */
    val refusedFeed: StateFlow<List<FeedTriple>> = _refusedFeed.asStateFlow()

    /**
     * Fold one successful `fauna.family.status` read. [supervised] is whether it
     * named a guardian: a graduated account has no guardian to be waiting on,
     * so a stale ask from an earlier read must not keep painting "asked —
     * waiting" (the client half of the nest's own graduation drop).
     */
    fun setFromStatus(
        supervised: Boolean,
        contact: List<FfiFamilyContactRequest>,
        feed: List<FfiFamilyFeedRequest>,
    ) {
        _contactRequests.value = if (supervised) contact else emptyList()
        _feedRequests.value = if (supervised) feed else emptyList()
    }

    /** A landed contact ask's re-read. Empty = the re-read FAILED (the ask
     *  itself landed — the guardian has been rung): keep what we hold. */
    fun replaceContactRequests(requests: List<FfiFamilyContactRequest>) {
        if (requests.isNotEmpty()) _contactRequests.value = requests
    }

    /** A landed feed-source ask's re-read — same empty-means-failed rule. */
    fun replaceFeedRequests(requests: List<FfiFamilyFeedRequest>) {
        if (requests.isNotEmpty()) _feedRequests.value = requests
    }

    /** Record a typed guardian refusal of one feed-source operation (a set:
     *  re-refusing the same triple adds nothing). */
    fun noteFeedRefusal(triple: FeedTriple) {
        val held = _refusedFeed.value
        if (triple !in held) _refusedFeed.value = held + triple
    }

    /** Whether an ask for [peerActorIdHex] is outstanding (the shared compare). */
    fun contactAskPending(peerActorIdHex: String): Boolean =
        wardContactAskPending(_contactRequests.value, peerActorIdHex)

    /** Drop everything on an identity change: these are one account's asks and
     *  refusals, and the incoming account's first status read has not landed. */
    fun clear() {
        _contactRequests.value = emptyList()
        _feedRequests.value = emptyList()
        _refusedFeed.value = emptyList()
    }
}

// ── Contact ask (contacts page + profile page) ─────────────────────────────

/** How a knock send ended, classified once — the one failure that reveals the
 *  ask (the TYPED guardian refusal, rule (a)) apart from every other one. tui's
 *  `contacts::KnockSend`, linux's `client::KnockSend`. */
sealed interface KnockSend {
    data object Sent : KnockSend
    data object RefusedByGuardian : KnockSend
    data class Failed(val message: String) : KnockSend
}

/**
 * Run one knock [send] and classify how it ended. The guardian gate is told
 * apart on the exception TYPE the shared `stringify` routes the nest's typed
 * refusal to (`RpcError::is_guardian_approval_required` → `FfiException.
 * GuardianApprovalRequired`) — never on message text, which is the nest's
 * business and would also match an unsupervised user's unrelated failure.
 */
suspend fun classifyKnock(send: suspend () -> Unit): KnockSend =
    try {
        send()
        KnockSend.Sent
    } catch (e: CancellationException) {
        throw e
    } catch (_: FfiException.GuardianApprovalRequired) {
        KnockSend.RefusedByGuardian
    } catch (e: Exception) {
        KnockSend.Failed(e.message ?: e.toString())
    }

/**
 * The per-peer knock state a page holds for the peer it currently shows.
 * [peer] names whom the flags belong to, so a new lookup / a new profile open
 * starts from [forPeer] instead of carrying a refusal over to somebody else.
 */
data class KnockAskState(
    val peer: String?,
    val knockSent: Boolean = false,
    val knockInFlight: Boolean = false,
    val guardianRefused: Boolean = false,
    val askSent: Boolean = false,
    val askInFlight: Boolean = false,
) {
    companion object {
        /** A fresh state for [peer] — a new lookup, or a new profile open. */
        fun forPeer(peer: String?) = KnockAskState(peer = peer)
    }
}

/** What `error-message` should do after a knock reply: clear it, show the
 *  localized guardian-gate sentence (rule (b) — still a real failure, just no
 *  longer a dead end), or show another failure's own text. */
sealed interface KnockErrorEffect {
    data object Clear : KnockErrorEffect
    data object Guardian : KnockErrorEffect
    data class Failed(val message: String) : KnockErrorEffect
}

/**
 * Rule (g): fold a knock reply sent to [sentTo] into the state of the page as
 * it is NOW. The reply can outlive the lookup / open it was sent from — a
 * "Sent" or a guardian refusal for the actor you just left must not paint on
 * the one you now see — so the flags move only when `state.peer == sentTo`.
 * The error effect applies regardless: the send genuinely failed (or landed).
 */
fun foldKnockReply(
    state: KnockAskState,
    sentTo: String,
    result: KnockSend,
): Pair<KnockAskState, KnockErrorEffect> {
    if (state.peer != sentTo) {
        return state to when (result) {
            KnockSend.Sent -> KnockErrorEffect.Clear
            KnockSend.RefusedByGuardian -> KnockErrorEffect.Guardian
            is KnockSend.Failed -> KnockErrorEffect.Failed(result.message)
        }
    }
    val settled = state.copy(knockInFlight = false)
    return when (result) {
        KnockSend.Sent -> settled.copy(knockSent = true) to KnockErrorEffect.Clear
        KnockSend.RefusedByGuardian -> settled.copy(guardianRefused = true) to KnockErrorEffect.Guardian
        is KnockSend.Failed -> settled to KnockErrorEffect.Failed(result.message)
    }
}

/** The guardian ask for [askedFor] landed: flip the just-asked flag only on
 *  that peer's own open (rule (g), the ask half). */
fun foldContactAsked(state: KnockAskState, askedFor: String): KnockAskState =
    if (state.peer == askedFor) state.copy(askSent = true, askInFlight = false) else state

/** The ask for [askedFor] failed: re-enable its button on that peer's open. */
fun foldContactAskFailed(state: KnockAskState, askedFor: String): KnockAskState =
    if (state.peer == askedFor) state.copy(askInFlight = false) else state

/** What the refused-send surface shows beside the knock. */
enum class ContactAskRender { PENDING, ASK }

/**
 * The pair's render: the durable pending label (read FIRST — it survives
 * navigation and a restart, and is honest on a session that never saw the
 * refusal), else the ask button only after a TYPED refusal this open saw, else
 * nothing (`null`). [durablePending] is `WardAsks.contactAskPending(peer)` —
 * the shared compare over the durable list; no supervision test here (rule (h)).
 */
fun contactAskRender(durablePending: Boolean, state: KnockAskState): ContactAskRender? = when {
    durablePending || state.askSent -> ContactAskRender.PENDING
    state.guardianRefused -> ContactAskRender.ASK
    else -> null
}

// ── Feed-source ask (bridges page) ─────────────────────────────────────────

/** The `(bridge_id, operation, target)` triple a feed-source grant is scoped to. */
data class FeedTriple(val bridgeId: String, val operation: String, val target: String)

/** What one bridge card paints for the ask surface. */
data class SourceAskRows(
    /** One `bridge-source-request-state` per durable ask on this bridge. */
    val states: List<FfiFeedRequestState> = emptyList(),
    /** One `bridge-source-request-button` per refused triple this session saw
     *  that has no durable row yet — each carrying ITS OWN triple. */
    val asks: List<FeedTriple> = emptyList(),
) {
    val isEmpty: Boolean get() = states.isEmpty() && asks.isEmpty()
}

/**
 * The card's ask rows (tui's `bridges::source_ask_rows`). Paints nothing in the
 * common case: both inputs are supervised-only by construction (rule (h)).
 * Durable rows first, then session-only refusals; a refused triple with a
 * durable row is skipped, so an answered ask shows its verdict instead of
 * offering the button again. An APPROVED row is a LABEL (the "try again"
 * prompt), never a button — the ward redeems the single-use grant by
 * repeating the ORIGINAL Link / Add Follow (rule (e)). Keyed on the ask data,
 * never the add-follow form's buffers, which dispatch clears (rule (d)).
 */
fun sourceAskRows(
    feedAsks: List<FfiFamilyFeedRequest>,
    refused: List<FeedTriple>,
    bridgeId: String,
): SourceAskRows {
    val states = feedAsks
        .filter { it.bridgeId == bridgeId }
        .mapNotNull { wardFeedRequestState(listOf(it), it.bridgeId, it.operation, it.target) }
    val asks = refused.filter {
        it.bridgeId == bridgeId &&
            wardFeedRequestState(feedAsks, it.bridgeId, it.operation, it.target) == null
    }
    return SourceAskRows(states, asks)
}

// ── The un-deny surface (family page, guardian side) ───────────────────────

/**
 * What ONE allow button un-denies: a ward and that row's own
 * [FfiFamilyBlockedPeer] record, handed as-is to the shared
 * `FfiFamilyClient.allowBlockedPeer` — which owns the wire shape (the
 * idempotent, not queue-scoped approving `dm_hold` decide, so it works long
 * after the hold row that prompted the deny is gone).
 *
 * ⚠ Rule (f): built from ONE row's own [FfiFamilyBlockedPeer], never an index
 * into a list a refresh could reorder — pointing every button at row 0 would
 * un-deny the wrong person while the surface still looked correct.
 */
data class UndenyDecide(
    val supervisedActorId: ByteArray,
    val peer: FfiFamilyBlockedPeer,
) {
    override fun equals(other: Any?): Boolean =
        other is UndenyDecide &&
            supervisedActorId.contentEquals(other.supervisedActorId) &&
            peer == other.peer

    override fun hashCode(): Int = supervisedActorId.contentHashCode() * 31 + peer.hashCode()
}
