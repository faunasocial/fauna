package com.fauna.app.p2pshare

import android.content.Context
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.ffi.FfiCeremonySeat
import com.fauna.ffi.FfiGroupShareViews
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.fauna_client_capabilities.CeremonyStatus
import uniffi.fauna_client_capabilities.OfflineSharePanel
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Process-wide owner of this session's co-present offline-share ceremony
 * (`docs/goal/behavior/p2p.md` § Offline share initiation): the seat, the
 * panel the user has open, the acts in flight, and the FFI doors they cross.
 * **The built half** of the `p2p-share` glue (`dynamic-features.md`
 * § Platform-family surface excision), compiled into every build type but
 * `storeSafe`, which takes the inert `src/noP2pShare/` twin. That twin is
 * signature-identical and names no ceremony FFI symbol. Nothing outside these
 * two files may name one: the shared code talks to the seam records in
 * `src/main/.../p2pshare/OfflineShareRows.kt`.
 *
 * **Why none of this lives on `DevicesVM`.** The Folders page's VM is scoped to
 * its navigation entry, and a ceremony outlives a page visit by design: the
 * recipient's "did the invitation land" check IS a re-navigation, and the
 * initiator's Begin blocks across the whole walk while the counterpart decides.
 * A VM-scoped seat would lose its armed `expect_from` listener on that very
 * re-navigation (windows found the same hazard on a page-scoped seat and moved
 * it onto `App` — `p2p.md` § Implementation status today), and a
 * `viewModelScope` Begin would be CANCELLED by it — cancelling a UniFFI future
 * drops the Rust walk mid-ceremony. So the seat, the state the panel paints,
 * and the scope every act runs on all live here, for the session's lifetime.
 *
 * A leaf (only the application context) so [ApiClient] can inject it without a
 * Hilt cycle and [reset] it on identity teardown — the seat is bound to one
 * actor's key, and a seat that survived a sign-out would listen as the wrong
 * person. Every act therefore takes the [ApiClient] it crosses the boundary
 * through as an argument. Every decision is shared Rust (priority #2); this
 * class holds state and sequences acts. Mirrors apple
 * `APIClient.{bindOfflineShareSeat, offlineShareViewSnapshot, …}` and windows'
 * `INestRpcClient` ceremony doors.
 */
@Singleton
class OfflineShareHost @Inject constructor(
    @ApplicationContext private val appContext: Context,
) {
    /** Every ceremony act runs here, never on a UI scope (see the class doc). */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * The bound ceremony listener — one per session, bound on the first panel
     * open (`p2p.md` § Offline share initiation → *One seat per session*) and
     * handed straight back on every later open. `null` until then, or when
     * the `p2p-share` brake refused.
     */
    @Volatile private var seat: FfiCeremonySeat? = null
    private val bindLock = Mutex()

    /** The initiator this device armed an expectation for, so cancel can withdraw it. */
    @Volatile private var expectingFrom: ByteArray? = null

    /**
     * The walk in flight (a Begin, or a consent) — a second one while it runs
     * would mint a competing scope or race the first to the same record, so
     * [launchWalk] refuses it. Deliberately NOT surfaced as an interim status:
     * the walk reports its final status in one round trip on every leg.
     */
    @Volatile private var walk: Job? = null

    @Volatile private var panel = OfflineSharePanel.CLOSED
    @Volatile private var status = CeremonyStatus.IDLE
    /** `offline-share-peer-code-input`'s live value — a draft, committed only by Begin/Expect. */
    @Volatile private var peerCodeInput = ""

    /**
     * Bumped on every write the paint decision reads (panel, typed code,
     * status, the seat binding — which gives the compare code its
     * addressing). The Screen collects it and recomputes [decision]; the
     * state itself stays behind this seam.
     */
    private val _changes = MutableStateFlow(0)
    val changes: StateFlow<Int> = _changes.asStateFlow()

    /**
     * The consent cards and the landed scopes — ONE read, because they come
     * from one record and must never disagree about a scope.
     */
    private val _groupShares = MutableStateFlow(GroupShareRows())
    val groupShares: StateFlow<GroupShareRows> = _groupShares.asStateFlow()

    /**
     * Why the last act stopped short — the page's `error-message` reading. Held
     * here, not on the VM, because the act that fails may finish after the page
     * that started it is gone.
     */
    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    private fun changed() { _changes.update { it + 1 } }

    private fun setStatus(value: CeremonyStatus) { status = value; changed() }

    private fun setPanel(value: OfflinePanel) {
        panel = when (value) {
            OfflinePanel.CLOSED -> OfflineSharePanel.CLOSED
            OfflinePanel.INITIATE -> OfflineSharePanel.INITIATE
            OfflinePanel.RECEIVE -> OfflineSharePanel.RECEIVE
        }
        changed()
    }

    private fun failed(e: Exception) {
        _error.value = appContext.getString(R.string.folders_error_offline_share).replace("{message}", e.message ?: "")
    }

    // ── The FFI doors ────────────────────────────────────────────────────

    /**
     * This session's ceremony seat — bound on the first call, handed straight
     * back on every later one: one actor-keyed listener per session, in either
     * order or in a race, hence the lock. Throws when the `p2p-share` brake
     * refuses.
     */
    private suspend fun bindSeat(api: ApiClient): FfiCeremonySeat =
        bindLock.withLock {
            seat ?: com.fauna.ffi.offlineShareBindSeat(api.nestRpc(), api.ownerSecretBytes())
                .also { seat = it; changed() }
        }

    /**
     * The consent cards and the landed scopes, in one read. The face is
     * already fail-safe empty on a read error; not being connected yet is the
     * same answer here, never a page error. The session's bound seat rides
     * along: with the nest unreachable, the shared read answers from the
     * seat's in-memory ceremony record, so a co-present consent card still
     * paints (`p2p.md` § Offline share initiation).
     */
    private suspend fun readGroupShares(api: ApiClient) {
        val views = runCatching {
            com.fauna.ffi.offlineShareLoadGroupShares(api.nestRpc(), api.ownerSecretBytes(), seat)
        }.getOrDefault(FfiGroupShareViews(emptyList(), emptyList()))
        _groupShares.value = GroupShareRows(
            invitations = views.invitations.map { GroupInvitation(it.scopeId, it.initiator, it.shortId) },
            scopes = views.scopes.map { GroupScope(it.shortId, it.memberCount, it.sharedBy) },
        )
    }

    /** The typed code's parse against this actor — `null` with no identity. */
    private fun parsePeerCode(api: ApiClient, input: String): com.fauna.ffi.PeerCodeParsed? {
        val ownerSecret = api.secret?.let { HexUtil.hexToBytes(it) } ?: return null
        return runCatching { com.fauna.ffi.offlineShareParsePeerCode(input, ownerSecret) }.getOrNull()
    }

    // ── The paint decision ───────────────────────────────────────────────

    /**
     * The whole paint decision for the affordance — `null` when no identity
     * secret is held, which hides the section: an affordance that cannot work
     * is worse than an absent one. The code it carries is the bare key until
     * the seat binds and addressed after.
     */
    fun decision(api: ApiClient): OfflineShareDecision? {
        val ownerSecret = api.secret?.let { HexUtil.hexToBytes(it) } ?: return null
        val view = runCatching {
            com.fauna.ffi.offlineShareView(panel, ownerSecret, seat, peerCodeInput, status)
        }.getOrNull() ?: return null
        val gates = com.fauna.ffi.offlineShareGates(view)
        return OfflineShareDecision(
            panel = when (view.panel) {
                OfflineSharePanel.CLOSED -> OfflinePanel.CLOSED
                OfflineSharePanel.INITIATE -> OfflinePanel.INITIATE
                OfflineSharePanel.RECEIVE -> OfflinePanel.RECEIVE
            },
            ownCode = view.ownCode,
            peerCode = view.peerCode,
            statusLabel = com.fauna.ffi.offlineShareStatusLabel(view.status),
            codeError = parsePeerCode(api, view.peerCode)?.error
                ?.let { com.fauna.ffi.offlineShareCodeErrorLabel(it) },
            showsEntryButtons = gates.showsEntryButtons,
            showsCodeWidgets = gates.showsCodeWidgets,
            canBegin = gates.canBegin,
            canExpect = gates.canExpect,
            showsCancel = gates.showsCancel,
        )
    }

    // ── The acts ─────────────────────────────────────────────────────────

    /**
     * Run a walk (Begin, consent) unless one is already running. A refused one
     * is a no-op, which is what the user pressing twice meant.
     */
    @Synchronized
    private fun launchWalk(act: suspend () -> Unit) {
        if (walk?.isActive == true) return
        walk = scope.launch { act() }
    }

    /**
     * `offline-share-button` / `offline-receive-button` — open a panel and bind
     * the session's seat if it is not bound yet (an open on a bound seat is a
     * pure flip). A bind the brake refuses closes the panel again, with the
     * reason on the error bar: a panel that can never act is worse than none.
     */
    fun open(api: ApiClient, which: OfflinePanel) {
        setPanel(which)
        peerCodeInput = ""
        setStatus(CeremonyStatus.IDLE)
        _error.value = null
        if (seat != null) return
        scope.launch {
            try {
                bindSeat(api)
                readGroupShares(api)
            } catch (e: Exception) {
                setPanel(OfflinePanel.CLOSED)
                failed(e)
            }
        }
    }

    fun setPeerCode(text: String) {
        peerCodeInput = text
        changed()
    }

    /**
     * `offline-share-begin-button` — the initiator's whole walk, reported as
     * its final status in one round trip (every leg's shape). A walk that
     * lands a scope re-reads the listing, so the set lists on the page the
     * user is looking at rather than at their next visit.
     */
    fun begin(api: ApiClient) {
        val peerCode = peerCodeInput
        launchWalk {
            _error.value = null
            try {
                val result = com.fauna.ffi.offlineShareInitiate(api.nestRpc(), bindSeat(api), api.ownerSecretBytes(), peerCode)
                setStatus(result)
                if (com.fauna.ffi.offlineShareStatusLandsAScope(result)) readGroupShares(api)
            } catch (e: Exception) {
                setStatus(CeremonyStatus.FAILED)
                failed(e)
            }
        }
    }

    /**
     * `offline-receive-expect-button` — the receive act: admit exactly this
     * initiator's ceremony frames for the expectation's TTL. An in-memory write
     * on the live seat, true the instant the user says so. The button is
     * disabled while this cannot succeed, so the failure arms below are the
     * belt to that brace — never a silent drop (e2e convention 11).
     */
    fun expect(api: ApiClient) {
        val peer = parsePeerCode(api, peerCodeInput)?.actor
        scope.launch {
            _error.value = null
            try {
                if (peer == null || peer.isEmpty()) error(appContext.getString(R.string.folders_offline_share_code_malformed))
                bindSeat(api).expectFrom(peer)
                expectingFrom = peer
                setStatus(CeremonyStatus.EXPECTING)
            } catch (e: Exception) {
                setStatus(CeremonyStatus.FAILED)
                failed(e)
            }
        }
    }

    /**
     * `offline-share-cancel-button` — close the panel; on the recipient side
     * also withdraw the expectation (rule 6: the user changed their mind). The
     * SEAT stays bound: the listener is the session's, not this ceremony's.
     */
    fun cancel() {
        val bound = seat
        val expecting = expectingFrom
        if (bound != null && expecting != null) {
            runCatching { bound.cancelExpectation(expecting) }
        }
        expectingFrom = null
        peerCodeInput = ""
        status = CeremonyStatus.IDLE
        _error.value = null
        setPanel(OfflinePanel.CLOSED)
    }

    /**
     * The consent card's Accept (`folder-share-accept-button`, the group arm):
     * one act for one decision, addressed by scope id. Re-reads the listing
     * after, so the card leaves and the landed set lists.
     */
    fun consent(api: ApiClient, scopeId: ByteArray) {
        launchWalk {
            _error.value = null
            try {
                setStatus(com.fauna.ffi.offlineShareConsent(api.nestRpc(), bindSeat(api), api.ownerSecretBytes(), scopeId))
            } catch (e: Exception) {
                setStatus(CeremonyStatus.FAILED)
                failed(e)
            }
            readGroupShares(api)
        }
    }

    /** The consent card's Decline (`folder-share-decline-button`, the group arm) — terminal. */
    fun decline(api: ApiClient, scopeId: ByteArray) {
        scope.launch {
            _error.value = null
            try {
                com.fauna.ffi.offlineShareDecline(api.nestRpc(), bindSeat(api), api.ownerSecretBytes(), scopeId)
            } catch (e: Exception) {
                failed(e)
            }
            readGroupShares(api)
        }
    }

    /**
     * Re-read the consent cards and the landed scopes. Rides the page's
     * `start` — every edge it becomes visible on — so an offer that arrived
     * while the user was elsewhere knocks when they come back, never only at
     * sign-in (the stale-once-fetched trap linux paid for).
     */
    fun loadGroupShares(api: ApiClient) {
        scope.launch { readGroupShares(api) }
    }

    /**
     * The identity-teardown boundary (sign-out, account switch, factory
     * reset) — called from [ApiClient.clearAuth]. Stops every act in flight and
     * drops the seat, whose listener would otherwise keep answering as the
     * previous actor.
     */
    fun reset() {
        scope.coroutineContext.cancelChildren()
        walk = null
        seat?.destroy()
        seat = null
        expectingFrom = null
        panel = OfflineSharePanel.CLOSED
        status = CeremonyStatus.IDLE
        peerCodeInput = ""
        _groupShares.value = GroupShareRows()
        _error.value = null
        changed()
    }
}
