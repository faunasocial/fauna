package com.fauna.app.core

import android.content.Context
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.app.data.api.*
import com.fauna.app.p2pshare.OfflineShareHost
import com.fauna.ffi.*
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.Json
import uniffi.fauna_core.RsvpResponse
import okhttp3.*
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody
import java.io.IOException
import javax.inject.Inject
import javax.inject.Singleton
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine

/** One of the user's own folders as an ingest target: its label and its `FolderRef` wire string. */
data class FolderChoice(val name: String, val folderId: String)

@Singleton
class ApiClient @Inject constructor(
    // The base client (timeouts, pool) — deliberately NOT a property: every
    // call sends through [httpClient], the trust-deciding client built from it.
    okHttpClient: OkHttpClient,
    // App context for the conversations-session MLS db path (the scoped
    // `mls.db`, never MlsManager's `fauna-mls.db` — two engines on
    // one SQLite race, libs/fauna-ffi/Cargo.toml § conversations-session).
    @ApplicationContext internal val context: Context,
    // Read-only source of the logged-in self handle + domain, for the
    // conversations-session self address. Leaf storage (SecureStorage), one-way,
    // so no Hilt cycle.
    internal val sessionAccount: SessionAccount,
    // Owns the shared ConversationsManager + its observer. The connect path hands
    // it the per-session FfiDraftsSync so it can restore drafts on launch and
    // autosave compose edits (draft-persistence v2, file-sync.md § Drafts Sync),
    // and the connected FfiNestClient so it builds the conversations receive
    // session + starts the inbound-mail receive loop (mail-spam.md § Impl item 6).
    // One-way dependency (the host injects nothing), so no Hilt cycle.
    private val conversationsManagerHost: ConversationsManagerHost,
    // Owns the events-rail live draft (draft-persistence v2, events twin of
    // conversationsManagerHost's draft-sync section — file-sync.md § Drafts
    // Sync, docs/goal/ui/events.md § Persistence). The connect path hands it
    // the per-session FfiEventDraftsSync; unlike conversations/posts, this
    // host restores AND holds the live typed draft itself, since the Events
    // page has no shared manager to hold it. One-way dependency, no Hilt
    // cycle.
    private val eventDraftsHost: com.fauna.app.core.events.EventDraftsHost,
    // Resolves the account-scoped store paths (account-scoping.md § Serialized
    // switching) — today the conversations MLS store this class opens per session
    // and the sync-engine state dir. Leaf (Context + the account registry), so no
    // Hilt cycle.
    private val accountStores: AccountStores,
    // The cross-page critical-alerts registry (critical-alerts.md § Mechanism)
    // — cleared here on every identity teardown. Leaf (no deps of its own), so
    // no Hilt cycle.
    private val criticalAlertsHost: CriticalAlertsHost,
    // This session's co-present offline-share ceremony (p2p.md § Offline share
    // initiation) — its seat is bound to the signed-in actor's key, so it is
    // reset on every identity teardown too. Leaf, so no Hilt cycle.
    private val offlineShareHost: OfflineShareHost,
    // Raises the OS knock toast off the knock pump. Leaf (Context only), so no
    // Hilt cycle.
    private val notificationHelper: NotificationHelper,
) : KidsExcisedApi() {
    var nodeUrl: String = ""

    // The one HTTP client the residual-HTTP leg (blob upload / download /
    // `HEAD`) sends through. A plain `OkHttpClient` makes no trust decision
    // beyond the OS trust store, which refuses the nest's self-signed floor
    // cert (`SSLHandshakeException`) and broke media against a same-box nest;
    // this one answers with the three-way policy windows' `DirectNestClient`
    // and apple's `APIClient` apply, for whatever nest [nodeUrl] names at
    // handshake time (security.md § Transport trust, [NestCertTrust]). Lazy:
    // the first call builds the TLS context, not the constructor.
    private val httpClient: OkHttpClient by lazy { NestCertTrust.makeClient(okHttpClient) { nodeUrl } }

    /** The supervised caller's own pending contact / feed-source asks and this
     *  session's typed feed-source refusals ([WardAsks]) — folded by every
     *  [familyStatus] read, dropped by [clearAuth]. */
    val wardAsks = WardAsks()

    private var token: String? = null
    private var tokenExpiresAt: Long = 0
    // `internal`, not `private`, for the payments variant seam alone — the §4
    // webhook-URL preview derives the actor id from it locally (see the
    // Payments comment below).
    internal var secret: String? = null

    // WS-RPC façade for the fauna.bridges.* / fauna.email.* / fauna.feed.* /
    // fauna.posts.* / fauna.{knocks,contacts,inbox.mode}.* / fauna.notifications.*
    // / fauna.account.* / fauna.quota.get / fauna.profile.handle.change kinds
    // (the HTTP twins were deleted in the T9+T10 sweep, the feed/posts T4
    // cutover, the social-inbox T4 cutover, and the account T3 cutover). Built
    // once per session in [authenticate]; the reconnect supervisor inside
    // FfiNestClient owns token refresh + reconnection. Mirrors the
    // Arc<NestClient> the Linux app builds in apps/fauna-linux/src/client.rs.
    internal var nestClient: FfiNestClient? = null
        private set

    /** Bumped by [clearAuth], android's one identity-teardown funnel. Captured
     *  at the top of [authenticate] and re-checked after each of its two
     *  awaits, the bearer mint and `connect()` on the client it seated, before
     *  anything lands (`account-scoping.md` § The scoping taxonomy → the
     *  in-memory corollary). The same shape as [WebPublishStore]'s
     *  `generation`; apple's `sameActorSince()` is the twin. Neither caller
     *  serializes against a switch: `ensureAuthenticated()` refreshes an
     *  expired token from any RPC site, so a clear can land inside either
     *  await. */
    private var actorGeneration = 0L

    // The two FFI entry points [authenticate] reaches before it holds a
    // client — `internal` only so [ApiClientActorGenerationTest] can hold the
    // mint open across an identity change and hand back a client that never
    // dials. Production never reassigns either.
    internal var bearerMinter: suspend (String, ByteArray) -> FfiBearerToken =
        { url, secretBytes -> mintBearer(url, secretBytes) }
    internal var nestClientFactory: (String, ByteArray) -> FfiNestClient =
        { url, secretBytes -> FfiNestClient(url, secretBytes) }

    private var bridgesClient: FfiBridgesClient? = null
    private var emailClient: FfiEmailClient? = null
    private var adminClient: FfiAdminClient? = null
    private var feedClient: FfiFeedClient? = null
    private var postsClient: FfiPostsClient? = null
    private var contactsClient: FfiContactsClient? = null
    private var notificationsClient: FfiNotificationsClient? = null
    private var inboxClient: FfiInboxClient? = null
    private var accountClient: FfiAccountClient? = null
    private var spamClient: FfiSpamClient? = null
    private var conversationsClient: FfiConversationsClient? = null
    private var syncClient: FfiSyncClient? = null
    private var moderationClient: FfiModerationClient? = null
    private var snapshotsClient: FfiSnapshotsClient? = null
    private var caldavClient: FfiCaldavClient? = null
    // Read-only CardDAV Address Book surface (the Contacts page's "Address Book"
    // segment — carddav-server.md § Independent enablement). The consumption
    // analogue of caldavClient; both read the same encrypted DAV store over the
    // shared msek gate.
    private var carddavClient: FfiCarddavClient? = null
    private var blueskyClient: FfiBlueskyClient? = null
    // fauna.features.* WS-RPC seam — the gated-feature plane's transparency
    // read (`feature-limits-section`, `dynamic-features.md` § Transparency &
    // auditability).
    private var featuresClient: FfiFeaturesClient? = null
    // Per-session conversations-rail draft autosync (FfiDraftsSync over
    // fauna.drafts.{get,put}); handed to ConversationsManagerHost on connect for
    // the restore-on-launch + debounced save, and closed here on teardown.
    private var draftsSync: FfiDraftsSync? = null

    // Per-session posts-rail (feed composer) draft autosync — the [draftsSync]
    // twin for the `"posts"` rail (file-sync.md § Drafts Sync, docs/goal/ui/feed.md
    // § Persistence). Built eagerly here at connect, same as [draftsSync] (cheap: a
    // transport handle + the derived key) — but unlike conversations, the
    // [FfiFeedManager] itself is built LAZILY on first Feed-page access (see
    // [feedManager] below), so [FeedManagerHost], not this class, drives the
    // restore-on-launch + debounced save once a manager exists to attach to; this
    // field is exposed to it via [postsDraftsSync] and closed here on teardown.
    private var postsDraftsSync: FfiDraftsSync? = null

    // Per-session events-rail draft autosync — the typed [FfiEventDraftsSync]
    // (events twin of [draftsSync]/[postsDraftsSync], which are the
    // rail-agnostic bytes shape). Built eagerly at connect and handed to
    // [eventDraftsHost], which restores + holds the live draft itself; closed
    // here on teardown.
    private var eventDraftsSync: com.fauna.ffi.FfiEventDraftsSync? = null

    // The feed / search / atproto-settings singletons live in [KidsExcisedApi]
    // (src/noKids), the superclass the `kids` build type replaces with an
    // inert twin; [clearAuth] tears them down through [detachKidsExcised].

    // The in-process file-sync engine host (libs/fauna-sync-engine's `EngineHost`,
    // over the UniFFI `FfiSyncEngineHost` face) — android's deployment of the same
    // shared engine linux/apple/windows already run. A connection-bound singleton
    // like [feedManager]: built lazily by [syncEngineHost] and held for the whole
    // session, because dropping it stops every resident + one-shot engine
    // (`sync_engine_host.rs:139-146`). Closed here on teardown.
    private var syncEngineHost: com.fauna.ffi.FfiSyncEngineHost? = null

    /** Off-main scope for the fire-and-forget WS teardown in [clearAuth]. */
    private val teardownScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    // -- Reconnect re-hydrate (transport.md § Push events) --
    //
    // Bumped once per WS reconnect-*after-first* (the initial connect never
    // bumps). Live surface VMs collect [reconnectTick] and re-pull their
    // snapshot surfaces: the feed has NO poll backstop, so a post that arrived
    // while the client was disconnected would otherwise stay invisible until a
    // manual refresh. `replay = 0` so a VM created *after* a reconnect doesn't
    // replay a stale bump (it loaded fresh on creation anyway). Android is the
    // last native fan-out of the shared `NestClient::subscribe_reconnects`
    // watch — mirrors linux `WsEvent::Reconnected`, the windows/apple pumps,
    // and the web `reconnectTick` store.
    private val _reconnectTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
    val reconnectTick: SharedFlow<Unit> = _reconnectTick.asSharedFlow()

    // -- Knock push re-hydrate (transport.md § Push events) --
    //
    // Bumped on every inbound `fauna.knock` push, so a MOUNTED contacts screen
    // grows the new knock row with no navigation — only [reconnectTick]'s sweep
    // recovered it before. Fed by [startKnockPump] off the shared
    // `NestClient::subscribe_knocks` broker via the dedicated `FfiKnockSubscription`
    // seam (distinct from the generic `subscribePushes`/`FfiPushEvent` dispatch,
    // which buckets `fauna.knock` as `Other` by design — a client drives one seam
    // or the other for knocks, never both). Mirrors the windows `StartKnockPump`/
    // `KnockReceived` pump; the payload feeds the OS knock toast
    // ([NotificationHelper.postKnockNotification]), the contacts screen only the
    // re-fetch tick. Same replay-0 idiom as [reconnectTick].
    private val _knockTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
    val knockTick: SharedFlow<Unit> = _knockTick.asSharedFlow()

    // -- Live push re-hydrate (transport.md § Push events) --
    //
    // Bumped on every inbound `fauna.notification` push, so a MOUNTED
    // notifications page grows the new row with no navigation and no poll (the
    // notifications surface has no poll backstop on any client — only a push can
    // satisfy it). Fed by [startPushPump] off the shared
    // `NestClient::subscribe_pushes` broadcast. Same `replay = 0` rationale as
    // [reconnectTick]: a VM created after a push loaded fresh on creation anyway.
    // A push is a HINT, never the only path to a value — the tick carries no
    // payload, and the collector re-fetches the authoritative rows.
    private val _notificationTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
    val notificationTick: SharedFlow<Unit> = _notificationTick.asSharedFlow()

    // Bumped on every inbound `fauna.calendar.changed` push — a durable write
    // landed in one of this actor's calendars (own other device, or an external
    // MUA via the MDA). The Events surface re-fetches on it; the push is a
    // nudge, the quick-appearance poll (once built on android) and the
    // reconnect re-pull remain the correctness backstop (transport.md § Push
    // events, ratified 2026-07-17). Same replay-0 tick idiom as above.
    private val _calendarChangedTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
    val calendarChangedTick: SharedFlow<Unit> = _calendarChangedTick.asSharedFlow()

    // Bumped on every inbound `fauna.sync.changed` push — a record landed in a
    // folder this actor participates in, own other device or a fellow
    // member (file-sync.md § Remote-change nudge). Twin of
    // [calendarChangedTick]: android has no resident local-mirror engine to
    // nudge (unlike linux/tui's `pull_set_now`/`PullFolderNow` — folder
    // content here is either machine-snapshot state or read-through, not a
    // local mirror), so every consumer just re-fetches its own machine
    // snapshot off this tick rather than pulling bytes. No per-set filtering
    // wired yet — every collector does a blanket re-fetch, matching
    // [calendarChangedTick]'s unfiltered shape.
    private val _folderChangedTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
    val folderChangedTick: SharedFlow<Unit> = _folderChangedTick.asSharedFlow()

    // Bumped by the shared store-change watch (`setStoreChangeListener`): the
    // account store may have changed — this app's own pump applied another
    // device's change, or another process committed. Every view model whose
    // load reads through the account store collects it and re-runs that load,
    // so an OPEN store-backed screen shows what a fresh visit would
    // (account-runtime.md § Multi-instance concurrency → *A runtime's own pump
    // is a source of the notice too*). Payload-free and a level, not an event:
    // a re-read paints only what differs and never discards a draft. A
    // gesture's own write does not fire it. Same replay-0 idiom as
    // [reconnectTick]: a VM created after a notice loaded fresh on creation.
    private val _storeChangedTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
    val storeChangedTick: SharedFlow<Unit> = _storeChangedTick.asSharedFlow()

    // Held for the process: the Rust slot keeps the registration across
    // sign-out and account switch, and the relay behind it ends with each
    // account runtime. Called on a Rust runtime thread — `tryEmit` is
    // thread-safe, and the collectors hop to their own scopes.
    private val storeChangeListener = object : com.fauna.ffi.FfiStoreChangeListener {
        override fun storeChanged() {
            _storeChangedTick.tryEmit(Unit)
        }
    }

    // Bumped on every inbound `fauna.addressbook.changed` push — a durable card
    // or book write landed in one of this actor's address books (own other
    // device, or an external contacts app via the MDA). ContactsVM re-reads the
    // books and the open book's cards on it, but only while the Address Book
    // segment is showing (`StaleSurfaces::address_book` is page-gated: a
    // contacts app's first sync is one push per card). Same replay-0 tick idiom.
    private val _addressBookChangedTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
    val addressBookChangedTick: SharedFlow<Unit> = _addressBookChangedTick.asSharedFlow()

    /** Long-lived scope hosting the reconnect-subscription pump. */
    internal val connectionScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var reconnectSub: FfiReconnectSubscription? = null
    private var reconnectJob: Job? = null
    private var pushSub: FfiPushSubscription? = null
    private var pushJob: Job? = null
    private var knockSub: FfiKnockSubscription? = null
    private var knockJob: Job? = null

    // -- Hands-off TLS-cert auto-renew cadence (tls-certificates.md § C.3 C2) --
    //
    // App-scoped background loop (NOT a per-VM one — AdminDnsVM dies with the
    // screen) that, every [uniffi.fauna_client_dns.autoRenewPollSecs], re-reads
    // the served-cert health and silently re-issues the certs the shared
    // decision flags at-risk ∧ auto-renew-on, so a synced admin device renews
    // with no admin tap. Native mirror of linux `run_auto_renew_cadence_tick`
    // (apps/fauna-linux/src/client.rs).
    internal var autoRenewJob: Job? = null

    // -- Connection-status indicator (global, top of shell) --
    //
    // Live nest WS-RPC connection state for the `connection-status` indicator.
    // Fed by [startConnectionStatePump] off the shared
    // `NestClient::connection_state()` watch (the UniFFI twin linux observes for
    // its top-of-sidebar indicator); the first `next()` seeds the current state,
    // then each transition updates it. Unlike [reconnectTick] (reconnect-only),
    // this reflects Connecting/Disconnected too, so a Watchtower-swap gap shows
    // as "Connecting…", never an error. Resets to Disconnected on [clearAuth].
    private val _connectionState = MutableStateFlow(FfiConnectionState.DISCONNECTED)
    val connectionState: StateFlow<FfiConnectionState> = _connectionState.asStateFlow()
    private var connectionStateSub: FfiConnectionStateSubscription? = null
    private var connectionStateJob: Job? = null

    // A post-auth verdict the signed-in session cannot survive, read off a
    // supervisor that stopped for good (`security.md` § Post-auth surfacing):
    // the nest identity changed, this identity was succeeded, or the nest
    // stopped signing it in — the last is the mid-session suspension
    // (`onboarding.md` § App-launch routing, the previously-signed-in row).
    // Emitted by [startConnectionStatePump] on the `Disconnected` that
    // announces the stop; the authenticated shell routes it to the launch
    // surface. No replay: a verdict belongs to the session it ended, and a
    // later session's shell must never re-escalate it.
    private val _sessionEnding =
        MutableSharedFlow<FfiSessionEndingVerdict>(replay = 0, extraBufferCapacity = 1)
    val sessionEnding: SharedFlow<FfiSessionEndingVerdict> = _sessionEnding.asSharedFlow()

    // The author-side encrypted-mode auto-approve reconcile loop (see
    // [startSubscriptionsAuthorPump]). Started on connect, canceled in clearAuth.
    internal var subscriptionsAuthorJob: Job? = null

    private val json = Json { ignoreUnknownKeys = true }
    private val octetMediaType = "application/octet-stream".toMediaType()
    private val cborMediaType = "application/cbor".toMediaType()

    // -- Auth --

    suspend fun authenticate(secretHex: String) {
        val mine = actorGeneration
        secret = secretHex
        val secretBytes = HexUtil.hexToBytes(secretHex)
        // Mint the bearer over the pre-identity WS-RPC silent challenge
        // (`fauna.auth.{challenge,verify}`) via the shared FFI `mintBearer`
        // (login.md § When to use which). `expiresAt` is absolute unix
        // **seconds** on this device's clock (anchored at receipt, login.md
        // § Token lifetime on the client's clock); refresh 60 s early,
        // matching the shared WsChallengeBearer pre-expiry buffer.
        val minted = bearerMinter(nodeUrl, secretBytes)
        // A clear during the mint: this is the outgoing actor's call. Writing
        // its token, or seating a client built from its secret, would hand the
        // incoming actor's connect a client that `ensureNestConnected`'s early
        // return reuses.
        ensureSameActor(mine)
        token = minted.token
        tokenExpiresAt = (minted.expiresAt.toLong() - 60) * 1000L

        ensureNestConnected(secretBytes, mine)
    }

    /** Ends an [authenticate] whose actor a [clearAuth] has retired since
     *  [mine] was captured. It ends as superseded rather than failed: the work
     *  belongs to an actor that is gone. */
    private fun ensureSameActor(mine: Long) {
        if (actorGeneration != mine) {
            throw kotlinx.coroutines.CancellationException("actor changed while authenticating")
        }
    }

    /**
     * Await the shared account-runtime stop for the account THIS client is
     * still serving, under `stop_account_runtime`'s own bounded budget
     * (`ACCOUNT_RUNTIME_STOP_BUDGET`, 5 s — `account-scoping.md` § Erasure
     * follows scope, "that call before its `account_state_erase_*`").
     *
     * Call this BEFORE [clearAuth] (which nulls [nestClient]) and before any
     * erase that follows sign-out. [clearAuth]'s own fire-and-forget teardown
     * calls `stopAccountRuntime()` again for every OTHER path (switch, the
     * post-auth re-entry) where nothing downstream waits on it — idempotent
     * either way, since the shared Rust takes the settled handle, the
     * assembly still in flight, or both, and no-ops once neither exists.
     * A timeout or any other failure here is not this caller's to report:
     * `stop_account_runtime` owns its own loud (`tracing::warn!`) line when
     * the budget lapses, and the erase proceeds regardless either way.
     */
    suspend fun stopAccountRuntimeAwaited() {
        try {
            nestClient?.stopAccountRuntime()
        } catch (_: Exception) {
        }
    }

    /**
     * The sign-out-shaped twin of [stopAccountRuntimeAwaited] — call in its
     * place wherever the erase that follows takes this machine's
     * account-store slot (the writer key) with it: retires this machine's
     * enrollment nest-side FIRST, while the runtime still holds the writer
     * key, then runs the same local teardown
     * (`sync-agent-credentials.md` § Credential model → *The signed-out
     * reconcile*). A plain switch, or a reset/logout that leaves the slot in
     * place, keeps [stopAccountRuntimeAwaited] instead — retiring the
     * enrollment there would be wrong. Mirrors apple's
     * `APIClient.stopAccountRuntimeForSignOut()` / windows'
     * `NestRpcClient.StopAccountRuntimeForSignOutAsync()`.
     */
    suspend fun stopAccountRuntimeForSignOutAwaited() {
        try {
            nestClient?.stopAccountRuntimeForSignOut()
        } catch (_: Exception) {
        }
    }

    fun clearAuth() {
        actorGeneration += 1
        token = null
        secret = null
        tokenExpiresAt = 0
        nodeUrl = ""

        // Identity is changing (sign-out, account switch, factory reset) — an
        // alert keyed to the departing identity's DID must not outlive it, or
        // the banner accuses the incoming account (critical-alerts.md §
        // Mechanism → Lifetime). Every android identity teardown routes
        // through this one function, so one call here covers all of them
        // (unlike linux's six separate teardown paths).
        criticalAlertsHost.clearAll()
        // The ward's own asks and refusals are one account's (family-safety.md
        // § Child-initiated contact requests / § Feed-source approvals) — an
        // incoming account must not inherit "asked — waiting".
        wardAsks.clear()
        // The same boundary for the ceremony seat: its listener answers as the
        // departing actor, and an act in flight writes that actor's config.
        offlineShareHost.reset()

        reconnectJob?.cancel()
        reconnectJob = null
        val reconnect = reconnectSub
        reconnectSub = null

        pushJob?.cancel()
        pushJob = null
        val push = pushSub
        pushSub = null

        knockJob?.cancel()
        knockJob = null
        val knock = knockSub
        knockSub = null

        autoRenewJob?.cancel()
        autoRenewJob = null

        connectionStateJob?.cancel()
        connectionStateJob = null
        val connState = connectionStateSub
        connectionStateSub = null
        _connectionState.value = FfiConnectionState.DISCONNECTED

        subscriptionsAuthorJob?.cancel()
        subscriptionsAuthorJob = null

        val client = nestClient
        val bridges = bridgesClient
        val email = emailClient
        val admin = adminClient
        val feed = feedClient
        val posts = postsClient
        val contacts = contactsClient
        val notifications = notificationsClient
        val inbox = inboxClient
        val account = accountClient
        val spam = spamClient
        val conversations = conversationsClient
        val sync = syncClient
        val moderation = moderationClient
        val snapshots = snapshotsClient
        val caldav = caldavClient
        val carddav = carddavClient
        val bluesky = blueskyClient
        val drafts = draftsSync
        val postsDrafts = postsDraftsSync
        val eventDrafts = eventDraftsSync
        // Retire the draft autosave before tearing the handle down (the host
        // cancels its debounce; the FfiDraftsSync is closed below).
        conversationsManagerHost.stopDraftsSync()
        eventDraftsHost.stopDraftsSync()
        // Retire the conversations receive session: cancel the loop-launch
        // coroutine + close the session object. The Rust receive loop then exits
        // on the session-closed SIGNAL (`ConversationsSession::closed()`, shared
        // Rust) once `client.close()` (below) drops the last session Arc the
        // factory stashed in the nest client — not at a later liveness tick,
        // which is what it used to wait for until the 2026-08-27 fix
        // (`account-scoping.md`, the `tui (in-memory)` ledger row).
        conversationsManagerHost.stopConversationsSession()
        // The client-level twin of the manager-level retire just above
        // (`account-scoping.md` § Erasure follows scope, the ⚠ *An OPEN store
        // is an unerasable store* note) — refcount-independent, so it closes
        // `mls.db` / releases `mls.db.lock` however many `Arc`s this object
        // graph still holds, mirroring windows/apple calling both seams.
        // Idempotent (a second call finds the session already taken), so
        // covering every `clearAuth()` caller here costs nothing extra.
        client?.releaseAccountScopedStores()
        val closeKidsExcised = detachKidsExcised()
        val syncHost = syncEngineHost
        syncEngineHost = null
        nestClient = null
        bridgesClient = null
        emailClient = null
        adminClient = null
        feedClient = null
        postsClient = null
        contactsClient = null
        notificationsClient = null
        inboxClient = null
        accountClient = null
        spamClient = null
        conversationsClient = null
        syncClient = null
        moderationClient = null
        snapshotsClient = null
        caldavClient = null
        carddavClient = null
        blueskyClient = null
        featuresClient = null
        draftsSync = null
        postsDraftsSync = null
        eventDraftsSync = null
        if (client != null) {
            teardownScope.launch {
                // Stop hosting the account plane as the DEPARTING account before
                // anything else is torn down (`account-data-plane.md` § The
                // account store → *The client-side lifecycle*). `clearAuth` is
                // android's one identity-teardown funnel — sign-out, account
                // switch and factory reset all route through it — which is
                // exactly the set the shared teardown is for, and it is exactly
                // the set a plain process end is NOT in: on a quit the process
                // ends, the handle drops, and nobody has to take the role over
                // (android hosts no resident agent). Awaits the pump's in-flight
                // pass, so by the time the close below runs nothing is still
                // writing as the old account — the half-drained-outbox window a
                // mid-pass switch would otherwise leave. Idempotent and safe
                // with no runtime installed, so the pre-auth teardown path costs
                // nothing.
                try { client.stopAccountRuntime() } catch (_: Exception) {}
                try { client.disconnect() } catch (_: Exception) {}
                bridges?.close()
                email?.close()
                admin?.close()
                feed?.close()
                posts?.close()
                contacts?.close()
                notifications?.close()
                inbox?.close()
                account?.close()
                spam?.close()
                conversations?.close()
                sync?.close()
                moderation?.close()
                snapshots?.close()
                caldav?.close()
                carddav?.close()
                bluesky?.close()
                drafts?.close()
                postsDrafts?.close()
                // The events rail's handle, alongside its two siblings. Nulling
                // the field alone left the Rust side to a GC-timed `Cleaner`:
                // `FfiEventDraftsSync` holds a `DraftsClient { nest, key }`, so
                // the DEPARTING identity's drafts `BackupKey` and a strong
                // `Arc<NestClient>` (whose `AuthClient` holds the actor keypair)
                // stayed resident for an unbounded window while the next actor
                // was signed in.
                eventDrafts?.close()
                reconnect?.close()
                push?.close()
                knock?.close()
                connState?.close()
                closeKidsExcised()
                syncHost?.close()
                client.close()
            }
        }
    }

    /**
     * Build and connect the WS-RPC client for this actor/nest, once per
     * session. FfiNestClient handles the https→wss scheme conversion and the
     * /api/v1/ws path internally, so it takes the plain [nodeUrl]. A connect
     * failure is logged but does not abort HTTP auth — the reconnect
     * supervisor keeps retrying, and the first bridges/email RPC surfaces a
     * clear error if the nest is still unreachable.
     *
     * Every client this builds ends seated in [nestClient], and so inside
     * [clearAuth]'s teardown (`disconnect()` + `close()`), never merely
     * dropped. [mine] was checked after the mint, and nothing suspends between
     * that check, the early return and the seat write, so no client is built
     * for a stale generation. The early return means a concurrent call for
     * this same actor already filled the seat. A different actor's client
     * cannot be there: every identity change runs
     * [ActorScope.dropActorScopedState] → [clearAuth] before the next actor
     * connects, and the generation check stops a stale call from seating one
     * afterwards. So there is never a losing client to disconnect.
     */
    private suspend fun ensureNestConnected(secretBytes: ByteArray, mine: Long) {
        if (nestClient != null) return
        val client = nestClientFactory(nodeUrl, secretBytes)
        nestClient = client
        bridgesClient = client.bridges()
        emailClient = client.email()
        adminClient = client.admin()
        feedClient = client.feed()
        postsClient = client.posts()
        contactsClient = client.contacts()
        notificationsClient = client.notifications()
        inboxClient = client.inbox()
        accountClient = client.account()
        spamClient = client.spam()
        conversationsClient = client.conversations()
        syncClient = client.sync()
        moderationClient = client.moderation()
        snapshotsClient = client.snapshots()
        caldavClient = client.caldav()
        carddavClient = client.carddav()
        blueskyClient = client.bluesky()
        featuresClient = client.features()
        // Build the shared conversations receive session over this connection and
        // start its inbound-mail receive loop (mail-spam.md § Impl item 6;
        // conversations.md § Receiving into the conversations view). Placed BEFORE
        // the drafts sync so the drafts restore below targets the session's
        // manager. `selfAddress` = bare handle @ cached identity domain — bare via
        // substringBefore since the cached handle is sometimes @-qualified
        // (MailSettingsVM.bareHandle precedent); an empty address is tolerated (the
        // receive loop is connection-scoped, only the SMTP From/self-domain leans
        // on it). `mlsDbPath` is a DISTINCT file, never MlsManager's fauna-mls.db.
        val bareHandle = sessionAccount.handle.orEmpty().substringBefore("@")
        val selfDomain = sessionAccount.domain.orEmpty()
        val selfAddress = if (bareHandle.isNotBlank() && selfDomain.isNotBlank()) {
            "$bareHandle@$selfDomain"
        } else {
            ""
        }
        val mlsDbPath = accountStores.conversationsMlsDbPath()
        // The actor whose predecessor keys feed the __mls re-seal must be
        // THIS session's own actor — never accountStores.activeActorHex(),
        // which can disagree during an append-mode sign-in or a switch race. predecessorBackupKeys() resolves off
        // sessionActorHex(), the same secretBytes this connection
        // authenticates as — shared with [buildMediaMachine] so the two
        // consumers never see two independent walks disagree.
        val predecessorKeys = predecessorBackupKeys()
        conversationsManagerHost.startConversationsSession(
            client,
            selfAddress,
            secretBytes,
            mlsDbPath,
            predecessorKeys,
            // The recording device — the same id `serveSetFolder` passes its
            // walk — so the launch resume finishes an interrupted served-set
            // re-seal (`webdav-server.md` § Key model (c)).
            sessionAccount.deviceId,
        )
        // Draft-persistence v2 (file-sync.md § Drafts Sync): build the
        // conversations-rail drafts sync over this connection and hand it to the
        // manager host, which restores the owner's persisted drafts on launch and
        // autosaves compose edits. Independent of the (not-yet-wired) send/receive
        // backends — it only round-trips the manager's draft snapshot bytes.
        val drafts = client.draftsSync("conversations")
        draftsSync = drafts
        conversationsManagerHost.startDraftsSync(drafts)
        // Draft-persistence v2, posts rail (file-sync.md § Drafts Sync,
        // docs/goal/ui/feed.md § Persistence): build the feed-composer drafts
        // sync over this connection too. Unlike the conversations handle above,
        // no restore happens here — the FfiFeedManager is built lazily on first
        // Feed-page access (see [feedManager] below), so [FeedManagerHost]
        // fetches this handle via [postsDraftsSync] and drives the restore +
        // autosave once a manager exists to attach it to.
        postsDraftsSync = client.draftsSync("posts")
        // Draft-persistence v2, events rail (file-sync.md § Drafts Sync,
        // docs/goal/ui/events.md § Persistence): the third and last rail.
        // Typed, unlike the two above — the Events page has no manager to
        // decode/encode the record, so [eventDraftsHost] holds it directly
        // and restores on launch here, same trigger point as conversations.
        val eventsDrafts = client.eventDrafts()
        eventDraftsSync = eventsDrafts
        eventDraftsHost.startDraftsSync(eventsDrafts)
        try {
            client.connect()
        } catch (e: Exception) {
            android.util.Log.w("ApiClient", "WS-RPC connect failed: ${e.message}")
            ShellLog.w("ApiClient", "WS-RPC connect failed: ${e.message}")
        }
        // A clear during connect() already took this client into its teardown
        // when it emptied the seat. Starting the pumps or the account runtime
        // now would run them on a closed client as the departing actor. From
        // here to the runtime start nothing suspends, so this one check covers
        // them all.
        ensureSameActor(mine)
        startReconnectPump(client)
        startPushPump(client)
        startKnockPump(client)
        startConnectionStatePump(client)
        startAutoRenewCadence()
        startSubscriptionsAuthorPump(client, secretBytes)
        startAccountRuntime(client)
    }

    /**
     * Host the W3 (account-data-plane.md § Workstreams) account-store runtime in this process
     * (`account-data-plane.md` § The account store → *The client-side
     * lifecycle*). Android is the **iOS**-shaped
     * host: `apps/sync-agent.md` § Scope per platform lists android among the
     * targets with no resident agent process, so an in-process host is the
     * only host the account plane gets here — there is no app-dead backstop to
     * fall back on and no W5.1 peer to lose the election to.
     *
     * **Placed here, not in [ConversationsManagerHost.startConversationsSession].**
     * That function returns early under `TestAgent.isE2EActive &&
     * !isRealConversationsActive`, and an app with no conversations rail still
     * hosts a perfectly correct runtime (`memberships: None` is a *supported*
     * wiring, not a degraded one — the shared module's own header says so).
     * There is deliberately no ordering contract between the two calls: the
     * membership source re-reads the client's stashed session on every pump
     * pass, so starting either first is correct.
     *
     * **The three inputs, and why each is that value:**
     * - `appDataDir` = `filesDir` — **unread** by the assembly now (it rooted
     *   the retired device-local `__config` replica; see
     *   `docs/goal/architecture/config-dissolution.md`). The store root is a
     *   sibling of this dir, never a child.
     * - `storeContainerDir` = [AccountStores.accountStoreContainerDir] —
     *   MANDATORY on this target. `StoreRoot::platform()` takes the generic
     *   unix branch on android and resolves `$HOME/.config/fauna/sync`, which
     *   inside the sandbox is unwritable, and the assembly is best-effort by
     *   construction, so it would degrade to "no runtime" **silently**. The
     *   same value must reach both erases, which is why it has one accessor.
     * - `ownDeviceId` = this install's stable sync device id, the same 32 bytes
     *   `syncEngineHost`/`resolvePhotoLibrarySet` seat. Deliberately NOT
     *   `null`-by-default the way `indexLeaseDevice` is: that `null` is
     *   ratified because a phone hosts no content index, and that reason does
     *   not transfer to the account plane. A malformed/wrong-length id FAILS
     *   the call by design (`nest_client.rs`, the same ruling as
     *   `index_lease_device`) — the catch below logs it instead of hiding it.
     *   A `null` id fails the start too: the id is the machine's named row,
     *   the enrollment's one target (`sync-agent-credentials.md`
     *   § Credential model → the RULED 2026-09-28 block, decision 3).
     * - `accounts` = the account registry itself
     *   ([AccountStores.accountRegistry]). Shared Rust resolves this session's
     *   own actor's **attested** succeeded-from identities off it — never the
     *   active account, which can disagree during an append-mode sign-in or a
     *   switch race: their ids are the fleet view's `prior`
     *   (`account-data-taxonomy.md` § The generation machinery → *The source
     *   of `prior`*, ruled 2026-09-13), and their key schedules are what a
     *   predecessor's preference rows are carried under. Nothing to resolve
     *   for an identity that never succeeded, which is fail-safe.
     *
     * Best-effort like every other post-auth hook here: a failure leaves every
     * preference surface on the blob rail exactly as before this existed, and
     * never fails a sign-in.
     */
    private suspend fun startAccountRuntime(client: FfiNestClient) {
        try {
            val ownDeviceId = sessionAccount.deviceId?.let { HexUtil.hexToBytes(it) }
            // Before the start, so the runtime's first changed run is heard.
            // Idempotent: a re-register replaces the same listener.
            com.fauna.ffi.setStoreChangeListener(storeChangeListener)
            // The container crosses PAIRED with how it stays out of Google
            // device backup — android's manifest arm, the same statement the
            // custodian host makes (`CloudBackupPosture`). The account store's
            // writer key sits in `EncryptedSharedPreferences` under
            // `allowBackup="false"`; its dir must be under the same rule, or a
            // restore would bring back a store whose writer key did not travel
            // (`common.md` § Credential storage → *The shared Rust credential
            // slots on the phones*).
            client.startAccountRuntime(
                context.filesDir.absolutePath,
                com.fauna.ffi.FfiStoreContainer(
                    dir = accountStores.accountStoreContainerDir(),
                    exclusion = CloudBackupPosture.exclusion(),
                ),
                ownDeviceId,
                accountStores.accountRegistry,
            )
        } catch (e: Exception) {
            android.util.Log.w("ApiClient", "account runtime start failed: ${e.message}")
            ShellLog.w("ApiClient", "account runtime start failed: ${e.message}")
        }
    }

    /**
     * The `account_pump_cycles` e2e state value, or `null` when this app has no
     * connected client to ask (which the serializer reports as the key being
     * absent — see [com.fauna.app.testing.TestAgent]).
     *
     * A plain atomic read of two counters plus two booleans, which is why it is
     * on the sync half of the FFI surface: convention 11's corollary forbids
     * blocking I/O on the state path. Reached through this accessor rather than
     * the private [nestClient] for the same reason every other TestAgent read
     * is — the FFI handles stay owned by this class.
     *
     * Deliberately NOT `test-helpers`-gated: the shared method rides the
     * `account-runtime` feature, exactly as `conv_receive_cycles_json` rides
     * the conversations session, so all three android flavors publish it.
     */
    fun accountPumpCyclesJson(): String? = nestClient?.accountPumpCyclesJson()

    /**
     * The `account_pump_now` poke — one full account-pump pass now, convention
     * 14's `run_now` for the account plane. `false` when there is no connected
     * client or no assembled runtime yet (pre-auth): a legitimate quiet no-op,
     * honoured rather than dropped, with the consumer's own deadline poll on
     * `account_pump_cycles` as the barrier that fails and names the app.
     */
    suspend fun accountPumpNow(): Boolean = nestClient?.accountPumpNow() ?: false

    /**
     * The Devices page's standing enrollment-refusal notice
     * (`docs/goal/ui/devices.md` § Errors & edge cases; today's one refusal is
     * the tier device cap, `devices.md` § Step 4) — `Some(sentence)` while the
     * nest refuses to enroll this machine, `null` otherwise or with no
     * connected client. A local slot read off the shared account runtime,
     * never a network call — same shape as [accountPumpCyclesJson] above.
     */
    suspend fun accountEnrollmentNotice(): String? = nestClient?.accountEnrollmentNotice()

    /**
     * Pump the shared reconnect signal onto [reconnectTick]. `subscribeReconnects`
     * yields the UniFFI twin of consuming the `NestClient::subscribe_reconnects`
     * watch directly (as the Rust-native Linux app does); each `next()`
     * resolves on a `Connected` transition AFTER the first connect (the initial
     * connect never bumps), and returns null once the client tears down so the
     * loop terminates. Single-consumer, so the one task here serializes `next()`.
     * Mirrors the windows/apple pumps (transport.md § Push events).
     */
    private fun startReconnectPump(client: FfiNestClient) {
        val sub = client.subscribeReconnects()
        reconnectSub = sub
        reconnectJob = connectionScope.launch {
            while (true) {
                val bump = try { sub.next() } catch (_: Exception) { null } ?: break
                _reconnectTick.tryEmit(Unit)
            }
        }
    }

    /**
     * Pump the dedicated `fauna.knock` seam onto [knockTick]. `subscribeKnocks`
     * yields the UniFFI twin of consuming the shared `NestClient` knock broker
     * directly (windows' `StartKnockPump` is the reference); each `next()` resolves
     * the decoded knock (its OS toast is raised off it; [ContactsVM] re-fetches the
     * authoritative rows on the tick), and returns null once the client tears down so the loop
     * terminates. Single-consumer, so the one task here serializes `next()`.
     */
    private fun startKnockPump(client: FfiNestClient) {
        val sub = client.subscribeKnocks()
        knockSub = sub
        knockJob = connectionScope.launch {
            while (true) {
                val knock = try { sub.next() } catch (_: Exception) { null } ?: break
                notificationHelper.postKnockNotification(knock)
                _knockTick.tryEmit(Unit)
            }
        }
    }

    /**
     * Android's central push dispatch — the Kotlin twin of linux's
     * `app.rs` `WsEvent::Push(e) => match e { … }` and the web SPA's
     * `rpc.ts` `onPushEvent` fan-out (transport.md § Push events and `seq`
     * numbering). `subscribePushes` yields every decoded push on the ONE
     * authenticated socket; `next()` returns null once the client tears down, so
     * the loop terminates. Single-consumer, so the one task here serializes
     * `next()` — every arm must therefore stay cheap and non-blocking (each just
     * bumps a tick a VM re-fetches on).
     *
     * Which surfaces are stale is derived from [staleSurfacesForPushEvent]
     * (`fauna_protocol::PushEvent::invalidates`, `transport.md` § Which
     * surfaces a push invalidates) rather than a hand-matched `when` over
     * [FfiPushEvent] — this file no longer needs its own copy of the
     * kind→surface table, so a kind the shared seam later grows a new surface
     * for reaches android with no edit here.
     */
    private fun startPushPump(client: FfiNestClient) {
        val sub = client.subscribePushes()
        pushSub = sub
        pushJob = connectionScope.launch {
            while (true) {
                val event = try { sub.next() } catch (_: Exception) { null } ?: break
                val stale = staleSurfacesForPushEvent(event)
                if (stale.notifications) {
                    // Mirrors linux: `notify_unified(notif_type, summary)` +
                    // `fetch_notifications()`. The OS toast is a follow-on leg
                    // (NotificationHelper); the re-fetch is what makes a mounted
                    // page grow the row, and is what the cross-app e2e asserts.
                    _notificationTick.tryEmit(Unit)
                }
                if (stale.events) {
                    // A durable write landed in one of this actor's calendars.
                    // EventsVM re-fetches on the tick — the android leg of the
                    // calendar-change push (transport.md § Push events).
                    _calendarChangedTick.tryEmit(Unit)
                }
                if (stale.addressBook) {
                    // The android leg of the Address Book's live refresh
                    // (transport.md § Push events); ContactsVM page-gates it.
                    _addressBookChangedTick.tryEmit(Unit)
                }
                if (stale.media) {
                    // A record landed in a folder this actor participates in.
                    // DevicesVM (folders listing/membership) and MediaVM
                    // (cross-set content) re-fetch on the tick — the android leg
                    // of the remote-change nudge (file-sync.md § Remote-change
                    // nudge). `event.folder` is not threaded through: neither
                    // collector filters by set yet, matching CalendarChanged's
                    // unfiltered shape above.
                    _folderChangedTick.tryEmit(Unit)
                }
                if (stale.knocks || stale.contacts || stale.account || stale.atproto) {
                    // Surfaces with no dedicated push tick on android — knocks
                    // rides its own `FfiKnockSubscription` seam (never `Other`
                    // here in practice, since a `fauna.knock` push arrives on
                    // THAT subscription instead); contacts/account/bluesky have
                    // no push-specific tick at all yet. [reconnectTick]'s
                    // collectors (knocks/contacts/notifications/account/events/
                    // media, plus the feed — EventsVM/MediaVM/DevicesVM joined
                    // this fan-out adopting the shared classifier, closing a
                    // real gap: neither had a reconnect arm before) are the
                    // only recovery path for these today, so reuse it rather
                    // than mint a fifth tick. ⚠ No bluesky VM subscribes to
                    // reconnectTick yet — a bluesky-classified kind here is a
                    // documented no-op until one does (a separate,
                    // pre-existing absence, not something this adoption
                    // introduced).
                    _reconnectTick.tryEmit(Unit)
                }
                // `stale.feed` is never true here — no push feeds the feed
                // (`StaleSurfaces::feed`'s own doc); only a reconnect does,
                // via [startReconnectPump] below, never this loop.
            }
        }
    }

    /**
     * Pump the shared connection-state watch onto [connectionState], driving the
     * global `connection-status` indicator. `subscribeConnectionState` is the
     * UniFFI twin of observing `NestClient::connection_state()` directly (as the
     * Rust-native Linux app does for its top-of-sidebar indicator); the first
     * `next()` yields the *current* state, then each transition
     * (Connecting/Connected/Disconnected), and null once the client tears down so
     * the loop terminates. Single-consumer, so the one task here serializes
     * `next()`. Mirrors [startReconnectPump] (transport.md § Connection lifecycle).
     */
    private fun startConnectionStatePump(client: FfiNestClient) {
        val sub = client.subscribeConnectionState()
        connectionStateSub = sub
        connectionStateJob = connectionScope.launch {
            while (true) {
                val state = try { sub.next() } catch (_: Exception) { null } ?: break
                _connectionState.value = state
                // Every value the indicator receives, repeats included — the
                // StateFlow above conflates a repeat away, and the repeat is
                // the stickiness proof (`connection_reports`; a no-op in every
                // shipping flavor).
                com.fauna.app.testing.TestAgent.observeConnectionReport(state)
                // A `Disconnected` is also where a supervisor that stopped for
                // good says why — the stop is recorded before the state is
                // announced. A session-ending verdict ends this session: hand it
                // to the shell, which re-enters launch. The shared
                // `session_ending_verdict` classifies it, so no wire code is
                // matched here; tui's, linux's and apple's pumps are the model.
                if (state == FfiConnectionState.DISCONNECTED) {
                    val verdict = client.sessionEndingVerdict()
                    if (verdict != null) {
                        _sessionEnding.tryEmit(verdict)
                        break
                    }
                }
            }
        }
    }

    /**
     * Resolve the connected nest's own 32-byte identity — the `target_nest_id`
     * every DNS cert-issuance path feeds `DnsAction.IssueCert` /
     * `BeginManualIssueCert` (tls-certificates.md § C.3 D7: "the home nest the
     * cert serves"; the single-nest case is the connected nest itself). Goes
     * through the shared `fauna-client-pair` `thisNest()` (`fauna.nest.info` over
     * the authed WS-RPC connection), exactly as the linux issuance glue +
     * auto-renew cadence resolve it (apps/fauna-linux/src/client.rs
     * `resolve_this_nest_id`) and the windows/macos/ios fan-out. `null` (caller
     * gives up gracefully) when not connected or the call fails.
     */
    suspend fun resolveThisNestId(): ByteArray? =
        try {
            buildLinkedNestsMachine()?.thisNest()?.id
        } catch (e: Exception) {
            ShellLog.w("ApiClient", "resolve this-nest id: ${e.message}")
            null
        }

    /**
     * Build the user-settings Nests machine over the live WS-RPC connection
     * (`fauna.pair.{list,add,revoke}` — User-gated, owner-scoped, so the
     * connection actor scopes every pairing call). Built **with both the mail
     * relay-provisioning post-link hook AND the Nests-page trust facet**
     * (`docs/goal/ui/nests.md` § Where logic lives): a both-ends `LinkBoth`
     * auto-provisions the just-linked home box's mailbox reusing the fleet MSEK
     * (the home-with-public-relay one-action flow,
     * deployment-home-with-public-relay.md § Pairing), and the machine hydrates
     * the connected nest's trust facet + drives Mint/Renew/Revoke/SetLens. Both
     * need the actor secret (the same bytes [buildMailSettingsMachine] threads);
     * falls back to the plain hook-less/trust-less machine when the secret is
     * absent or keypair derivation fails, so list/link/unlink still work and
     * only the mailbox auto-provision + trust facet are skipped. Mirrors the
     * Linux lead (apps/fauna-linux/src/settings/linked_nests.rs
     * `wire_machine`). Returns null when not yet connected to a nest; the
     * caller renders an empty page and the machine hydrates once the socket is
     * up. Each call builds a fresh machine bound to the current connection.
     */
    fun buildLinkedNestsMachine(): uniffi.fauna_client_pair.LinkedNestsMachine? {
        val client = nestClient ?: return null
        val secretBytes = secret?.let { HexUtil.hexToBytes(it) }
        if (secretBytes != null) {
            linkedNestsMachineWithMailRelay(client, secretBytes)?.let { return it }
        }
        return com.fauna.ffi.buildLinkedNestsMachine(client)
    }

    /**
     * Build the page-level Devices state machine over the live WS-RPC connection
     * (`fauna.sync.devices.*` / `fauna.folders.*` / `fauna.sync.conflicts.*`,
     * plus the embedded folder creation wizard's
     * `fauna.folders.{create,places.set}`). `observer` ticks on every snapshot
     * change. Returns null when not yet connected to a nest; the caller renders an
     * empty page and rebuilds once the socket is up (mirrors
     * [buildLinkedNestsMachine] / the Linux app's hydrate-retry). Each call
     * builds a fresh machine bound to the current connection.
     */
    fun buildDevicesMachine(
        observer: uniffi.fauna_devices_machine.DevicesObserver,
    ): uniffi.fauna_devices_machine.DevicesMachine? =
        nestClient?.let { com.fauna.ffi.buildDevicesMachine(it, observer) }

    /**
     * Build the page-level Backups state machine (the snapshot half) over the
     * live WS-RPC connection — `fauna.folders.list` for the selector plus
     * `fauna.filesync.snapshot.{list,create_folder,delete,delete_immediate,
     * prune_set_policy,check,get}` for the list and its gestures. `observer`
     * ticks on every snapshot change; the screen renders off `snapshot()` and
     * holds no page logic (`ui/backups.md` § Snapshot-list shape, Architectural
     * rule 1). Returns null until connected to a nest (mirrors
     * [buildDevicesMachine] / [buildMediaMachine]).
     *
     * `deviceIdHex` is this shell's stable sync device id, so manual snapshots
     * carry row provenance (the *Create* ruling's `device_id`). Null ⇒
     * unattributed, which is exactly what the wire's `Option` means — never a
     * fabricated one; an unparseable value is cleared by the shared setter for
     * the same reason.
     */
    fun buildBackupsMachine(
        observer: uniffi.fauna_backups_machine.BackupsObserver,
        deviceIdHex: String?,
    ): uniffi.fauna_backups_machine.BackupsMachine? =
        nestClient?.let { client ->
            com.fauna.ffi.buildBackupsMachine(client, observer).also { machine ->
                deviceIdHex?.let { com.fauna.ffi.backupsMachineSetDeviceId(machine, it) }
            }
        }

    /**
     * Build the page-level Media state machine over the live WS-RPC connection —
     * the content-plane analogue of [buildDevicesMachine]. It owns the cross-set
     * all-media read (`fauna.media.list` via `refresh()`), the client-held view
     * state (`media-view-toggle`/`media-sort-select`/`media-folder-filter`), and
     * the shared upload / thumbnail-fetch gestures; the page renders off its
     * `snapshot()` (`docs/goal/ui/media.md` § Where logic lives — Media reads file
     * sets, never configures them). `observer` ticks on every snapshot change.
     * Returns null until connected to a nest (the page renders empty + rebuilds on
     * the next gesture, mirroring [buildDevicesMachine]).
     */
    fun buildMediaMachine(
        observer: uniffi.fauna_media_machine.MediaObserver,
    ): uniffi.fauna_media_machine.MediaMachine? =
        nestClient?.let { com.fauna.ffi.buildMediaMachine(it, observer) }

    /**
     * The owner's 32-byte `BackupKey` (`backup_key_derive` from the identity
     * secret) — the Library-audience seal key the Media upload gesture seals a
     * new blob under and the thumbnail-fetch decrypts with
     * (`docs/goal/ui/media.md` § Encryption at rest). Returns null when there is
     * no identity / secret yet; a malformed secret degrades to null rather than
     * throwing (the caller then declines the upload).
     */
    fun ownerBackupKey(): ByteArray? =
        secret?.let { runCatching { com.fauna.ffi.backupKeyDerive(HexUtil.hexToBytes(it)) }.getOrNull() }

    /**
     * The session's raw 32-byte identity secret, for the shared
     * `MediaMachine::set_share_author` seam only — the share-link author signs
     * the token and derives the filename-seal root from it inside shared Rust
     * (`docs/goal/behavior/share-links.md` § Where logic lives; apple's
     * `APIClient.identitySecretBytes` is the twin). Null with no identity yet.
     */
    fun identitySecretBytes(): ByteArray? =
        secret?.let { runCatching { HexUtil.hexToBytes(it) }.getOrNull() }

    /**
     * This session's retired owner `BackupKey`s off its succession chain
     * (`AccountStores.predecessorBackupKeys`), empty for an identity that
     * never succeeded (`sync-agent.md` § Credential model → *Retired owner
     * keys after an identity succession*) — the same resolve
     * [ensureNestConnected]'s `__mls` re-seal already uses, off
     * [sessionActorHex] and never `accountStores.activeActorHex()`, which a
     * mid-switch transient can move first.
     */
    fun predecessorBackupKeys(): List<ByteArray> =
        sessionActorHex()?.let { accountStores.predecessorBackupKeys(it) } ?: emptyList()

    /**
     * This session's attested predecessor ids (`AccountStores.attestedPredecessorActorIds`),
     * empty for an identity that never succeeded — off [sessionActorHex], never
     * the active pointer, like [predecessorBackupKeys].
     */
    fun attestedPredecessorActorIds(): List<ByteArray> =
        sessionActorHex()?.let { accountStores.attestedPredecessorActorIds(it) } ?: emptyList()

    /**
     * This session's predecessors paired with their retired keys
     * (`AccountStores.predecessorChain`) — what the Media machine's
     * `setPredecessorChain` takes, empty for an identity that never succeeded.
     */
    fun predecessorChain(): com.fauna.ffi.FfiPredecessorChain =
        sessionActorHex()?.let { accountStores.predecessorChain(it) }
            ?: com.fauna.ffi.FfiPredecessorChain(emptyList(), emptyList())

    /**
     * The in-process file-sync engine host for this actor
     * (`libs/fauna-ffi/src/nest_client.rs:460`) — the byte-sync deployment
     * `file-sync.md` § Apple apps — convergence design calls "the Linux
     * shape" (nothing apple-specific despite the module name); android is a
     * fourth deployment of the same shared `EngineHost`. [PhotoBackupEngine]'s
     * MediaStore ingress and [WatchedDirectoryManager]'s SAF ingress both stage
     * a file to a temp path and call `ingestFile` on the returned handle.
     *
     * Unlike [buildMediaMachine]/[buildDevicesMachine] (fresh per call), this
     * is built **once per session and held**: dropping the handle stops every
     * resident + one-shot engine (`sync_engine_host.rs:139-146`), so the first
     * caller wins and every later caller shares the same handle. Closed in
     * [clearAuth]. [deviceIdHex] is hex-encoded exactly as
     * [buildTaskDelegationView] takes it; [stateDir] is where
     * `folder-map.json` / `photo-ingress.json` persist (the app files dir).
     * Returns null when not yet connected or the secret is absent — a
     * malformed secret/device id or a build failure also degrades to null
     * (mirrors [ownerBackupKey]'s null-degradation: the caller then declines
     * the sync pass rather than crashing).
     */
    fun syncEngineHost(deviceIdHex: String, stateDir: String): com.fauna.ffi.FfiSyncEngineHost? {
        syncEngineHost?.let { return it }
        val client = nestClient ?: return null
        val secretBytes = secret?.let { HexUtil.hexToBytes(it) } ?: return null
        val deviceIdBytes = runCatching { HexUtil.hexToBytes(deviceIdHex) }.getOrNull() ?: return null
        val host = runCatching {
            client.syncEngineHost(secretBytes, deviceIdBytes, stateDir, "fauna-android")
        }.onFailure { ShellLog.w("ApiClient", "syncEngineHost build failed: ${it.message}") }.getOrNull()
            ?: return null
        syncEngineHost = host
        return host
    }

    /**
     * Resolve (creating if needed) the folder this device's photo ingress
     * feeds — the wizard-preset "Photo Library" set
     * (`ui/folders.md` § Photo backup → *Target set model*). Must succeed
     * before any ingest: the nest rejects a `changes.record` into a set with no
     * control-plane row (`not_found`), so ingesting into an unresolved set would
     * silently upload chunks that never become files. [deviceIdHex] is passed
     * through as-is — unlike [syncEngineHost], `photo_library_set` takes the
     * device id as its *String* form, not bytes (`libs/fauna-ffi/src/photo_library.rs`).
     * [stateDir] persists the device-local `photo-ingress.json` binding
     * alongside `folder-map.json`. Throws when not connected (mirrors
     * [backupDestinationsList] — the caller fails the whole pass rather than
     * uploading into a set the nest doesn't have).
     */
    suspend fun resolvePhotoLibrarySet(deviceIdHex: String, stateDir: String): FfiPhotoLibrarySet =
        com.fauna.ffi.photoLibrarySet(nestRpc(), stateDir, deviceIdHex, "fauna-android")

    /**
     * The user's own folders as ingest targets — each row's name beside its
     * `FolderRef` wire string, resolved through the shared `folderRefForRow`.
     * Owner-scoped list (`folders().list()`), so every row is this nest's own —
     * exactly the rows the in-process engine host resolves a ref against
     * (`engine_lifecycle::summary_for_ref` over this nest's list). A row the
     * resolver yields no ref for is left out, never offered by name.
     */
    suspend fun ownFolderChoices(): List<FolderChoice> =
        nestRpc().folders().list().mapNotNull { row ->
            folderRefForRow(row.id, null, null)?.let { FolderChoice(row.name, it) }
        }

    /**
     * The posts-rail (feed composer) drafts-sync handle for the current
     * connection, or null when not yet connected. Built once per session in
     * [ensureNestConnected] and closed in [clearAuth]; [FeedManagerHost] fetches
     * it the moment a fresh [feedManager] singleton exists, to restore the
     * owner's persisted draft and drive the debounced autosave.
     */
    fun postsDraftsSync(): FfiDraftsSync? = postsDraftsSync

    /**
     * Build the shared Task-delegation surface view-model
     * (`fauna_client_delegation::TaskDelegationView`) over this connection — the
     * native twin of the wasm SPA's binding all seven apps render
     * (`docs/goal/behavior/participants.md` § Task delegation; ui.yaml page
     * `task-delegation`). [deviceIdHex] is hex-encoded exactly as
     * [buildCustodianHost] takes it (the lease loop's own encoding);
     * [capability] is [FfiHeavyTaskCapability.VIEWER_ONLY] on android — a phone
     * is always battery-mobile and never runs a heavy task kind. Returns null
     * until the WS socket is up / the actor is authenticated (mirrors
     * [buildLinkedNestsMachine]); the caller retries on the next page visit.
     */
    fun buildTaskDelegationView(
        deviceIdHex: String,
        capability: FfiHeavyTaskCapability,
    ): FfiTaskDelegationView? {
        val client = nestClient ?: return null
        if (secret == null) return null // not signed in yet
        val deviceIdBytes = HexUtil.hexToBytes(deviceIdHex)
        return client.taskDelegationViewForDevice(deviceIdBytes, capability)
    }

    // ── Backup-destination management (backups.md § Manage backup destinations) ──
    // The config read + mutate/save half of the destination CRUD, over the shared
    // `backup_destinations_*` FFI (config read + the add/edit/remove-and-save
    // sequencing the desktop apps drive directly). Mirrors `webClient()`:
    // throws when the WS socket is down / no identity.

    internal fun backupSecret(): ByteArray =
        secret?.let { HexUtil.hexToBytes(it) }
            ?: throw ApiException("Not connected to nest (WS-RPC)")

    internal fun backupNest(): FfiNestClient =
        nestClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    /** Read the configured backup destinations from the owner's `fauna.state.backup` rows. */
    suspend fun backupDestinationsList(): List<com.fauna.ffi.FfiBackupDestinationView> =
        com.fauna.ffi.backupDestinationsList(backupNest(), backupSecret())

    /** Resolve + record a new destination; returns the updated list. */
    suspend fun backupDestinationAdd(url: String, name: String): List<com.fauna.ffi.FfiBackupDestinationView> =
        com.fauna.ffi.backupDestinationAdd(backupNest(), backupSecret(), url, name)

    /**
     * Enroll **this device** as a client custodian — the second add path
     * (`docs/goal/ui/backups.md` § Third destination kind → *Enrollment*), the
     * native twin of what linux/tui reach by linking `fauna_client_config`
     * directly. Returns the updated list, like every sibling here.
     *
     * A separate FFI verb rather than more arguments on [backupDestinationAdd]:
     * a custodian has **no address**, so there is nothing to resolve and no
     * destination nest to open a session with.
     *
     * [custodianDeviceId] must be this device's stable sync device id — the same
     * one [syncEngineHost] presents — because the source nest keys the
     * custodian's status row on it. The caller reads it from `SecureStorage`
     * rather than minting one; a blank id is refused by the shared enroll (not
     * defaulted), since a `client-device` row without one projects `Inert` and
     * nothing could ever drive it.
     *
     * [capacityCapBytes] is already parsed by the caller through the shared
     * `parseByteSize`; `null` means **uncapped**, a real choice.
     */
    suspend fun backupDestinationEnrollCustodian(
        custodianDeviceId: String,
        name: String,
        capacityCapBytes: ULong?,
    ): List<com.fauna.ffi.FfiBackupDestinationView> =
        com.fauna.ffi.backupDestinationEnrollCustodian(
            backupNest(), backupSecret(), custodianDeviceId, name, capacityCapBytes,
        )

    /** Rename / change-URL a destination (same-nest only); returns the updated list. */
    suspend fun backupDestinationEdit(id: String, url: String, name: String): List<com.fauna.ffi.FfiBackupDestinationView> =
        com.fauna.ffi.backupDestinationEdit(backupNest(), backupSecret(), id, url, name)

    /** Drop a destination; returns the updated list. */
    suspend fun backupDestinationRemove(id: String): List<com.fauna.ffi.FfiBackupDestinationView> =
        com.fauna.ffi.backupDestinationRemove(backupNest(), backupSecret(), id)

    // ── Destination places (backup-destinations.md § Ordinary-folder coverage) ──
    // The folders page's per-folder attach/detach section, over the shared
    // `fauna_client_config::{list_folder_destinations, attach_folder_to_destination,
    // detach_folder_from_destination}` sequences (`libs/fauna-ffi/src/
    // backup_destinations.rs`'s `folderDestinations*` trio) — android is a
    // non-linking consumer like web, so this leg reaches them over the FFI web
    // minted rather than a second wasm-style face. Each mutation re-reads the
    // folder's places and returns THAT, never an optimistic flip.

    /** `fauna.backup.destination.list` joined with the config's display names,
     *  for ONE folder — the section's lazy-on-first-expand read. */
    suspend fun folderDestinationsList(folderId: Long): List<com.fauna.ffi.FfiFolderDestinationPlace> =
        com.fauna.ffi.folderDestinationsList(backupNest(), backupSecret(), folderId)

    /** Attach [folderId] to [destinationId]; resolves to the folder's re-read places. */
    suspend fun folderDestinationAttach(
        folderId: Long,
        destinationId: String,
    ): List<com.fauna.ffi.FfiFolderDestinationPlace> =
        com.fauna.ffi.folderDestinationAttach(backupNest(), backupSecret(), folderId, destinationId)

    /** Detach [folderId] from [destinationId]; [folderSet] is the attached row's
     *  own `folder_set` — never re-derived here. Resolves to the folder's
     *  re-read places. */
    suspend fun folderDestinationDetach(
        folderId: Long,
        destinationId: String,
        folderSet: String,
    ): List<com.fauna.ffi.FfiFolderDestinationPlace> =
        com.fauna.ffi.folderDestinationDetach(backupNest(), backupSecret(), folderId, destinationId, folderSet)

    /**
     * Run one client-side backup **audit** pass and hand back the full
     * per-destination picture (`docs/goal/ui/backups.md` § Audit-alert
     * surface) — the gated FFI free fn [com.fauna.ffi.backupAuditRunPass],
     * which implements no audit logic itself: it loads this device's audit
     * state, audits every configured destination directly (never through the
     * source nest), merges over what was already known, persists, and
     * returns one row per destination carrying `alertReason` — never the raw
     * verdict, so this client cannot re-derive which states are loud.
     * Deliberately a **separate** round trip from [loadBackupDestinationStatus]
     * (a slow/unreachable destination must not delay the status rows).
     * [accountStores.backupAuditStatePath] is this device's actor-scoped
     * audit-state file; [accountStores.syncStateDir] is where the
     * `FfiSyncEngineHost` keeps this account's synced folder replicas, which
     * anchor the covered-folder mirror plane's inclusion population
     * (`backup-destinations.md` § Ordinary-folder coverage → *Retention + audit*).
     * [ownCustodian] is this device's own custodian store as
     * [custodianStoreFootprint] read it, with this device's sync id: the pass
     * folds its standing source regressions into the row that assigns this
     * device. `null` is "no store was read" — that row's record stands.
     */
    suspend fun backupAuditRunPass(
        ownCustodian: com.fauna.ffi.FfiOwnCustodianStore? = null,
    ): List<com.fauna.ffi.FfiDestinationAuditRow> =
        com.fauna.ffi.backupAuditRunPass(
            backupNest(),
            backupSecret(),
            accountStores.backupAuditStatePath(),
            syncStateDir = accountStores.syncStateDir(),
            ownCustodian = ownCustodian,
        )

    /**
     * Feed the audit's observation high-water: "this client has displayed
     * activity stamped [lastActivityMs]" — the load-bearing half of the
     * audit-alert surface, not an afterthought. A shell that renders the two
     * elements but never calls this ships a **permanently-passing** audit
     * (`docs/goal/ui/backups.md` § Audit-alert surface). Call from the
     * conversation list's own render, the one place this client shows the
     * user what it knows about nest-originated message kinds — mirrors
     * linux/tui/web. Pure local file op (no nest connection needed); never
     * throws. Returns whether anything was persisted (the shared
     * `observe_local_record` is monotonic, so a repeat render is a no-op).
     */
    fun backupAuditObserve(lastActivityMs: Long): Boolean =
        com.fauna.ffi.backupAuditObserve(accountStores.backupAuditStatePath(), lastActivityMs)

    /**
     * Report this box's public IP so the nest can gate ACME HTTP-01 on the
     * strong resolve-check (domains-and-tls-bootstrap.md § Host-address
     * acquisition). Fire-and-forget at the universal post-auth hook,
     * **admin-gated** by the caller — a non-admin call is refused nest-side.
     * The safety invariant (never publish a private/LAN address) lives
     * entirely in the shared fn; this adds no classification logic.
     */
    suspend fun reportHostAddress(): com.fauna.ffi.FfiHostAddressOutcome =
        com.fauna.ffi.reportHostAddress(backupNest())

    /**
     * The deployment-identity rotation ceremony's roster read (the
     * `admin-nest-seed-rotate-*` section) — `fauna.admin.admins.list` folded
     * with each admin's resolved label into a [com.fauna.ffi.FfiSeedRotationConfirmView]
     * (`can_confirm` + a `blockedReason` when it isn't), the same shared fold
     * linux's `client.rs::load_seed_rotate_roster` drives. Free function, not a
     * client method — the FFI shape tui/linux/macos/ios/web all share.
     */
    suspend fun seedRotateRoster(): com.fauna.ffi.FfiSeedRotationConfirmView =
        com.fauna.ffi.seedRotateRoster(backupNest())

    /**
     * Rotate this nest's deployment identity. Resolves THIS
     * connection's nest id before the ceremony (the box serves the successor
     * identity afterward), then the account-plane rotation drive: the
     * successor's custody row is merged and published before the dispatch, and
     * there is no fan-out afterwards (`box-recovery.md` § The plane-era
     * recovery floor, *(c) The writes*). The call outlives a naive reply budget — a WS 1001 drop + reconnect
     * mid-ceremony is expected, never bound this to an agent-command timeout
     * (mirrors linux `client.rs::rotate_deployment_seed`'s own warning).
     */
    suspend fun rotateDeploymentSeed(): com.fauna.ffi.FfiSeedRotationResult =
        com.fauna.ffi.rotateDeploymentSeed(backupNest(), backupSecret())

    /**
     * The deployment-seed **custody leg** at the post-auth edge
     * (`box-recovery.md` § The plane-era recovery floor, *(c) The writes*) —
     * the only capture: if the account plane holds no live entry for THIS
     * nest and this identity is an admin here, fetch the box's seed, refuse one
     * that does not derive to the bound id, and merge the entry. Run on every
     * connect; the account-runtime seat runs the same leg at its store-ready
     * edge. The caller surfaces an unconfirmed outcome on the recovery-custody
     * warning banner. `appDataDir` is unused by the FFI now (kept in its
     * signature).
     */
    suspend fun selfHealDeploymentSeedCustody(): com.fauna.ffi.FfiDeploymentSeedSelfHeal =
        com.fauna.ffi.selfHealDeploymentSeedCustody(
            backupNest(), backupSecret(), context.filesDir.absolutePath,
        )

    /**
     * The whole S8 seal-backfill SWEEP — D1 (folder-plane) then D3 (owned
     * sets' snapshot tags), skipping `role == "member"` rows — the ONE
     * sequencing seam every UniFFI app now calls at its post-auth hook
     * instead of hand-rolling D1-then-D3-skip-member itself
     * (`docs/goal/behavior/path-sealing.md` § Implementation status today).
     * `nestRpc().folders()` already wires this connection's resolver +
     * owner `BackupKey` — no second derivation here. Best-effort throughout;
     * never throws.
     */
    suspend fun runSealBackfillSweep(): com.fauna.ffi.FfiSealBackfillSweepReport =
        nestRpc().folders().runSealBackfillSweep()

    /**
     * Read the per-destination backup status (last-upload time + backlog) for the
     * configured destinations — the NEST's `fauna.backup.status` projection over the
     * gated FFI free fn [com.fauna.ffi.backupDestinationStatus], which wraps the
     * shared `fauna_client_config::read_backup_status` every app now calls
     * (backups.md § Per-destination status read; repointed 2026-07-24, slice-4
     * leg (d)).
     *
     * The `deviceIdHex` / `dataDir` arguments are **gone**: the nest derives the
     * owner from the authenticated connection and there is no local coordinator
     * state to path-match, so the `data_dir` canonical-path contract retires with
     * them. Empty when zero destinations are configured. Mirrors apple
     * `APIClient.loadBackupDestinationStatus`.
     */
    suspend fun loadBackupDestinationStatus(): List<com.fauna.ffi.FfiBackupDestinationStatus> =
        com.fauna.ffi.backupDestinationStatus(backupNest(), backupSecret())

    // No upload-coordinator builder lives here any more. The in-app segment-backup
    // upload driver was deleted at the slice-5 flip — the **source nest** is the
    // segment-backup writer (`docs/goal/architecture/message-segment-store.md`
    // § Cross-location backup protocol), and this page reads per-destination status
    // from the nest projection ([loadBackupDestinationStatus]). Do not reintroduce a
    // client-side upload pass to make a delegation row look right: that row is meant
    // to show the nest.

    /**
     * Build this device's **client-device custodian host** — the *pull* direction
     * of backup on this device (since the slice-5 flip, the only direction an app
     * drives), and the
     * android half of slice 3d's mobile arm (`docs/goal/ui/backups.md` § Third
     * destination kind; `docs/goal/behavior/backup-restore.md` § Background
     * Tasks). Both android triggers build through it: the periodic
     * [com.fauna.app.service.CustodianHostWorker] (construct-`runAllKinds()`-
     * drop) and the foreground [com.fauna.app.service.CustodianPushKick]
     * (`startPushDebounce`).
     *
     * Returns **null when this device is not an enrolled custodian**, which is
     * the ordinary answer on most devices and not an error — the trigger runs a
     * clean no-op pass. Discovery is the source nest's destination
     * registry, never the at-rest config and never local IPC, so a cap the owner
     * raises on another device reaches this phone (`sync-agent.md` § Control
     * plane split).
     *
     * [deviceIdHex] is this device's stable sync device id — the id enrollment
     * recorded in the registry row, which is what the row is
     * matched on. [dataDir] is [AccountStores.custodianStoreBaseDir], the
     * **unscoped** base: shared Rust derives the actor-scoped store location
     * under it (see that method for why passing a pre-scoped path would be
     * wrong).
     *
     * The cloud-backup exclusion is **not** a parameter: android states its
     * posture once, in [CloudBackupPosture], so neither trigger can assert a
     * different one. The caller owns the returned handle and must `close()` it.
     */
    suspend fun buildCustodianHost(
        deviceIdHex: String,
        dataDir: String,
    ): com.fauna.ffi.FfiCustodianHost? =
        com.fauna.ffi.buildCustodianHost(
            backupNest(),
            backupSecret(),
            HexUtil.hexToBytes(deviceIdHex),
            dataDir,
            CloudBackupPosture.exclusion(),
        )

    /**
     * Measure this device's sealed custodian store — the `store_holds_bytes`
     * input to the shared orphanhood verdict (`backups.md` § Manage backup
     * destinations → *Reclaim this device's copy*).
     *
     * ⚠ Deliberately **not** reached through [buildCustodianHost]: that builder
     * answers `null` whenever no registry row names this device, and no row is
     * exactly what makes a store orphaned — so a host-bound read would be blind
     * in the one state this page needs it for. The shared fn opens the store
     * from the data dir alone, and an absent store reads as an empty one rather
     * than an error, which is the honest answer on every device that never
     * enrolled.
     */
    suspend fun custodianStoreFootprint(dataDir: String): com.fauna.ffi.FfiCustodianStoreInfo =
        com.fauna.ffi.custodianStoreFootprint(backupSecret(), dataDir)

    /**
     * Free this device's whole sealed custodian store — the confirmed
     * `backup-destination-reclaim-button` action and the remove dialog's opt-in.
     *
     * `still_hosting` in the returned outcome is a **reported refusal**, never
     * an error: shared Rust stops this process's own custodian work first, and
     * if it will not stop inside its budget nothing is deleted. The caller must
     * surface that outcome rather than treat it as success — the store is
     * intact and the orphaned row is the way to retry.
     */
    suspend fun reclaimCustodianStore(dataDir: String): com.fauna.ffi.FfiCustodianReclaimOutcome =
        com.fauna.ffi.reclaimCustodianStore(backupSecret(), dataDir)

    // ── Subscriptions — profile Tiers-tab author management (monetization.md ──
    // § Pillar 1 / profile.md). Slice A (SELF author mgmt): tier CRUD + pending-
    // request approve/reject + the per-tier subscriber roster + remove. Reads +
    // metadata writes ride FfiNestClient.subscriptions(); the encrypted-mode
    // mint+upload orchestration (create-tier / approve / remove) rides the
    // `subscriptions_*` author free-fns (nest + owner secret), mirroring the
    // backup-destinations seam. Pure glue over shared Rust (priority #2); lifts
    // the linux lead (apps/fauna-linux/src/views/profile/tiers.rs). Plaintext
    // mode mints nest-side — the same calls drive both modes.

    private fun subscriptionsClient(): com.fauna.ffi.FfiSubscriptionsClient =
        nestRpc().subscriptions()

    internal fun ownerSecretBytes(): ByteArray =
        secret?.let { HexUtil.hexToBytes(it) }
            ?: throw ApiException("Not connected to nest (WS-RPC)")

    /** §1 — the author's own tier definitions (authenticated own-read, ascending rank). */
    suspend fun subscriptionTiersList(): List<com.fauna.ffi.FfiTierItem> =
        subscriptionsClient().tiersList()

    /** §1 edit — metadata only (no roster change), the thin update kind; `null` leaves a field. */
    suspend fun subscriptionUpdateTier(
        name: String,
        rank: UInt?,
        description: String?,
        priceHint: String?,
        paymentUrl: String?,
        autoApprove: Boolean?,
        // `null` = keep the tier's current asking price; this kind has no
        // clear verb for any of its optional fields.
        askingPriceSats: ULong? = null,
    ): Boolean = subscriptionsClient()
        .tiersUpdate(name, rank, description, priceHint, paymentUrl, autoApprove, askingPriceSats)

    /** §1 delete. */
    suspend fun subscriptionDeleteTier(name: String): Boolean =
        subscriptionsClient().tiersDelete(name)

    /** §2 — pending subscribe/unsubscribe requests for the author's tiers. */
    suspend fun subscriptionRequestsList(): List<com.fauna.ffi.FfiPendingRequest> =
        subscriptionsClient().requestsList()

    /** §2 reject — the thin reject kind. */
    suspend fun subscriptionRejectRequest(requestId: Long): Boolean =
        subscriptionsClient().requestsReject(requestId)

    /** §3 — the selected tier's current subscriber roster. */
    suspend fun subscriptionSubscribersList(
        tierName: String,
    ): List<com.fauna.ffi.FfiSubscriberEntry> =
        subscriptionsClient().subscribersList(tierName)

    // ── Subscriptions — consumer side (the `subscription-settings` page, Slice ──
    // B; monetization.md § Pillar 1). Distinct from the author Tiers-tab above:
    // this is what the user *consumes* across every creator. Pure read + thin
    // unsubscribe over `FfiNestClient.subscriptions()`; lifts the linux lead
    // (apps/fauna-linux/src/settings/subscriptions.rs).

    /** The caller's own subscriptions across every creator (active + pending). */
    suspend fun subscriptionMineList(): List<com.fauna.ffi.FfiMineSubscription> =
        subscriptionsClient().mineList()

    /**
     * Unsubscribe from [authorId]; encrypted mode returns `Queued` (the
     * subscriber stays a member until the author commits the removal), so the
     * row does not vanish — the re-read reflects the nest's state.
     */
    suspend fun subscriptionUnsubscribe(authorId: ByteArray): com.fauna.ffi.FfiUnsubscribeReply =
        subscriptionsClient().unsubscribe(authorId)

    // ── Payments — Pillar 3 client legs (monetization.md § Pillars 2+3). ──
    // Author side: the Tiers-tab §4 provider section; buyer side: the
    // subscription-settings claim redemption.
    //
    // NOT HERE — they are extensions on this class, in a per-variant source set
    // (`src/payments/java/com/fauna/app/payments/PaymentsGlue.kt`, twinned by
    // `src/noPayments/`). `payments` is a gated-feature-registry member, so a
    // store-safe build links a `fauna-ffi` that exports no `FfiPaymentsClient`
    // at all, and a member of this shared class could not be removed per build
    // variant (dynamic-features.md § Platform-family surface excision). Call
    // sites are unchanged — `api.paymentsProvidersList()` still reads the same;
    // they just import from `com.fauna.app.payments`. `nestRpc()` and [secret]
    // below are `internal` rather than `private` for exactly those two files.

    // ── Subscriptions — OTHER-profile browse (profile.md § Layout & flow; ──
    // monetization.md § Pillar 1 Slice B item 7). When viewing ANOTHER actor's
    // profile, read their offered tiers + the viewer's status, then subscribe /
    // follow. Over the shared FfiSubscriptionsClient (offers_list is the OTHER
    // analogue of the bearer-keyed tiers_list — a prospective subscriber can't read
    // a creator's tiers via tiers_list); lifts the linux lead
    // (apps/fauna-linux/src/views/profile/offers.rs).

    /** Another author's offered tiers (by hex actor_id) — the OTHER-profile offers section. */
    suspend fun subscriptionOffersList(authorIdHex: String): List<com.fauna.ffi.FfiTierItem> =
        subscriptionsClient().offersList(HexUtil.hexToBytes(authorIdHex))

    /** The viewer's subscription status for [authorIdHex] (the held tier name, if any). */
    suspend fun subscriptionStatus(authorIdHex: String): com.fauna.ffi.FfiSubscriptionStatus =
        subscriptionsClient().statusGet(HexUtil.hexToBytes(authorIdHex))

    // ── Cross-user folder sharing — owner side (folders.md § Sharing). ──
    // The per-set "Shared with" roster read + the author share/remove orchestration
    // over the shared FoldersAuthor. The author fns reuse the SAME live conversations
    // ConversationsSession (one per-actor MlsEngine over the one mls_state.db — never a
    // second engine racing the SQLite file) held by the ConversationsManagerHost. Pure
    // glue over shared Rust (priority #2); mirrors apple APIClient.{shareFolder,
    // removeFolderMember,folderActorMembers}.

    /** The set's cross-user "Shared with" roster: `[{actor_id, handle, role}]` (owner + members). */
    suspend fun folderActorMembers(name: String): List<com.fauna.ffi.FfiFolderActorMember> =
        nestRpc().folders().membersListActors(name)

    /**
     * `fauna.folders.devices` — the ordinary sync per-device activity signal
     * (`folder-device-activity-item`/-label/-count, file-sync.md § Implementation
     * status today): which devices have recorded changes on this set, and how many.
     * Distinct from [folderActorMembers] (cross-USER sharing) — this is per-DEVICE,
     * same shape as web's `foldersDevices` / linux's `fetch_folder_devices` / tui's
     * `FoldersClient::devices`. Thin FFI passthrough, no machine involved (mirrors
     * `folderActorMembers`'s deliberate bypass of `DevicesMachine`/`DevicesNestApi`
     * for a read-only per-set fetch).
     */
    suspend fun folderDevices(name: String): List<com.fauna.ffi.FfiFolderDevice> =
        nestRpc().folders().devices(name)

    /**
     * Share an owner-only set with a resolved recipient (hex actor-id): create the MLS
     * group admitting them → bind the genesis content key → deliver the Welcome. Reuses
     * the live conversations MlsEngine. `member_nest_url` is `null` (same-nest only;
     * cross-nest share is a follow-on for every app, parity with the linux/apple leads).
     * `access` is the share-time grant (`"writer"` for read-write, `null`/`"reader"` for
     * the read-only default — multi-writer Phase 1, folders.md § Sharing).
     */
    suspend fun shareFolder(
        name: String,
        memberActorIdHex: String,
        access: String? = null,
    ): com.fauna.ffi.FfiShareOutcome =
        com.fauna.ffi.foldersShare(
            nestRpc(),
            conversationsSessionOrThrow(),
            ownerSecretBytes(),
            name,
            HexUtil.hexToBytes(memberActorIdHex),
            null,
            access,
        )

    /**
     * Remove a member from a shared set (rotates the content key for forward secrecy).
     * `groupIdHex` = the set's `mls_group_id`; the 32-byte channel-id derivation is a
     * blake3 KDF only shared Rust can reproduce ([folderChannelIdFromGroupId]).
     */
    suspend fun removeFolderMember(name: String, memberActorIdHex: String, groupIdHex: String) {
        com.fauna.ffi.foldersRemoveMember(
            nestRpc(),
            conversationsSessionOrThrow(),
            ownerSecretBytes(),
            name,
            com.fauna.ffi.folderChannelIdFromGroupId(groupIdHex),
            HexUtil.hexToBytes(memberActorIdHex),
        )
    }

    /**
     * Grant or edit a shared set's member `access` (+ optional byte cap, `null` =
     * uncapped) — the owner-editable-in-place write behind `folder-member-role-select`
     * / `folder-member-cap-input` (multi-writer Phase 1, folders.md § Sharing).
     * `fauna.folders.members.set_access` upserts the whole row — callers must send
     * both fields together, never just one.
     */
    suspend fun setFolderMemberAccess(
        name: String,
        memberActorIdHex: String,
        access: String,
        byteCap: Long?,
    ) {
        nestRpc().folders().membersSetAccess(name, memberActorIdHex, access, byteCap)
    }

    /**
     * Flip a folder's WebDAV serve flag (`folder-webdav-toggle`, every
     * owner row) through the shared author `serve_set` — content-key genesis/rotation +
     * the nest `webdav_enabled` flag + the MSEK-sealed `WebdavKeysBlob`
     * re-provision (`webdav-server.md` § Independent enablement point 2).
     * `mlsGroupIdHex` = the set's `mls_group_id` (`null` for an unshared set),
     * mirroring [shareFolder]/[removeFolderMember]'s author-over-conversations-session
     * shape; the native twin of the FFI `folders_serve_set` face linux/web call.
     * Returns the number of served sets the re-sealed blob carries (the face's
     * own `u32`; the test agent's `serve_enable_folder` reports it).
     */
    suspend fun serveSetFolder(name: String, mlsGroupIdHex: String?, enable: Boolean): UInt =
        com.fauna.ffi.foldersServeSet(
            nestRpc(),
            conversationsSessionOrThrow(),
            ownerSecretBytes(),
            name,
            mlsGroupIdHex,
            enable,
            // The session's sync device id: an enable also re-seals the set's
            // pre-serve files onto the served key, recorded under it
            // (`webdav-server.md` § Key model (c)).
            sessionAccount.deviceId,
        )

    /**
     * Whether this actor can serve any set over WebDAV — the capability gating
     * `folder-webdav-toggle` (serving seals the `WebdavKeysBlob` under the MSEK,
     * minted when mail is first enabled). The native twin of the FFI
     * `folders_can_serve_webdav` face linux/web call.
     *
     * Takes no conversations session — unlike [serveSetFolder], the question needs
     * only the owner's account store, so the Folders screen can ask it before the
     * rail is up.
     */
    suspend fun canServeWebdav(): Boolean =
        com.fauna.ffi.foldersCanServeWebdav(nestRpc(), ownerSecretBytes())

    /**
     * Paywall a website-enabled folder to a subscription tier (`folder-paywall-tier-select`,
     * website-enabled rows only) through the shared author `paywall_set` orchestration —
     * content-key genesis/re-seal + the nest `web_paywall_tier` flag + the
     * `content.read{folder:set}` grant minted to the nest's web-serve holder
     * (`monetization.md` § Pillar 2, the folder half). `mlsGroupIdHex` = the set's
     * `mls_group_id` (`null` for an owner-only set), mirroring [serveSetFolder]'s
     * author-over-conversations-session shape; the native twin of the FFI
     * `folders_paywall_set` face linux/web call. v1 is set-only (ratified 2026-07-13).
     */
    suspend fun paywallSetFolder(name: String, mlsGroupIdHex: String?, tier: String) {
        com.fauna.ffi.foldersPaywallSet(
            nestRpc(),
            conversationsSessionOrThrow(),
            ownerSecretBytes(),
            name,
            tier,
            mlsGroupIdHex,
        )
    }

    /**
     * Read the owner's default conflict policy for new folders
     * (`sync-default-conflict-policy-select`'s current value) — `null` = no
     * preference (new sets take the nest column default, `auto`). The native
     * twin of the shared `preference_surfaces::load_sync_prefs` linux/tui call.
     */
    suspend fun defaultConflictPolicyGet(): String? =
        com.fauna.ffi.loadSyncPrefs()

    /**
     * Set the owner's default conflict policy for new folders and persist —
     * stamped onto creates via `FolderWizardMachine.setDefaultConflictPolicy`,
     * never retro-applied to existing sets (each set's own
     * `folder-conflict-policy-select` stays authoritative). Returns the stored
     * value so the UI shows exactly what was saved.
     */
    suspend fun defaultConflictPolicySet(policy: String): String? =
        com.fauna.ffi.saveSyncPrefs(policy)

    /**
     * Push a newly resolved `<handle>@<domain>` into the live conversations
     * session (conversations.md § State & data shape → *Self-address: live,
     * never baked*). One shared cell, read by both rails at use time — the SMTP
     * `From:` and the domain the FaunaMls plane compares to route same-nest vs.
     * cross-nest — so this heals a session built before the identity resolved
     * without rebuilding any backend.
     *
     * A no-op when no session is active: the address is re-applied from the
     * refreshed caches the next time [authenticate] builds one.
     */
    fun setConversationsSelfAddress(address: String) {
        conversationsManagerHost.session?.setSelfAddress(address)
    }

    /** The live conversations session, or throw — the folder MLS ops require it. */
    private fun conversationsSessionOrThrow(): uniffi.fauna_conversations.ConversationsSession =
        conversationsManagerHost.session
            ?: throw ApiException("Conversations session not active — cannot share folder")

    // ── Cross-user folder sharing — recipient side (folders.md § Sharing, ──
    // Recipient side). Pure glue over shared Rust (priority #2); mirrors apple
    // APIClient.{pendingShares,acceptPendingShare,declinePendingShare,leaveFolder}.

    /** The staged (knocked) folder shares awaiting accept/decline — a peek, never acks. */
    suspend fun folderPendingShares(): List<com.fauna.ffi.FfiPendingShare> =
        com.fauna.ffi.foldersPendingShares(nestRpc())

    /**
     * Accept a staged share (`folder-share-accept-button`): re-resolve the Welcome by
     * `inbox_id`, join the MLS group off the chat rail, then ack. Bypasses the contact
     * gate — accepting is an explicit user decision.
     */
    suspend fun acceptFolderShare(inboxId: Long) {
        com.fauna.ffi.foldersAcceptShare(nestRpc(), conversationsSessionOrThrow(), inboxId)
    }

    /** Decline a staged share (`folder-share-decline-button`): a bare ack — never joins. */
    suspend fun declineFolderShare(inboxId: Long) {
        com.fauna.ffi.foldersDeclineShare(nestRpc(), inboxId)
    }

    /**
     * Leave a set shared *with* us (`folder-leave-button`): self-scoped — drops only
     * our own roster row (no `ownerSecret`, no content-key rotation) then forgets the
     * MLS group locally. `groupIdHex` = the set's `mls_group_id`.
     */
    suspend fun leaveFolder(groupIdHex: String) {
        com.fauna.ffi.foldersLeave(nestRpc(), conversationsSessionOrThrow(), groupIdHex)
    }

    // ── T16 custody facet, owner side (devices.md § Custody facet, piece 2) ──
    //
    // Pure glue over `libs/fauna-ffi/src/custody.rs`, which itself projects the
    // shared fold — nothing here derives. The three calls are the whole surface
    // that face exports: load, revoke, drive. The two store-writing gestures
    // (budget / stop) and accept + mint are deliberately absent from the
    // boundary because no UniFFI app reaches the W3 account store yet, which is
    // why this app renders piece 2 only (see that module's docs).

    /**
     * Fold the owner-side custody facet off `fauna.state.custody-ceremony` (`custody-holder-*`).
     *
     * `null` = the store was unreadable this pass — a **transient**, not "no
     * custodians": the caller keeps whatever rows it already painted rather than
     * blanking live ones (the face's own contract).
     */
    suspend fun custodyFacetLoad(): uniffi.fauna_client_capabilities.CustodyFacetView? =
        com.fauna.ffi.custodyFacetLoad(nestRpc(), ownerSecretBytes())

    /**
     * Who holds this account's generation-key escrow, as lowercase-hex
     * identities — the Nests page's escrow-holder badge source
     * (`participant-escrow-holder-badge`). The shared
     * `AccountStoreHandle::escrow_holders` derivation (recorded escrow receipts,
     * never a nest assertion), read off the hosted account runtime. `null` = no
     * runtime yet or an unreadable pass — keep the previous set.
     */
    suspend fun custodyEscrowHolders(): List<String>? = com.fauna.ffi.custodyEscrowHolders()

    /**
     * Revoke a custody grant (`custody-holder-revoke-button`) — piece 2's one
     * gesture, and store-free.
     *
     * Carries the **grant id, never a row index**: a refold re-orders rows, so an
     * index captured at paint time can address a different custody by the time
     * the act runs. `holder` is the row's accept-bound `custodian_key`, absent
     * while the ceremony is still pending (which is why the control is disabled
     * there). The load-bearing nest-before-record ordering lives in shared Rust.
     */
    suspend fun custodyRevoke(
        grantId: ByteArray,
        holder: ByteArray?,
    ): com.fauna.ffi.FfiCustodyActOutcome =
        com.fauna.ffi.custodyRevoke(nestRpc(), ownerSecretBytes(), grantId, holder)

    /**
     * Fire one ceremony drive pass — the "act" half of the record-then-act loop,
     * and what makes owner-side receipt freshness real (it fetches the receipts
     * custodian nests deposited at this account's nest and folds each through the
     * recorded-accept verify path).
     *
     * Fire-and-forget by design and cheap when settled, so the page calls it on
     * its own edge. A no-op when the conversations session isn't live yet: the
     * pass has no channel to post on, and the next edge retries — same guarded
     * shape as [wireDevicesMlsQuery], never an error surfaced to the user.
     *
     * `suspend` only so the pass can be spawned: the shared drive hands it to
     * `tokio::spawn`, which needs the runtime context an `async_runtime = "tokio"`
     * export enters and a synchronous one never has — the synchronous export
     * panicked "there is no reactor running" on every call, and the drive never ran.
     */
    suspend fun custodyDrive() {
        val session = conversationsManagerHost.session ?: return
        runCatching { com.fauna.ffi.custodyDrive(nestRpc(), ownerSecretBytes(), session) }
            .onFailure { ShellLog.w("ApiClient", "custodyDrive failed: ${it.message}") }
    }

    /**
     * Wire the B3 member-row join-filter into a freshly built [devices] machine — the
     * load-bearing `MlsQuery` seam (`docs/goal/ui/folders.md` § Sharing: the nest
     * returns *rostered* members, so a `role == "member"` row must render only if this
     * client has actually MLS-joined the group; unwired ⇒ fail-safe, every member row
     * hidden). MUST run before the machine's first `refresh()`. A no-op when the
     * conversations session isn't live yet (mirrors apple/windows — guarded, no retry).
     */
    fun wireDevicesMlsQuery(devices: uniffi.fauna_devices_machine.DevicesMachine) {
        conversationsManagerHost.session?.let { session ->
            com.fauna.ffi.wireDevicesMlsQuery(devices, session)
        }
    }

    /**
     * Inject the foreign-set (cross-nest) list source into [devices] — the twin
     * [wireDevicesMlsQuery] has always needed. A set shared from ANOTHER nest has
     * no row in this nest's own list; unwired, `DevicesMachine`'s foreign-sets
     * source stays `None` and the machine appends NO foreign rows at all
     * (`libs/fauna-devices-machine/src/machine.rs` — `if let Some(source)`), so a
     * cross-nest shared set renders as *absent*, not stale
     * (`docs/goal/ui/folders.md` § Implementation status today). MUST run
     * before the machine's first `refresh()`, beside [wireDevicesMlsQuery].
     * Best-effort exactly like that sibling: no secret / no nest connection ⇒ no
     * foreign rows, never a page error.
     */
    fun wireDevicesForeignSets(devices: uniffi.fauna_devices_machine.DevicesMachine) {
        val nest = nestClient ?: return
        val ownerSecret = secret ?: return
        com.fauna.ffi.wireDevicesForeignSets(devices, nest, HexUtil.hexToBytes(ownerSecret))
    }

    /**
     * Inject the followed-public-folders source into [media] — the Media page's
     * twin of [wireDevicesForeignSets] (`media.md` § Followed public folders).
     * Unwired, `snapshot.followed` stays permanently empty, so
     * `media-folder-filter` offers no followed scope and `select_followed_scope`
     * / `download_followed` have nothing to act on — compiles and is silently
     * unreachable. Run once right after [buildMediaMachine]. Best-effort like
     * its sibling: no secret / no nest connection ⇒ no followed scopes, never a
     * page error.
     */
    fun wireMediaFollowedFolders(media: uniffi.fauna_media_machine.MediaMachine) {
        val nest = nestClient ?: return
        val ownerSecret = secret ?: return
        runCatching { com.fauna.ffi.wireMediaFollowedFolders(media, nest, HexUtil.hexToBytes(ownerSecret)) }
    }

    // ── Profile publish/edit (profile.md § Where logic lives → Profile ──
    // publish/edit). The SELF edit form's read-modify-write publish over the
    // shared `fauna-client-profile` UniFFI seam (`FfiProfileClient` + the
    // `build_edited_profile` / `decode_profile_display` free fns), mirroring the
    // subscriptions glue above. Pure glue over shared Rust (priority #2); lifts
    // the linux lead (apps/fauna-linux/src/views/profile/{mod,edit}.rs).

    private fun profileClient(): com.fauna.ffi.FfiProfileClient =
        nestRpc().profile()

    /**
     * `fauna.profile.get` — the stored signed profile bytes for [actorIdHex], or
     * `null` when the actor has never published (`fauna.profile.not_found`) or the
     * read fails. Mirrors linux's open-form fetch (`edit.rs::open_form` /
     * `mod.rs::refresh_header_name`, both `Err(_) => None`): a missing/failed read
     * starts the edit form from a blank first-publish (read-modify-write base `null`)
     * and the header from its handle/actor_id fallback.
     */
    suspend fun profileGet(actorIdHex: String): ByteArray? =
        try {
            profileClient().profileGet(actorIdHex)
        } catch (_: Exception) {
            null
        }

    /**
     * The edit form's read-modify-write base: this session's OWN stored profile
     * bytes, read through the shared read-prove-record
     * (`FfiProfileClient.loadEditBase`), so a succession link the base needs is
     * recorded in the account registry before [buildEditedProfileWithImages]
     * reads [profilePredecessors] (`profile.md` § After an identity succession
     * → the linkless bullet). `null` for a never-published profile or a failed
     * read — the blank first-publish, as with [profileGet].
     */
    suspend fun loadProfileEditBase(): ByteArray? =
        try {
            profileClient().loadEditBase(ownerSecretBytes(), accountStores.accountRegistry)
        } catch (_: Exception) {
            null
        }

    /** Project stored profile bytes to the three editable display fields. */
    fun decodeProfileDisplay(body: ByteArray): com.fauna.ffi.FfiProfileDisplay =
        com.fauna.ffi.decodeProfileDisplay(body)

    /**
     * Read-modify-write sign step: overwrite only `display_name` / `bio` / `links`
     * on the fetched [baseBody] (preserving avatar/banner/nests/admin_nests/
     * load_hint/inbox_mode), or build a fresh profile when [baseBody] is `null`
     * (first publish). Returns the signed `EmbedAsBytes` wire for [profileSet]. The
     * FFI twin of linux `edit.rs::submit`.
     */
    fun buildEditedProfile(
        baseBody: ByteArray?,
        displayName: String?,
        bio: String?,
        links: List<com.fauna.ffi.FfiProfileLink>,
    ): ByteArray =
        com.fauna.ffi.buildEditedProfile(
            ownerSecretBytes(), baseBody, profilePredecessors(), displayName, bio, links,
        )

    /** Whom this session's identity succeeded from, per the account registry —
     *  the only evidence that admits a stored base signed by someone else
     *  (`profile.md` § After an identity succession, the successor
     *  RE-PUBLISHES). Empty for an identity that never succeeded. */
    private fun profilePredecessors(): List<String> =
        sessionActorHex()?.let { accountStores.predecessorsOf(it) } ?: emptyList()

    /**
     * [buildEditedProfile] plus the two picture fields — the full edit-form
     * write once the caller can set/clear an avatar/banner. `avatar`/`banner`
     * are [com.fauna.ffi.FfiProfileImageEdit.Keep] on a text-only save (what
     * [buildEditedProfile] passes).
     */
    fun buildEditedProfileWithImages(
        baseBody: ByteArray?,
        displayName: String?,
        bio: String?,
        links: List<com.fauna.ffi.FfiProfileLink>,
        avatar: com.fauna.ffi.FfiProfileImageEdit,
        banner: com.fauna.ffi.FfiProfileImageEdit,
    ): ByteArray =
        com.fauna.ffi.buildEditedProfileWithImages(
            ownerSecretBytes(), baseBody, profilePredecessors(), displayName, bio, links, avatar, banner,
        )

    /** `fauna.profile.set` — publish/replace the caller's own profile. */
    suspend fun profileSet(body: ByteArray) =
        profileClient().profileSet(body)

    private fun bridgesRpc(): FfiBridgesClient =
        bridgesClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun emailRpc(): FfiEmailClient =
        emailClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun adminRpc(): FfiAdminClient =
        adminClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    // `internal`, not `private`, for the payments variant seam alone (see the
    // Payments comment above) — every other caller is inside this class.
    internal fun nestRpc(): FfiNestClient =
        nestClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun feedRpc(): FfiFeedClient =
        feedClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun postsRpc(): FfiPostsClient =
        postsClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun contactsRpc(): FfiContactsClient =
        contactsClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun notificationsRpc(): FfiNotificationsClient =
        notificationsClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun inboxRpc(): FfiInboxClient =
        inboxClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun moderationRpc(): FfiModerationClient =
        moderationClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun snapshotsRpc(): FfiSnapshotsClient =
        snapshotsClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun caldavRpc(): FfiCaldavClient =
        caldavClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun carddavRpc(): FfiCarddavClient =
        carddavClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun blueskyRpc(): FfiBlueskyClient =
        blueskyClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun featuresRpc(): FfiFeaturesClient =
        featuresClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun accountRpc(): FfiAccountClient =
        accountClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun spamRpc(): FfiSpamClient =
        spamClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    /**
     * The `fauna.conversations.keypackage.{upload,count}` typed client (the MLS
     * key-package pool). `internal` so [MlsManager] — which owns the local-engine
     * key-package generation — composes upload + count directly off it, the same
     * way the search/spam seams keep the transport client here and the domain
     * call at the consumer.
     */
    internal fun conversationsRpc(): FfiConversationsClient =
        conversationsClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    private fun syncRpc(): FfiSyncClient =
        syncClient ?: throw ApiException("Not connected to nest (WS-RPC)")

    internal suspend fun ensureAuthenticated() {
        if (token == null || System.currentTimeMillis() >= tokenExpiresAt) {
            val s = secret ?: throw ApiException("No secret for token refresh")
            authenticate(s)
        }
    }

    // Node discovery (node-info / handle-available), account registration
    // (POST /api/v1/register), and the RFC-8628 device-code/-token flow used to
    // live here as HTTP calls. All four nest routes were deleted in the WS-RPC
    // rip-out: onboarding discovery + registration now run inside the FFI
    // onboarding machine (pre-identity WS-RPC over `fauna_anon_client`), and the
    // device-code flow was replaced by Ed25519 challenge-auth. The Android shells
    // are snapshot-driven views of that machine, so the old HTTP methods had no
    // remaining callers and were removed (api-layers.md § Discovery / Auth).

    // -- Inbox (fauna-native delivery queue) --

    /**
     * Peek the caller's undelivered fauna-native inbox items over WS-RPC
     * `fauna.inbox.fetch` — the transport migration of the deleted HTTP
     * `GET /api/v1/inbox/{actor_id}` twin (api-layers.md § Inbox & Messaging).
     * Caller-scoped (the WS connection knows the actor), so there is no
     * `actorId` param. This is a pure **peek**: it does not mark items
     * delivered — the old GET marked-on-read, dropping items if the client
     * crashed before applying them. A draining consumer would `ack` the ids
     * it durably applies; the unread-count widget reads the first page only.
     * `limit = 0` selects the handler default cap.
     */
    suspend fun fetchInbox(): List<ByteArray> =
        inboxRpc().fetch(0u).items.map { it.payload }

    // NOTE: `sendToInbox` (the `POST /api/v1/inbox/{actor}` social-inbox-delivery
    // twin) was removed in the WS-RPC HTTP rip-out. Its only caller was Android
    // P2P signaling, which was off-spec against that route — the nest's inbox
    // accepts strictly a signed (ContactRequest, Post) tuple (it rejected the
    // signal JSON), and P2P signaling belongs on a nest-relayed WebSocket, not the
    // contact-inbox (docs/goal/behavior/p2p.md § Tunnel lifecycle). The authed
    // replacement for genuine social-inbox send is `fauna.inbox.send`
    // (FfiInboxClient.send) — `sendKnock` below is the first real (CR, Post)
    // social-inbox sender; a general (non-knock) sender would follow the same
    // shape with `build_signed_email` in place of `build_knock_payload`.

    suspend fun sendEmailSmtp(to: String, rawRfc5322: ByteArray) {
        emailRpc().send(listOf(to), rawRfc5322)
    }

    // -- Blobs --

    /**
     * Upload a blob in the encrypted-mode wire shape: `multipart/form-data`
     * with a `sidecar` part (DAG-CBOR [com.fauna.ffi.FfiSealedUpload.sidecarCbor],
     * `application/cbor`) and a `bytes` part (the sealed bytes,
     * `application/octet-stream`). The caller produces both via
     * [com.fauna.ffi.processAndSealUpload]. The nest parses this in
     * `blob_routes.rs::parse_multipart_upload` — since the 2026-07-01 strict flip
     * it is the ONLY accepted shape; a raw `application/octet-stream`
     * body is rejected 400 (there is no legacy raw-bytes upload path).
     */
    suspend fun uploadBlobWithSidecar(sidecarCbor: ByteArray, sealedBytes: ByteArray): BlobResponse {
        val body = postMultipartBlob("api/v1/blob", sidecarCbor, sealedBytes)
        return json.decodeFromString(body)
    }

    /**
     * Upload [data] as a `PublicPost` blob (signed plaintext — the shape feed
     * attachments and profile avatar/banner pictures both use, `media.md`
     * § Encryption at rest) and return the nest's blob hash. Uploads the sealed
     * thumbnail first, best-effort (the nest never gates the primary on it).
     * Mirrors linux/tui's `fauna_client::upload_public_post_blob`.
     */
    suspend fun uploadPublicPostBlob(data: ByteArray): String {
        val payload = com.fauna.ffi.processAndSealUpload(data, com.fauna.ffi.FfiUploadAudience.PublicPost)
        payload.thumbnail?.let { thumb ->
            runCatching { uploadBlobWithSidecar(thumb.sidecarCbor, thumb.bytes) }
        }
        return uploadBlobWithSidecar(payload.primary.sidecarCbor, payload.primary.bytes).hash
    }

    fun blobUrl(hash: String): String = "$nodeUrl/api/v1/blob/$hash"

    suspend fun checkBlobC2pa(hash: String): Boolean {
        val encoded = java.net.URLEncoder.encode(hash, "UTF-8")
        ensureAuthenticated()
        val request = authorizedRequest("$nodeUrl/api/v1/blob/$encoded")
            .head()
            .build()
        return suspendCancellableCoroutine { cont ->
            val call = httpClient.newCall(request)
            cont.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onResponse(call: Call, response: Response) {
                    val hasC2pa = response.header("x-c2pa") == "true"
                    cont.resume(hasC2pa)
                }
                override fun onFailure(call: Call, e: java.io.IOException) {
                    cont.resume(false)
                }
            })
        }
    }

    /**
     * Fetch a blob's bytes by content hash over the bulk-binary HTTP carve-out
     * (`GET /api/v1/blob/<hash>`) — the feed media painter (the `RenderBlock.Image`
     * hash the `FeedManager` folds into `PostSummary.document`,
     * render-model.md § D6). Mirrors linux `FaunaClient::fetch_blob_bytes`, and like
     * it returns the stored bytes as-is: a public post's media is signed plaintext,
     * while a tier-restricted post's attachment is ciphertext that
     * [com.fauna.app.core.feed.PostMediaOpen] opens through the shared feed manager
     * before anything decodes it (`media.md` § Encryption at rest) — unlike the
     * conversations `attachment_bytes` path, whose decrypt and cache live in the
     * conversations manager. Kept as client glue (the async byte load stays
     * per-platform — render-model.md § The boundary); the shared model only carries
     * the hash.
     */
    suspend fun fetchBlobBytes(hash: String): ByteArray {
        val encoded = java.net.URLEncoder.encode(hash, "UTF-8")
        return fetchNestPathBytes("/api/v1/blob/$encoded")
    }

    /**
     * The bytes at a nest-relative [path] on the reader's own nest, with the session
     * bearer — how a bridged post's `ProxiedImage` is fetched, exactly as a blob is
     * (render-model.md § D6c: the proxied path is the blob loader's call with a
     * different argument). The path is opaque, never parsed for the remote host.
     */
    suspend fun fetchNestPathBytes(path: String): ByteArray {
        ensureAuthenticated()
        val request = authorizedRequest("$nodeUrl$path").build()
        return executeBytes(request)
    }

    // -- Sync (fauna.sync.* WS-RPC via FfiSyncClient; the deleted
    //    GET|POST /api/v1/sync/{register,changes,files,status,backup-status}
    //    HTTP twins are gone from the nest — api-layers.md § File Sync). These ride
    //    the FfiSyncStatus / FfiBackupStatusEntry records
    //    directly — the shape the Linux/Windows apps already consume, no
    //    per-app sync DTOs (the account.* / quota seam precedent). The dead
    //    HTTP `register` (no callers — registration rides the sync engine) and
    //    `files` (no callers) twins were dropped, and the single-file byte
    //    download route was deleted nest-side (never a caller; sealed sets
    //    made server-side reassembly impossible — backup-restore.md § 3). The
    //    bespoke `/api/v1/chunks` + `/api/v1/manifests` HTTP calls and
    //    `recordChange` (`changesRecord`) were dropped from this client the same way in the
    //    `FfiSyncEngineHost` cutover — `ingestFile` now
    //    does the changes.record custody upsert internally.

    suspend fun fetchSyncStatus(folder: String): FfiSyncStatus =
        syncRpc().status(folder)

    // -- Snapshots (fauna.filesync.snapshot.* WS-RPC via FfiSnapshotsClient) --
    // The snapshot control plane was deleted from the nest router in the
    // WS-RPC-everywhere rip-out (api-layers.md § Snapshots). The Backups page
    // consumes the FFI reply types directly (the shared shape the Linux app
    // reads via fauna-client-snapshots; priority #2), so the old JSON DTOs were
    // dropped.

    suspend fun createSnapshot(folder: String): FfiSnapshotCreateFolderReply =
        snapshotsRpc().snapshotCreateFolder(folder, emptyList())

    suspend fun fetchSnapshots(folder: String): List<FfiSnapshotSummary> =
        snapshotsRpc().snapshotList(null, folder, 0u)

    // The local-restore picker source: the owner-implicit message-kind snapshot
    // list (folder = None), backups.md § Restore from backup destination +
    // § Where logic lives ("owner-implicit, message-kind-scoped"). message_kind
    // = null returns every kind (mail / calendar / conv).
    suspend fun listMessageKindSnapshots(): List<FfiSnapshotSummary> =
        snapshotsRpc().snapshotList(null, null, 0u)

    suspend fun fetchSnapshotDetail(id: Int): FfiSnapshotGetReply =
        snapshotsRpc().snapshotGet(id.toLong())

    /** Single-file restore (`snapshot-file-download-button[i]`, backup-restore.md
     * § 3): fetch one file's decrypted bytes out of the snapshot via the shared
     * client-side walk (`fauna_core::file_download`). Replaces the legacy
     * `snapshotFileUrl` HTTP route — bearer-authed and refusing sealed
     * manifests, so backups.md § Where logic lives says it must gain no callers. */
    suspend fun downloadSnapshotFileBytes(deviceIdHex: String, snapshotId: Int, path: String): ByteArray =
        com.fauna.ffi.downloadSnapshotFileBytes(
            nestRpc(), ownerSecretBytes(), HexUtil.hexToBytes(deviceIdHex), snapshotId.toLong(), path
        )

    suspend fun fetchBackupStatus(): List<FfiBackupStatusEntry> =
        syncRpc().backupStatus()

    suspend fun deleteSnapshot(id: Int) {
        snapshotsRpc().snapshotDelete(id.toLong())
    }

    // Owner-only immediate snapshot delete (backups.md § User actions,
    // Architectural rule 4). Both confirmId (the snapshot id retyped) and
    // acknowledge (exactly immediate_delete_ack_text()) must match or the nest
    // rejects with confirm_mismatch / acknowledge_mismatch / hard_floor_breach —
    // the friction bar the modal enforces client-side before enabling confirm.
    suspend fun deleteSnapshotImmediate(id: Int, confirmId: String, acknowledge: String) {
        snapshotsRpc().snapshotDeleteImmediate(id.toLong(), confirmId, acknowledge)
    }

    // The exact acknowledge phrase the immediate-delete modal makes the user type,
    // from the protocol constant fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT
    // (the shared ack_text_matches_protocol_const test guards it can't drift).
    fun immediateDeleteAckText(): String = com.fauna.ffi.immediateDeleteAckText()

    suspend fun pruneSnapshots(folder: String, keepLast: Int = 3): FfiSnapshotPruneReply =
        // keepDaily/Weekly/Monthly/Yearly left null — android's prune only caps by keepLast.
        // keepYearly added to the shared FfiSnapshotsClient.snapshot_prune (apple).
        snapshotsRpc().snapshotPrune(folder, false, keepLast.toUInt(), null, null, null, null)

    suspend fun checkIntegrity(folder: String, verifyContent: Boolean = false): FfiSnapshotCheckReply =
        snapshotsRpc().snapshotCheck(folder, verifyContent)

    // -- Restore (history + divergence reads + the local restore action) --
    // backups.md §§ Restore history / Restore divergence / Restore from backup
    // destination. The kind-composition lives in the shared fauna-client-snapshots
    // crate (priority #2); these forward to the FfiSnapshotsClient seam.

    suspend fun listRestoreHistory(limit: Int = 0): List<FfiRestoreHistoryRow> =
        snapshotsRpc().snapshotListRestoreHistory(limit.toUInt())

    suspend fun listRestoreDivergence(snapshotId: Long): List<FfiRestoreDivergenceRow> =
        snapshotsRpc().snapshotListRestoreDivergence(snapshotId)

    suspend fun restoreMessageKind(snapshotId: Long, confirmId: String): FfiSnapshotRestoreReply =
        snapshotsRpc().snapshotRestoreMessageKind(snapshotId, confirmId)

    // -- Quota --

    // fauna.quota.get WS-RPC (the GET /api/v1/quota twin was deleted in the
    // account T3 cutover). Rides FfiQuotaGetReply directly — the rich shape
    // (tier + per-resource inbox/storage/devices/features usage) the apple +
    // windows apps already consume — no per-app quota model.
    suspend fun fetchQuota(): FfiQuotaGetReply {
        ensureAuthenticated()
        return accountRpc().quotaGet()
    }

    // -- Gated features --

    // `feature-limits-section` — the gated-feature plane's transparency read
    // (`dynamic-features.md` § Transparency & auditability, boundary 4: "no
    // silent gates"). `FfiFeaturesClient.rows()` is the one call: it joins
    // `fauna.features.status` with `fauna.nest.info`'s capability set and
    // folds both into ready-to-render rows, same as tui/linux/web.
    suspend fun fetchFeatures(): List<FfiFeatureRow> {
        ensureAuthenticated()
        return featuresRpc().rows()
    }

    // fauna.account.get WS-RPC — the full account snapshot (actor id, handle,
    // tier, quota, …). Used by factory-reset to source the admin's handle
    // authoritatively from the still-live session before the wipe.
    suspend fun accountGet(): FfiAccountGetReply {
        ensureAuthenticated()
        return accountRpc().get()
    }

    // (uploadProfile() removed: POST /api/v1/profile was deleted from the nest
    //  router with no WS-RPC replacement kind + no caller in any client — a dead
    //  byte-upload stub. A future profile-blob edit surface is net-new.)

    // -- Knocks (fauna.knocks.* WS-RPC; the HTTP twins were deleted in the
    // social-inbox T4 cutover). The connection actor replaces the old
    // `{actorId}` path param — it is accepted for call-site parity with the
    // other apps (mirrors apple APIClient.swift) but no longer routed. --

    suspend fun fetchKnocks(actorId: String): List<FfiKnockItem> {
        ensureAuthenticated()
        return contactsRpc().knocksList()
    }

    suspend fun acceptKnock(actorId: String, peerId: String) {
        ensureAuthenticated()
        contactsRpc().knocksAccept(peerId)
    }

    suspend fun blockKnock(actorId: String, peerId: String) {
        ensureAuthenticated()
        contactsRpc().knocksBlock(peerId)
    }

    // Unblock the guarded clear-the-edge twin of blockKnock: fauna.knocks.unblock
    // resets a `blocked` contact edge to None (contacts.md § Where logic lives →
    // Unblock). Consumes the shared fauna-client-contacts::knocks_unblock landed
    // 2026-06-22; drives the profile-block-button Block⇄Unblock toggle.
    suspend fun unblockKnock(actorId: String, peerId: String) {
        ensureAuthenticated()
        contactsRpc().knocksUnblock(peerId)
    }

    suspend fun dismissKnock(actorId: String, peerId: String) {
        ensureAuthenticated()
        contactsRpc().knocksDismiss(peerId)
    }

    // -- Contacts (fauna.contacts.* / fauna.knocks.* WS-RPC). --

    suspend fun fetchContacts(actorId: String): List<FfiContactItem> {
        ensureAuthenticated()
        return contactsRpc().contactsList()
    }

    suspend fun confirmContact(actorId: String, peerId: String) {
        ensureAuthenticated()
        contactsRpc().contactsConfirm(peerId)
    }

    // Knock-*sending* is deliberately NOT a local WS-RPC kind: it is a
    // federation ContactRequest delivered to the peer's inbox (gated by the
    // peer's InboxMode), per docs/goal/architecture/app-guidelines.md.
    // Composes the canonical signed (ContactRequest, Post) tuple via the
    // shared `build_knock_payload` writer (libs/fauna-client-core::email —
    // the "Knock"/"Contact request" wire sentinel is a Rust const, never a
    // per-app literal; priority #2/#4) and hands it to our home nest over
    // `fauna.inbox.send`. `recipientNestUrl = null` ⇒ same-nest local
    // delivery, faithful to the retired `POST /api/v1/contacts/{actor}/knock`
    // twin (a T4-cutover-deleted route with no server-side replacement — the
    // dead HTTP POST this replaced 404'd at runtime). Cross-nest delivery
    // awaits client-side peer discovery (federation.md). Mirrors windows
    // `NestRpcClient.{BuildKnockPayload,SendKnockAsync}` and linux
    // `build_and_send`.
    //
    // `recipientNestUrl` is the knock's route: `null` delivers on this nest (the
    // contacts page's Find User result, a bare id / same-nest lookup); the
    // profile page passes the shared `knockRecipientNestUrl` answer over the
    // profile its open already fetched, so the home nest originates the
    // federation leg (`profile.md` § Where logic lives → *Request contact
    // routing*). Throws `FfiException.GuardianApprovalRequired` when the
    // guardian gate refuses it — callers classify with [classifyKnock].
    suspend fun sendKnock(actorId: String, peerId: String, recipientNestUrl: String? = null) {
        ensureAuthenticated()
        val secretHex = secret ?: throw ApiException("No secret for knock")
        val payload = com.fauna.ffi.buildKnockPayload(
            HexUtil.hexToBytes(secretHex), HexUtil.hexToBytes(peerId), nodeUrl)
        inboxRpc().send(peerId, recipientNestUrl, payload)
    }

    // -- Resolution --
    // Recipient resolution moved to the shared Rust seam over UniFFI
    // (com.fauna.ffi.resolveNest / resolveHandle, calling the anonymous
    // fauna.nest.resolve / fauna.actor.by_handle discovery kinds); see
    // ResolveService. The GET /api/v1/resolve-node + /api/v1/actor/by-handle
    // HTTP twins are deleted nest-side (api-layers.md, Track A) and not called
    // from here.

    // -- Feed (fauna.feed.* WS-RPC; the HTTP twins were deleted in the T4
    // cutover). Rides the shared FfiFeedClient types directly — no per-app
    // feed model — matching windows + linux app.rs. --

    suspend fun listFeeds(): List<FfiFeedSummary> {
        ensureAuthenticated()
        return feedRpc().feedList()
    }

    suspend fun queryFeedPosts(feedId: String, cursor: Long? = null,
                                limit: Long? = null): FfiFeedPostsReply {
        ensureAuthenticated()
        return feedRpc().feedPosts(feedId, cursor, limit, null, null, null, null)
    }

    suspend fun queryLocalFeed(cursor: Long? = null,
                                limit: Long? = null): FfiFeedLocalPostsReply {
        ensureAuthenticated()
        return feedRpc().feedLocalPosts(cursor, limit, null)
    }

    /** `fauna.posts.create` — [payload] is the embed-as-bytes signed-post wire
     *  (built by the shared FFI `buildPost*`). Returns the new post id (hex). */
    suspend fun createPost(payload: ByteArray): String {
        ensureAuthenticated()
        return postsRpc().postsCreate(payload)
    }

    /** `fauna.posts.get` — raw resolved post bytes for the client's own/local
     *  posts (the kept `GET /api/v1/posts/{id}` is federation-only and must not
     *  be called from a client; the nest resolves remote posts server-side). */
    suspend fun getPost(postId: String): ByteArray {
        ensureAuthenticated()
        return postsRpc().postsGet(postId)
    }

    suspend fun interactWithPost(postId: String, action: String,
                                  body: String? = null) {
        ensureAuthenticated()
        postsRpc().postsInteract(postId, action, body)
    }

    /** `fauna.posts.get` — the native post-detail read (a single post per
     *  `docs/goal/ui/feed.md` § post_detail: author, body, tags, media). Replaces
     *  the broken `GET /api/v1/posts/{id}/detail` twin (a route the nest never
     *  served). The nest resolves remote (federated) posts server-side, so this
     *  covers every non-Bluesky source. Counts/viewer-state aren't part of the
     *  content read, so they default off (the detail screen hides zero counts);
     *  interactions still ride `fauna.posts.interact`. */
    suspend fun getPostDetail(postId: String, source: String = "fauna"): PostDetail {
        ensureAuthenticated()
        return decodePostFull(postsRpc().postsGet(postId)).toPostDetail(postId, source)
    }

    // -- Bluesky-native thread view (the one protocol-unique consume-side
    // Bluesky surface) on the `bluesky.feed.thread` WS-RPC kind via
    // FfiBlueskyClient, off the deleted GET /api/v1/bluesky/feed/thread/{uri}
    // HTTP twin (api-layers.md § Bluesky; bridges.md § Bluesky-native thread
    // view). The post-detail surface navigates from a crossposted *Fauna* post
    // (FfiFeedPostItem.post_id is the hex [u8;32] Fauna id, NOT the AT-URI — so
    // android, like linux, uses the PostId request variant; the handler
    // hex-decodes + resolves the AT-URI via the bluesky_posts crosspost
    // mapping). The kind returns a flat list (ancestors oldest-first, the focal
    // post, then its direct replies); we split it into the (parent, focal,
    // replies) shape PostDetailScreen renders. --

    suspend fun blueskyGetThread(faunaPostIdHex: String): BlueskyThreadResponse {
        ensureAuthenticated()
        val reply = blueskyRpc().threadByPostId(faunaPostIdHex)
        val posts = reply.posts
        if (posts.isEmpty()) throw ApiException("Empty Bluesky thread")
        // The nest names the focal post on the wire; the FFI seam passes it
        // through as `FfiBlueskyThreadReply.focalIndex` — no per-app guess.
        val focalIdx = reply.focalIndex.toInt()
        // Carry the Fauna post id onto the focal so its like/repost/reply
        // interactions ride fauna.posts.interact correctly (the wire id is the
        // bridge-side opaque id, which posts.interact does not key on).
        val focal = posts[focalIdx].toPostDetail().copy(postId = faunaPostIdHex)
        // Ancestors are oldest-first on the wire; reverse so the single parent
        // the UI shows (parents.firstOrNull) is the *direct* parent.
        val parents = posts.subList(0, focalIdx).asReversed().map { it.toPostDetail() }
        val replies = posts.subList(focalIdx + 1, posts.size).map { it.toPostDetail() }
        return BlueskyThreadResponse(post = focal, parents = parents, replies = replies)
    }

    // -- Unified Notifications (fauna.notifications.* WS-RPC; the HTTP twins
    // were deleted in the social-inbox T4 cutover). Rides FfiNotifItem /
    // FfiNotifListReply directly — no per-app notification model — matching
    // the feed/posts migration. The connection actor replaces the
    // old `{actorId}` path param. --

    suspend fun getNotifications(actorId: String, cursor: Long? = null, limit: Int = 25): FfiNotifListReply {
        ensureAuthenticated()
        return notificationsRpc().list(cursor, limit.toLong())
    }

    suspend fun markNotificationsRead(actorId: String, upTo: Long? = null) {
        ensureAuthenticated()
        // `upTo = null` ⇒ mark everything up to now (the twin's behavior).
        notificationsRpc().markRead(upTo)
    }

    suspend fun getUnreadNotificationCount(actorId: String): Int {
        ensureAuthenticated()
        return notificationsRpc().count().toInt()
    }

    // -- Calendars / Events (encrypted `bridge_caldav_*` store via FfiCaldavClient,
    //    events.md Decision B). Replaces the retired legacy plaintext
    //    `/api/{calendars,events}` REST + `fauna.{calendars,events}.*` path: the
    //    Events page now reads/writes the SAME encrypted store the mail-bridge MDA
    //    serves to Apple Calendar. `EventSummary.id` / `EventDetail.id` is the hex
    //    `uid_hash` (the write key); rsvp / reminder / delete address by it. The
    //    msek gate (mail not enabled) degrades to empty reads / "enable mail"
    //    write errors inside the seam. Invitations are iMIP (caldav-server.md
    //    § Scheduling); the invited-events list rides `queryInvitedEvents()`. --

    suspend fun listCalendars(): List<FaunaCalendar> {
        ensureAuthenticated()
        return caldavRpc().listCalendars().map {
            FaunaCalendar(
                id = it.id,
                name = it.name,
                color = it.color.ifBlank { null },
            )
        }
    }

    suspend fun createCalendar(name: String) {
        ensureAuthenticated()
        caldavRpc().createCalendar(name)
    }

    /** Import `.ics` into the calendar's encrypted store. `mode` is ignored on the
     *  encrypted path (events are upserted by `uid_hash`). */
    suspend fun importCalendar(calendarId: String, icsText: String, mode: String = "skip") {
        ensureAuthenticated()
        caldavRpc().importCalendarIcs(calendarId, icsText)
    }

    suspend fun exportCalendar(calendarId: String): String {
        ensureAuthenticated()
        return caldavRpc().exportCalendarIcs(calendarId)
    }

    // -- Address Book (CardDAV vCards — slice 4b, read-only) --
    //
    // The consumption analogue of the calendar reads above: the Contacts page's
    // "Address Book" segment lists the actor's OWN address books + vCards over the
    // encrypted `bridge_carddav_*` store, decrypted locally (carddav-server.md
    // § Independent enablement). The msek gate (mail not enabled) degrades to empty
    // reads inside the seam — no error, just an empty Address Book. The FfiCardRow /
    // FfiAddressbookRow records are already the display model (FN / EMAIL / TEL / ADR
    // / ORG / NOTE single-sourced by the shared crate), so they flow straight through
    // to the ViewModel with no re-mapping.

    suspend fun listAddressbooks(): List<FfiAddressbookRow> {
        ensureAuthenticated()
        return carddavRpc().listAddressbooks()
    }

    suspend fun queryCards(addressbookId: String): List<FfiCardRow> {
        ensureAuthenticated()
        return carddavRpc().queryCards(addressbookId)
    }

    /**
     * Resolve a `SearchNav.Contact` row's `uid_hash` to the Address Book's
     * `card_id` (search.md § Where logic lives → *Result navigation (deep
     * link)*) — the shared `CardDavClient::locate_card_by_uid_hash` over
     * `fauna.bridges.{list_addressbooks,query_cards}`, so a card in a book
     * the user never opened is reachable off one round of reads. Never a
     * client-side cast: `uid_hash` and `card_id` are different id spaces of
     * the same width, so matching one for the other would compile, run, and
     * open nothing. `FfiLocatedCard.found == null` is the DROPPED outcome
     * (deleted since it was indexed), not an error.
     */
    suspend fun locateCardByUidHash(uidHashHex: String): FfiLocatedCard {
        ensureAuthenticated()
        return carddavRpc().locateCardByUidHash(uidHashHex)
    }

    // -- Events --

    suspend fun queryEvents(calendarId: String): List<EventSummary> {
        ensureAuthenticated()
        return caldavRpc().queryEvents(calendarId).map { it.toSummary() }
    }

    /** The "invited" section — events across all calendars the actor was invited
     *  to but does not organize. `filter` is ignored on the encrypted path. */
    suspend fun queryMyEvents(filter: String): List<EventSummary> {
        ensureAuthenticated()
        return caldavRpc().queryInvitedEvents().map { it.toSummary() }
    }

    /** The encrypted store serves no server-side date filter (bodies are opaque) —
     *  the grids window client-side, so the range is ignored, matching Linux. */
    suspend fun queryEventsInRange(
        calendarId: String,
        after: String,
        before: String
    ): List<EventSummary> = queryEvents(calendarId)

    suspend fun createEvent(request: CreateEventRequest) {
        ensureAuthenticated()
        caldavRpc().createEvent(
            request.calendarId,
            request.summary,
            request.dtstart,
            request.dtend,
            request.location ?: "",
            request.description ?: "",
        )
    }

    suspend fun getEvent(eventId: String): EventDetail {
        ensureAuthenticated()
        val ev = caldavRpc().getEvent(eventId) ?: throw ApiException("Event not found")
        return ev.toDetail()
    }

    suspend fun deleteEvent(eventId: String) {
        ensureAuthenticated()
        caldavRpc().deleteEvent(eventId)
    }

    suspend fun listEventAttendees(eventId: String): List<Attendee> {
        ensureAuthenticated()
        return caldavRpc().getEvent(eventId)?.attendees.orEmpty().map {
            Attendee(email = it.email, name = it.name, rsvp = it.rsvp)
        }
    }

    suspend fun rsvpEvent(eventId: String, response: RsvpResponse) {
        ensureAuthenticated()
        caldavRpc().rsvpEvent(eventId, response)
    }

    /** Invite an attendee by **email** — the universal CalDAV/iMIP attendee
     *  identifier (the seam adds a `mailto:` ATTENDEE + fans out an iMIP REQUEST).
     *  The cross-nest mailbox-less (`nest_url`) route is a captured follow-on. */
    suspend fun inviteAttendee(eventId: String, email: String) {
        ensureAuthenticated()
        caldavRpc().inviteAttendee(eventId, email)
    }

    // -- Event Reminders (the single VEVENT VALARM offset on the encrypted body) --

    suspend fun getReminder(eventId: String): String? {
        ensureAuthenticated()
        return caldavRpc().getEvent(eventId)?.reminder
    }

    suspend fun setReminder(eventId: String, offset: String) {
        ensureAuthenticated()
        caldavRpc().setReminder(eventId, offset)
    }

    /** Clear the reminder — `setReminder` with an empty offset on the seam. */
    suspend fun removeReminder(eventId: String) {
        ensureAuthenticated()
        caldavRpc().setReminder(eventId, "")
    }

    /** The grid/list view of a decoded encrypted-store event. */
    private fun FfiCalEvent.toSummary(): EventSummary = EventSummary(
        id = id,
        uid = uid,
        summary = summary,
        dtstart = dtstart,
        dtend = dtend,
        calendarId = calendarId,
    )

    /** The detail-panel view of a decoded encrypted-store event. */
    private fun FfiCalEvent.toDetail(): EventDetail = EventDetail(
        id = id,
        uid = uid,
        summary = summary,
        dtstart = dtstart,
        dtend = dtend,
        description = description,
        location = location,
        organizer = organizer,
        organizedByMe = organizedByMe,
        calendarId = calendarId,
    )

    // -- Account Settings --

    // fauna.profile.handle.change WS-RPC (the PUT /api/v1/profile/handle twin
    // was deleted in the account T3 cutover). The handle change is now a
    // delayed pending action server-side; callers here only key off
    // success/exception, so the reply is intentionally not surfaced.
    suspend fun changeHandle(newHandle: String) {
        ensureAuthenticated()
        accountRpc().changeHandle(newHandle)
    }

    // fauna.account.delete WS-RPC (the DELETE /api/v1/account twin was deleted
    // in the account T3 cutover). A delayed, cancellable pending action — the
    // caller stays signed in on success (ruled 2026-08-26), so the reply is
    // not surfaced beyond the success/exception split.
    suspend fun deleteAccount() {
        ensureAuthenticated()
        accountRpc().delete()
    }

    // fauna.pending_actions.* WS-RPC (settings.md § Pending actions) — the
    // cancellation window the three delayed verbs above open.

    /** This actor's queued destructive operations, filtered to still-`pending`
     *  rows (an executed/cancelled/expired action has no cancel window left) —
     *  mirrors tui's/linux's own `list_pending_actions` helper. The wire
     *  reply carries ALL statuses, unfiltered. */
    suspend fun pendingActionsList(): List<FfiPendingActionSummary> {
        ensureAuthenticated()
        return accountRpc().pendingActionsList().filter { it.status == "pending" }
    }

    /** Cancel a scheduled action before it executes (one click, no confirm —
     *  cancelling is the safe direction). */
    suspend fun pendingActionCancel(id: Long) {
        ensureAuthenticated()
        accountRpc().pendingActionCancel(id)
    }

    suspend fun exportData(): ByteArray {
        ensureAuthenticated()
        // `include_blobs=true` is what makes this the whole archive rather than
        // an index of it — the nest defaults the flag off. Not a user choice:
        // account-data-plane.md § Nest-side requirements item 1, Payload stores
        // decision (5).
        val request = authorizedRequest("$nodeUrl/api/v1/export?include_blobs=true").build()
        return executeBytes(request)
    }

    // -- Privacy: Inbox Mode (fauna.inbox.mode.* WS-RPC; the HTTP twins were
    // deleted in the social-inbox T4 cutover). The connection actor replaces
    // the old `{actorId}` path param. --

    suspend fun getInboxMode(actorId: String): String {
        ensureAuthenticated()
        return contactsRpc().inboxModeGet()
    }

    suspend fun setInboxMode(actorId: String, mode: String) {
        ensureAuthenticated()
        contactsRpc().inboxModeSet(mode)
    }

    // -- Privacy: Email Filters --

    suspend fun listEmailFilters(): List<FfiEmailFilter> = emailRpc().filtersList()

    suspend fun createEmailFilter(
        name: String,
        rules: List<FfiEmailFilterRule>,
        combination: String,
        action: FfiEmailFilterAction,
        priority: Int,
    ): Long = emailRpc().filtersCreate(name, rules, combination, action, priority)

    suspend fun getEmailFilter(id: Long): FfiEmailFilter = emailRpc().filtersGet(id)

    suspend fun updateEmailFilter(
        id: Long,
        name: String,
        rules: List<FfiEmailFilterRule>,
        combination: String,
        action: FfiEmailFilterAction,
        priority: Int,
    ) {
        emailRpc().filtersUpdate(id, name, rules, combination, action, priority)
    }

    suspend fun deleteEmailFilter(id: Long) {
        emailRpc().filtersDelete(id)
    }

    // -- Privacy: Spam Preferences (fauna.spam.* WS-RPC) --
    // Migrated off the GET|PUT /api/v1/spam/preferences HTTP twin onto the
    // shared fauna.spam.{get,set}_preferences kinds via FfiSpamClient — the
    // native-client seam onto what the Rust-native Linux app calls
    // fauna_client_spam::SpamClient for directly. The wire carries the
    // thresholds as per-mille UShort (0–1000); the VM presents them as a
    // 0.0–1.0 slider, converting on each edge.

    suspend fun getSpamPreferences(): FfiSpamPreferences =
        spamRpc().getPreferences()

    /** Full save from the Privacy UI — every field is sent (the WS-RPC kind is
     *  a partial update, but the UI always carries the complete set). */
    suspend fun updateSpamPreferences(
        spamThreshold: UShort,
        phishingThreshold: UShort,
    ): FfiSpamPreferences =
        spamRpc().setPreferences(spamThreshold, phishingThreshold)

    // -- Bridges --

    suspend fun listBridges(): List<FfiBridgeStatus> = bridgesRpc().list()

    suspend fun linkBridge(
        bridgeId: String,
        mode: String,
        fields: Map<String, String> = emptyMap(),
    ): FfiLinkReply {
        // `mode` rides as its own argument; `params` carries only the per-mode
        // field values (template: apps/fauna-linux/src/views/bridges/detail.rs).
        val params = FfiCborValue.Map(
            fields.map { (k, v) -> FfiCborEntry(k, FfiCborValue.Text(v)) }
        )
        return bridgesRpc().link(bridgeId, mode, params)
    }

    suspend fun unlinkBridge(bridgeId: String) {
        bridgesRpc().unlink(bridgeId)
    }

    suspend fun updateBridgeSettings(bridgeId: String, settings: FfiCborValue) {
        bridgesRpc().setSettings(bridgeId, settings)
    }

    suspend fun listBridgeFollows(bridgeId: String): List<FfiBridgeFollow> =
        bridgesRpc().listFollows(bridgeId)

    suspend fun addBridgeFollow(
        bridgeId: String,
        id: String,
        petname: String? = null,
        extra: FfiCborValue? = null,
    ) {
        bridgesRpc().addFollow(bridgeId, id, petname, extra)
    }

    suspend fun removeBridgeFollow(bridgeId: String, followId: String) {
        bridgesRpc().removeFollow(bridgeId, followId)
    }

    // -- Nostr Connect / NIP-46 bunker (nostr.md § The nest as the user's
    // NIP-46 signer) --
    //
    // Thin pass-through to the shared `fauna.nostr.bunker.*` WS-RPC kinds via
    // the UniFFI `FfiNostrBunkerClient` (obtained from `FfiNestClient.nostrBunker()`),
    // the same shape as [familyClient]. Drives the Nostr page's *Connected
    // apps* section.

    private fun nostrBunkerRpc(): com.fauna.ffi.FfiNostrBunkerClient = nestRpc().nostrBunker()

    /** `fauna.nostr.bunker.create_invite` — mint a pending connection; the
     *  reply is the single one-time reveal of the connect string. */
    suspend fun nostrBunkerCreateInvite(): com.fauna.ffi.FfiCreateBunkerInviteReply =
        nostrBunkerRpc().createInvite()

    /** `fauna.nostr.bunker.list` — the caller's connection roster (pending +
     *  active rows). */
    suspend fun nostrBunkerList(): List<com.fauna.ffi.FfiBunkerAppEntry> = nostrBunkerRpc().list()

    /** `fauna.nostr.bunker.revoke` — immediate disconnect of one connection. */
    suspend fun nostrBunkerRevoke(connectionId: Long): Boolean = nostrBunkerRpc().revoke(connectionId)

    // -- Zap signers (the NIP-57 trust root — `monetization.md` § Zap
    // receipts — the trust model) --
    //
    // `zaps` is a subset member of `payments` (`dynamic-features.md` §
    // Charter members) and excises with it, so — unlike [nostrBunkerRpc]
    // above, which this used to mirror as a member here — this glue cannot
    // live in this shared class: a store-safe build's generated bindings
    // carry no `FfiNostrZapSignerClient`/`FfiZapSignerEntry` at all, and a
    // member cannot be removed per build variant while the class stays
    // shared. It lives instead as `ApiClient` extension functions in the
    // `src/payments`/`src/noPayments` variant source sets
    // (`com.fauna.app.payments.ZapSignerGlue.kt`, the `PaymentsGlue.kt`
    // pattern) — the 2026-08-28 regression this split fixes (§ Platform-family
    // surface excision's android row records it).

    // MLS key-package pool (publish + count) moved onto the
    // fauna.conversations.keypackage.{upload,count} WS-RPC kinds via
    // FfiConversationsClient — see [conversationsRpc] + MlsManager. The
    // POST|GET /api/v1/keypackage/{actor} HTTP twins were deleted at T8.

    // -- Moderation (fauna.moderation.* WS-RPC) --
    // `submitScanReport` was removed 2026-07-19 (the client-side compliance-
    // beacon producer was retired without replacement — `moderation.md` § State
    // & data shape) and the `fauna.moderation.scan_report` kind itself left the
    // wire 2026-09-24 with the compat-remnant sweep.

    /**
     * `fauna.moderation.actions` — read the caller's own moderation queue (one
     * [FfiObligationAction] per flagged / actioned piece of the caller's content),
     * for the standalone Moderation page (`moderation.md` § State & data shape).
     * Scoped to the connection actor; an empty list is the empty state, not an error.
     */
    suspend fun moderationActions(): List<FfiObligationAction> =
        moderationRpc().actions()

    /**
     * `fauna.moderation.train` — submit a spam/ham training correction for one
     * queue item (the Moderation page's `train-correction-button` sends `"ham"` —
     * a false-positive correction). Trains the caller's Bayesian model nest-side;
     * the reply is discarded (`moderation.md` § User actions).
     */
    suspend fun submitModerationTrain(contentId: String, verdict: String) {
        moderationRpc().train(contentId, verdict)
    }

    /**
     * `fauna.moderation.legal_takedown` — the Admin-only legal-compulsion
     * takedown/overturn (`moderation.md` § Legal takedown → Invocation
     * surface). `conversation` selects the
     * MLS relay-withhold kind, else post; `restore` overturns (the reference
     * becomes the optional note). The reply's status is discarded here — the
     * caller renders [com.fauna.ffi.takedownVerdict], the shared wording.
     */
    suspend fun legalTakedown(
        contentId: String,
        conversation: Boolean,
        legalReference: String,
        restore: Boolean,
    ) {
        moderationRpc().legalTakedown(contentId, conversation, legalReference, restore)
    }

    /**
     * `fauna.moderation.report_share.set` — set the caller's distributed,
     * k-anonymous report-sharing opt-in (`report-sharing.md` § Client wire;
     * default off). `share=false` also withdraws every report the caller
     * contributed (nest-side opt-out sweep). Returns the state now in effect.
     */
    suspend fun reportShareSet(share: Boolean): Boolean =
        moderationRpc().reportShareSet(share)

    /**
     * `fauna.moderation.report_share.status` — read the caller's opt-in state
     * plus the transparency list of `≥k` aggregates this nest publishes to
     * peers (the mail-spam `report-share-published-list`).
     */
    suspend fun reportShareStatus(): com.fauna.ffi.FfiReportShareStatus =
        moderationRpc().reportShareStatus()

    // ── Per-account spam-threshold override (mail-policy-config.md § Tier 3) — the mail-spam page's threshold input. A plain
    // fauna.bridges.{get,set}_spam_threshold_override RPC pair, not the
    // MailSpamMachine, same shape as the report-share flow above.

    /** Read the caller's per-account spam-folder threshold override, or `null`
     *  when the account follows the admin default. */
    suspend fun spamThresholdOverrideGet(): UInt? =
        com.fauna.ffi.spamThresholdOverrideGet(nestRpc())

    /** Set (or clear, with `null`) the override; returns the persisted value
     *  the nest confirmed, never the local edit. `0u` is a real setting — it
     *  turns automatic Junk filing off for this account, distinct from `null`. */
    suspend fun spamThresholdOverrideSet(value: UInt?): UInt? =
        com.fauna.ffi.spamThresholdOverrideSet(nestRpc(), value)

    // ── Moderation-queue local detections (moderation.md § Layout & flow) ──
    //
    // The `moderation-queue` is the UNION of the server `fauna.moderation.actions`
    // obligations and the client's own post-decrypt local detections — the only
    // social-content signal in encrypted mode, where the nest can't classify. The
    // shared writer (`ConversationsManager::ingest_inbound_to_thread`) `observe`s
    // each just-decrypted inbound message into the session-owned `LocalDetectionStore`
    // (no per-app writer); these three reads are the queue VM's window onto it.
    // They no-op (empty / false / null) when no session is active (pre-login) — never
    // an error, mirroring linux `active_session().map(...).unwrap_or_default()`.

    /** Post-decrypt local spam detections held in the live conversations session. */
    fun moderationLocalDetections(): List<uniffi.fauna_client_moderation.LocalDetection> =
        conversationsManagerHost.session?.moderationLocalDetections() ?: emptyList()

    /** Drop a local detection after a train correction (web's `removeFlagged`);
     *  false when absent / no session. */
    fun moderationRemoveLocalDetection(contentId: String): Boolean =
        conversationsManagerHost.session?.moderationRemoveLocalDetection(contentId) ?: false

    /** The retained plaintext body of a locally-detected message, for the
     *  client-side sealed spam train (mail-spam.md § Encrypted-mode interaction);
     *  null once the message ages out / no session. */
    fun moderationMessageBody(contentId: String): String? =
        conversationsManagerHost.session?.moderationMessageBody(contentId)

    /** The plaintext body of one of the caller's own posts (`fauna.posts.get` →
     *  shared `decodePostFull`), for the client-side sealed spam train on a **server**
     *  queue row (mail-spam.md § Encrypted-mode interaction — the same read-authz gate
     *  the server train enforces). Throws on an RPC/decode failure; the caller falls
     *  through to `fauna.moderation.train`, mirroring linux `train_moderation_flow`'s
     *  `if let Ok(reply)`. */
    suspend fun postBodyText(contentId: String): String {
        ensureAuthenticated()
        return com.fauna.ffi.decodePostFull(postsRpc().postsGet(contentId)).body
    }

    // ── Muted keywords (moderation.md § Muted keywords / content-moderation § Q3) ──
    //
    // The user-global sealed muted-word list, held in the `fauna.state.moderation` plane entry
    // (nest-opaque, no WS-RPC kind). CRUD rides the shared `preference_surfaces` seam via the
    // FFI free-fns, over the runtime's account store (no key crosses); the collapse match at
    // render is the pure `matchesMutedKeywords` (called directly). A *hide/collapse*, never a
    // spam-queue flag — it does NOT feed the LocalDetectionStore / moderation queue.

    /** Read the muted-word list (normalized, sealed client-side). */
    suspend fun mutedKeywordsList(): uniffi.fauna_client_config.MutedWordsSnapshot =
        com.fauna.ffi.loadMutedWords()

    /** Set the whole muted-word list (terms and weights); returns the normalized
     *  stored list (trim / drop-blanks / case-insensitive dedupe / weight clamp
     *  done shared-side).
     *  ⚠ Whole-list intents only — the page's add/remove buttons go through
     *  the delta pair below. */
    suspend fun mutedKeywordsSet(
        keywords: List<uniffi.fauna_core.MutedKeyword>,
    ): uniffi.fauna_client_config.MutedWordsSnapshot =
        com.fauna.ffi.saveMutedWords(keywords)

    /** Add one term as a DELTA against the stored list — the shared seam
     *  re-reads it inside its own CAS update, so a concurrent device's term
     *  survives. Re-adding an existing term is a no-op. */
    suspend fun mutedKeywordsAdd(word: String): uniffi.fauna_client_config.MutedWordsSnapshot =
        com.fauna.ffi.addMutedWord(word)

    /** Remove one term — the delta pair's inverse; removing a term already
     *  gone is a success no-op. */
    suspend fun mutedKeywordsRemove(word: String): uniffi.fauna_client_config.MutedWordsSnapshot =
        com.fauna.ffi.removeMutedWord(word)

    // ── Unattested-member review ──
    //
    // The permanent Settings sub-page's data seam — thin wrappers over
    // `libs/fauna-ffi/src/member_review.rs` (zero logic owed, priority #2).
    // `memberReviewRemove` never takes a verdict: it always evicts first and
    // persists only what the eviction earned, so this app cannot record
    // `Removed` from its own reasoning — the one seam that can write it calls
    // the one function that can earn it.

    /** Read the open review roster (`fauna_client_config::load_member_reviews`
     *  via the shared FFI face). */
    suspend fun memberReviewList(): List<com.fauna.ffi.FfiMemberReview> =
        com.fauna.ffi.memberReviewsList()

    /** The shared row-text parts for one review item — consumed, never
     *  re-derived (`fauna_core::data::review_row_text`'s reason-selection and
     *  unnameable-person rules live once, shared). `handle` must be resolved
     *  BEFORE a Remove call for the same person: `handleForPerson` reads live
     *  membership, so there is no seat left to read one off afterward. */
    fun memberReviewRowText(
        person: ByteArray,
        reasons: List<String>,
        handle: String?,
    ): uniffi.fauna_core.MemberReviewRowText =
        com.fauna.ffi.memberReviewRowText(person, reasons, handle)

    /** The handle `person` is seated under, in the owner's own conversations —
     *  `None` when conversations are not up yet or they hold no seat the
     *  manager can name. */
    fun memberReviewHandleForPerson(person: ByteArray): String? =
        conversationsManagerHost.manager.handleForPerson(person)

    /** The review mark each of `threadId`'s member chips carries —
     *  index-parallel with `ThreadDetail.participantDisplays`, hence with
     *  the `thread-member-chip[i]` those displays render: the flagged
     *  person's actor id (raw bytes) at a chip under open review, `null` at
     *  every other chip. `roster` is the caller's cached [memberReviewList]
     *  result — this answers per-row from that cache
     *  (`fauna_conversations::member_review_flags`, the same shared join
     *  web's `memberReviewMarksForThread` and linux's `is_under_review` call
     *  use), never a hand-rolled scan (`libs/fauna-ffi/src/member_review.rs`
     *  — the projection itself carries no UniFFI export, exactly so no app
     *  hand-rolls this join). */
    fun memberReviewMarksForThread(
        threadId: String,
        roster: List<com.fauna.ffi.FfiMemberReview>,
    ): List<ByteArray?> =
        com.fauna.ffi.memberReviewMarksForThread(conversationsManagerHost.manager, threadId, roster)

    /** Record **Keep** — closes every open item for `person` with no group
     *  changes. Returns whether anything was actually open (a concurrent
     *  device may have already answered — a success no-op, never an error). */
    suspend fun memberReviewKeep(person: ByteArray): Boolean =
        com.fauna.ffi.memberReviewKeep(person)

    /** Record **Remove** — evicts `person` from every group of the owner's
     *  they are in NOW (re-derived, never from the stored item), then
     *  persists only whatever verdict the eviction earned. A partial eviction
     *  earns none: the review item stays open, and the returned
     *  [uniffi.fauna_conversations.CrossGroupEviction]'s `evicted`/`failed`/
     *  `unreachable` fields are what the caller composes its own message
     *  from — the derivation is shared, the wording per-app. */
    suspend fun memberReviewRemove(person: ByteArray): uniffi.fauna_conversations.CrossGroupEviction =
        com.fauna.ffi.memberReviewRemove(conversationsManagerHost.manager, person)

    // ── Trained topics (topic-factors.md § Authoring surface & picker (S8) /
    // § Publishing a trained factor) ──
    //
    // Thin wrappers over the SHARED `fauna_client_personalization::topics::
    // TrainedTopics` lifecycle (the registry↔model-plane sequencing — advisory
    // example-count read, create cap, delete's registry-removal-then-
    // `model.delete` pairing — is not re-derived here, priority #2) and the
    // shared `publish_trained_factor_list` publish lifecycle (derive the
    // per-factor keypair → resolve next version off the catalog → build + sign
    // → `fauna.labelers.publish`).

    suspend fun trainedTopicsList(): List<FfiTrainedTopicRow> =
        com.fauna.ffi.listTrainedTopics(nestRpc())

    suspend fun trainedTopicsCreate(name: String): List<FfiTrainedTopicRow> =
        com.fauna.ffi.createTrainedTopic(nestRpc(), name)

    suspend fun trainedTopicsRename(id: ByteArray, name: String): List<FfiTrainedTopicRow> =
        com.fauna.ffi.renameTrainedTopic(nestRpc(), id, name)

    suspend fun trainedTopicsSetLearnFromEngagement(id: ByteArray, on: Boolean): List<FfiTrainedTopicRow> =
        com.fauna.ffi.setTrainedTopicEngagement(nestRpc(), id, on)

    suspend fun trainedTopicsDelete(id: ByteArray): List<FfiTrainedTopicRow> =
        com.fauna.ffi.deleteTrainedTopic(nestRpc(), id)

    /** Publish a trained factor's kept exemplars as a tier-3 List labeler
     *  (topic-factors.md § Publishing a trained factor). [name] is the
     *  publisher-chosen PUBLIC name — the sealed registry name stays private. */
    suspend fun publishTrainedFactorList(
        factorId: ByteArray,
        name: String,
        entries: List<FfiPublishEntry>,
    ): FfiPublishedList =
        com.fauna.ffi.trainedTopicPublishList(nestRpc(), ownerSecretBytes(), factorId, name, entries)

    /** Publish a trained factor as a tier-3 Model labeler (topic-factors.md §
     *  Publishing a trained factor, v2) — the scrubbed n-gram vocabulary.
     *  [moreDocs]/[lessDocs] are the corpus counters `scrubCorpusForFactor`
     *  returned, passed through UNSHRUNK regardless of what the review pruned
     *  — they say how many public examples the vocabulary was built from,
     *  which stays true however much of it the caller withheld. */
    suspend fun publishTrainedFactorModel(
        factorId: ByteArray,
        name: String,
        moreDocs: UInt,
        lessDocs: UInt,
        ngrams: List<FfiPublishNgram>,
    ): FfiPublishedModel =
        com.fauna.ffi.trainedTopicPublishModel(
            nestRpc(), ownerSecretBytes(), factorId, name, moreDocs, lessDocs, ngrams,
        )

    // -- Family safety (family-safety.md § App surface) --
    //
    // Thin pass-through to the shared `fauna.family.*` WS-RPC kinds via the
    // UniFFI `FfiFamilyClient` (obtained from `FfiNestClient.family()`), the same
    // shape as [subscriptionsClient]. Drives the Family page (guardian + supervised
    // sections, the incoming-transfer prompt), the gated `family-tab`, and the
    // global `supervised-indicator`.

    private fun familyClient(): com.fauna.ffi.FfiFamilyClient = nestRpc().family()

    /** `fauna.family.status` — the caller's family relationships, both roles in
     *  one read; drives the supervised indicator, `family-tab` gate, and the page.
     *
     *  Every **successful** read also rewrites the caller's persisted last-known
     *  supervision snapshot (family-safety.md § Content policy, clause 2 —
     *  "written on every successful status read"). This method is android's one
     *  choke point for the read — the indicator poll, [ContentPolicyStore], the
     *  family gate and FamilyVM all come through here — so persisting here keeps
     *  the clause true for every present and future caller. The fold (and its
     *  graduation gate) is the shared `SupervisionSnapshot::from_status` behind
     *  the FFI; a failed read throws before the write, which is clause 1 by
     *  construction. Keyed on the actor whose session made the read. */
    suspend fun familyStatus(): com.fauna.ffi.FfiFamilyStatus {
        val status = familyClient().status()
        sessionActorHex()?.let { accountStores.persistSupervisionSnapshot(it, status) }
        // The same choke point keeps the ward's own asks fresh for the
        // refused-send surfaces (contacts, profile, bridges) — gated on
        // `supervised_by` for the same reason the snapshot is: a graduated
        // account has no guardian to be waiting on.
        wardAsks.setFromStatus(status.supervisedBy != null, status.contactRequests, status.feedRequests)
        return status
    }

    /** The actor id (lowercase hex) of the session this client authenticates
     *  as, derived from its own cached secret — never the registry's active
     *  pointer, which a mid-switch transient can move first. */
    private fun sessionActorHex(): String? = secret?.let {
        runCatching {
            com.fauna.ffi.hexFull(com.fauna.ffi.actorIdFromSecret(HexUtil.hexToBytes(it)))
        }.getOrNull()
    }

    /** `fauna.family.policy.update` — replace a ward's reach policy (guardian-only). */
    suspend fun familyPolicyUpdate(supervisedActorId: ByteArray, policy: com.fauna.ffi.FfiReachPolicy) =
        familyClient().policyUpdate(supervisedActorId, policy)

    /** `fauna.family.notify_report` — the supervised caller's coarse per-category
     *  Guardian Notify counts (family-safety.md § Guardian Notify). A silent
     *  nest-side no-op unless the caller is supervised with `content_notify` on. */
    suspend fun familyNotifyReport(entries: List<com.fauna.ffi.FfiFamilyContentNotice>, utcOffsetMinutes: Int) =
        familyClient().notifyReport(entries, utcOffsetMinutes)

    /** `fauna.family.approvals.list` — the guardian's pending reach approvals
     *  across every ward (not ward-scoped — one read for the whole queue). */
    suspend fun familyApprovalsList(): List<com.fauna.ffi.FfiFamilyApprovalEntry> =
        familyClient().approvalsList()

    /** `fauna.family.approvals.decide` — approve/deny one queued item. Pass the
     *  key the `kind` names and leave the others empty; a list entry carries all
     *  of them. `contact`/`contact_request` are keyed by `peerActorId`; a
     *  `mail_hold` by `messageId`; a `feed_source` by the whole
     *  `(bridgeId, operation, target)` triple (`target` empty for a `link`; the
     *  label is never part of the key); a `dm_hold` by `(bridgeId, peerAddress)`. */
    suspend fun familyApprovalsDecide(
        supervisedActorId: ByteArray,
        kind: String,
        peerActorId: ByteArray,
        messageId: ByteArray,
        bridgeId: String,
        operation: String,
        target: String,
        peerAddress: String,
        approve: Boolean,
    ) = familyClient().approvalsDecide(
        supervisedActorId,
        kind,
        peerActorId,
        messageId,
        bridgeId,
        operation,
        target,
        peerAddress,
        approve,
    )

    /** The guardian's un-deny of one denied bridge-DM peer (family-safety.md
     *  § The bridge-DM gate → *The un-deny surface*) — the shared
     *  `allowBlockedPeer`, which owns the approving `dm_hold` decide's shape. */
    suspend fun familyAllowBlockedPeer(supervisedActorId: ByteArray, peer: com.fauna.ffi.FfiFamilyBlockedPeer) =
        familyClient().allowBlockedPeer(supervisedActorId, peer)

    /** `fauna.family.contact.request` — the supervised ward's ask, offered only
     *  after a knock came back [KnockSend.RefusedByGuardian] (family-safety.md
     *  § Child-initiated contact requests → *App affordance*). On success it
     *  re-reads the ward's own asks, so what paints is what the NEST holds; a
     *  failed re-read is not a failed ask (the guardian has been rung), so it
     *  degrades to "keep what [wardAsks] holds" and the caller's just-asked
     *  flag carries the render. Mirrors linux `ask_contact` / tui
     *  `contacts::ask_guardian`. */
    suspend fun familyContactRequest(peerActorIdHex: String) {
        familyClient().contactRequest(HexUtil.hexToBytes(peerActorIdHex))
        wardAsks.replaceContactRequests(
            runCatching { familyStatus().contactRequests }.getOrDefault(emptyList())
        )
    }

    /** `fauna.family.feed_source.request` — the ward's "ask your guardian"
     *  beside a `feed_sources` refusal (family-safety.md § Feed-source
     *  approvals); same re-read-and-keep-on-failure shape as
     *  [familyContactRequest]. `label` is display-only, never authorizing. */
    suspend fun familyFeedSourceRequest(triple: FeedTriple, label: String) {
        familyClient().feedSourceRequest(triple.bridgeId, triple.operation, triple.target, label)
        wardAsks.replaceFeedRequests(
            runCatching { familyStatus().feedRequests }.getOrDefault(emptyList())
        )
    }

    /** `fauna.family.contact.add` — pre-approve a contact on the ward's behalf. */
    suspend fun familyContactAdd(supervisedActorId: ByteArray, peerActorId: ByteArray) =
        familyClient().contactAdd(supervisedActorId, peerActorId)

    /** `fauna.family.device.mark` — set/clear the guardian-enrolled-device
     *  marker on one of the ward's devices (family-safety.md § Full
     *  visibility for young children, Slice F). Not batched behind
     *  `familyPolicyUpdate` — this is its own per-device RPC. */
    suspend fun familyDeviceMark(supervisedActorId: ByteArray, deviceId: String, marked: Boolean) =
        familyClient().deviceMark(supervisedActorId, deviceId, marked)

    /** `fauna.family.graduate` — supervised → full account, in place. */
    suspend fun familyGraduate(supervisedActorId: ByteArray) =
        familyClient().graduate(supervisedActorId)

    /** `fauna.family.transfer` — propose a new guardian; pending until accepted
     *  (a self-proposal completes immediately). */
    suspend fun familyTransfer(supervisedActorId: ByteArray, newGuardianActorId: ByteArray) =
        familyClient().transfer(supervisedActorId, newGuardianActorId)

    /** `fauna.family.transfer.accept` — consent to a proposal naming the caller. */
    suspend fun familyTransferAccept(supervisedActorId: ByteArray) =
        familyClient().transferAccept(supervisedActorId)

    /** `fauna.family.transfer.decline` — refuse a proposal naming the caller. */
    suspend fun familyTransferDecline(supervisedActorId: ByteArray) =
        familyClient().transferDecline(supervisedActorId)

    /** `fauna.family.transfer.cancel` — withdraw the ward's pending proposal. */
    suspend fun familyTransferCancel(supervisedActorId: ByteArray) =
        familyClient().transferCancel(supervisedActorId)

    /** `fauna.family.usage_report` — the **supervised** caller's screen-time
     *  heartbeat (family-safety.md § Screen time, Slice E): reports foreground
     *  minutes since the last successful report and gets back the nest-stamped
     *  local day + that day's cross-device total. Driven by [com.fauna.app.core.ScreenTimeStore]. */
    suspend fun familyUsageReport(minutes: UInt, utcOffsetMinutes: Int): com.fauna.ffi.FfiFamilyUsageReport =
        familyClient().usageReport(minutes, utcOffsetMinutes)

    // -- Admin --

    // fauna.account.am_i_admin WS-RPC (the GET /api/v1/am-i-admin twin was
    // deleted in the account T3 cutover). Non-admin / unreachable → false.
    suspend fun checkIsAdmin(): Boolean {
        return try {
            ensureAuthenticated()
            accountRpc().amIAdmin()
        } catch (_: Exception) {
            false
        }
    }

    /**
     * `fauna.admin.stats` — nest-wide counters for the admin dashboard. Rides
     * the persistent WS-RPC connection (the deleted `GET /admin/api/stats` twin
     * was removed in the WS-RPC-everywhere rip-out).
     */
    suspend fun fetchAdminStats(): FfiAdminStats = adminRpc().stats()

    // -- Admin user-administration hub (admin-users — admin.md § Users) --
    //
    // Thin pass-through to the shared `fauna.admin.*` WS-RPC kinds via the
    // UniFFI `FfiAdminClient` (obtained from `FfiNestClient.admin()`), mirroring
    // `bridgesRpc()`/`emailRpc()`. New admin surfaces go through these — NOT the
    // deleted `/admin/api/*` HTTP twins (no-http-ws-rpc-everywhere).

    /** `fauna.admin.users.list` — a page of users + the unpaginated total. */
    suspend fun adminUsersList(limit: Long? = null, offset: Long = 0): FfiAdminUsersListReply =
        adminRpc().usersList(limit, offset)

    /**
     * Every account on the nest, newest first — `fauna_client_admin::users_list_all`
     * (admin.md § 2 → *Which accounts a picker offers*). Every admin actor
     * picker (DNS catch-all/role-address, web apex, the guardian pickers)
     * reads from this, never a single [adminUsersList] page.
     */
    suspend fun adminUsersListAll(): List<FfiAdminUser> = adminRpc().usersListAll()

    /**
     * `fauna.admin.users.create` — admit a known actor id directly, the third
     * account-creation path (`admin-users-admit-*`; public-mode.md §
     * Registration & Identity). [handle] `null` admits the deliberate
     * handle-less state (§ A handle-less account); there is no set-later. A
     * duplicate actor or taken handle is `fauna.admin.conflict`, a
     * malformed/reserved handle `fauna.admin.invalid_params`.
     */
    suspend fun adminUsersCreate(actorId: ByteArray, tier: String, handle: String?) =
        adminRpc().usersCreate(actorId, tier, handle)

    /** `fauna.admin.users.update` — set a user's tier (= the quota) + label. */
    suspend fun adminUsersUpdate(actorId: ByteArray, tier: String, label: String) =
        adminRpc().usersUpdate(actorId, tier, label)

    /**
     * `fauna.admin.users.evict` — start the timed warn→suspend→delete ladder
     * (`admin-users-evict-button`; admin.md § 2 Users → *Cutting a user off*).
     * [reason]/[category] are the audit fields — the row passes the shared
     * `evict_default_reason` string + `"other"`, mirroring linux `evict_user`.
     */
    suspend fun adminUsersEvict(actorId: ByteArray, reason: String, category: String) =
        adminRpc().usersEvict(actorId, reason, category)

    /**
     * `fauna.admin.users.suspend` — cut a user off **immediately**, no delete
     * timeline (`admin-users-suspend-button`). Same audit-field contract as
     * [adminUsersEvict]; the nest refuses an admin target (`fauna.admin.conflict`).
     */
    suspend fun adminUsersSuspend(actorId: ByteArray, reason: String, category: String) =
        adminRpc().usersSuspend(actorId, reason, category)

    /**
     * `fauna.admin.users.cancel_eviction` — restore a suspended/evicted user
     * (`admin-users-cancel-eviction-button`; the shared exit from either the
     * evict ladder or an immediate suspend).
     */
    suspend fun adminUsersCancelEviction(actorId: ByteArray) =
        adminRpc().usersCancelEviction(actorId)

    /**
     * `fauna.admin.admins.add` — grant the admin role, the roster surface's
     * `admin-users-make-admin-button` (admin.md § Admin continuity and
     * succession, instrument 1). Schedules a 24h-delayed `AdminAdd` pending
     * action — a scheduled reply (no error) IS success; the row does not flip
     * to an admin row right away.
     */
    suspend fun adminAdminsAdd(actorId: ByteArray) = adminRpc().adminsAdd(actorId)

    /**
     * `fauna.admin.admins.remove` — revoke the admin role, the roster surface's
     * `admin-users-remove-admin-button`. Refuses (`fauna.admin.conflict`) when
     * it would leave zero superadmins — the nest, not the client, decides.
     */
    suspend fun adminAdminsRemove(actorId: ByteArray) = adminRpc().adminsRemove(actorId)

    /**
     * `fauna.admin.set_registration_mode` — set the deployment's registration
     * posture + the orthogonal free-tier ceiling together, one call
     * (`admin-users-registration-save-button`; admin.md § 2 Users → *Section 2
     * — Registration*). [maxFreeUsers] `null` **clears** the cap (blank input
     * = no cap), never `0`. The caller re-reads `nestSetupStatus()` afterward so
     * the section reflects the persisted posture, not the local selection
     * (mirrors [adminSetServingPort]'s reflective write-then-reread).
     */
    suspend fun adminSetRegistrationMode(mode: FfiRegistrationMode, maxFreeUsers: ULong?) =
        adminRpc().setRegistrationMode(mode, maxFreeUsers)

    /**
     * `fauna.admin.set_age_verification_required` — the D5+D6 require-knob
     * ("accept only signups carrying app age verification"; admin.md § 2 Users
     * → Registration). Dispatched by the same Registration save, beside
     * [adminSetRegistrationMode], only when the toggle changed; read back from
     * `nestSetupStatus().ageVerificationRequired`.
     */
    suspend fun adminSetAgeVerificationRequired(required: Boolean) =
        adminRpc().setAgeVerificationRequired(required)

    /** `fauna.admin.tiers.list` — the quota tiers backing the tier pickers. */
    suspend fun adminTiersList(): List<FfiAdminTier> = adminRpc().tiersList()

    /**
     * `fauna.admin.tiers.update` — persist a tier definition's caps (raw i64:
     * bytes for the byte caps, counts otherwise). The `name` identifies the
     * row and is not editable. Backs the `admin-settings` in-place cap editor
     * (admin.md § 3), mirroring Linux `update_admin_tier`.
     */
    suspend fun adminTiersUpdate(
        name: String,
        maxInboxBytes: Long,
        maxStorageBytes: Long,
        maxDevices: Long,
        maxBlobSize: Long,
        maxFeeds: Long,
    ) = adminRpc().tiersUpdate(name, maxInboxBytes, maxStorageBytes, maxDevices, maxBlobSize, maxFeeds)

    /**
     * `fauna.admin.membership_tiers.list` — every membership designation the
     * admin owns (monetization.md § Pillar 4): a link between one of the
     * admin's own subscription tiers and the quota tiers an admitted/lapsed
     * member runs under. An empty list is the out-of-the-box state — nothing
     * about this nest is monetized yet, not an error.
     */
    suspend fun adminMembershipTiersList(): List<FfiAdminMembershipTier> =
        adminRpc().membershipTiersList()

    /**
     * `fauna.admin.membership_tiers.set` — designate/re-point a membership
     * tier (an upsert, safe to repeat). Pass an empty [lapseTier] for the
     * documented default (`fauna_protocol::admin::DEFAULT_LAPSE_TIER`,
     * `"free"`) — the nest applies it when the wire field is omitted. Backs
     * `admin-settings-membership-save-button`.
     */
    suspend fun adminMembershipTiersSet(tierName: String, adminTier: String, lapseTier: String) =
        adminRpc().membershipTiersSet(tierName, adminTier, lapseTier)

    /**
     * `fauna.admin.membership_tiers.clear` — drop a designation; the
     * subscription tier itself is untouched, it just reverts to an ordinary
     * content tier. Backs `admin-settings-membership-clear-button`.
     */
    suspend fun adminMembershipTiersClear(tierName: String) =
        adminRpc().membershipTiersClear(tierName)

    /**
     * `fauna.admin.factory_reset` — wipe deployment state and return the nest to
     * fresh/unclaimed (mail-bridge-lifecycle.md § Factory reset). The reply
     * carries the post-reset claim code (the human never sees it: the nest exits
     * + restarts into the wipe right after replying), which the caller pre-fills
     * into the re-onboarding claim-code step. Mirrors Linux `client.factory_reset`.
     *
     * [newClaimCode] **pins** the code the wiped box will boot with (the nest honors
     * a pinned code verbatim). Callers MUST pin a code they have already persisted
     * via `mintAndPersistPendingFactoryReset` — that is what closes gap CR-1
     * (`nest/common.md` § Client-state recoverability): a client SIGKILL'd between
     * dispatch and reply-render would otherwise lose the only copy of the code, and
     * the box would land fresh/unclaimed but un-claimable. Passing `null` (the nest
     * mints a random code) reopens the gap and is not a supported client path.
     */
    suspend fun factoryReset(newClaimCode: String?): String =
        adminRpc().factoryReset(newClaimCode)

    /**
     * `fauna.setup.status` — the full deployment setup snapshot. The non-admin
     * first-setup mail glue ([com.fauna.app.ui.viewmodel.MailEnableGlueVM]) reads
     * `emailEnabled` + `autoEnableMailForNewUsers` (unset ⇒ true) to gate its
     * auto-mint per the deployment policy (mail-credentials.md § Auto-enable for new
     * users). Anonymous read over the live WS-RPC connection.
     */
    suspend fun nestSetupStatus(): FfiSetupStatus = nestRpc().setupStatus()

    /** `fauna.admin.invite_codes.list` — every closed-registration invite code. */
    suspend fun adminInviteCodesList(): List<FfiAdminInviteCode> = adminRpc().inviteCodesList()

    /**
     * `fauna.admin.invite_codes.create` — mint (or register) a code at `tier` +
     * `uses`. Pass an empty `code` to have the nest mint a random token; the
     * returned String is the resulting code for the UI to surface/copy.
     *
     * `guardianActor` binds the minted code to a supervising guardian (family safety).
     * Defaults to `null` = an ordinary unsupervised code, which is every android caller
     * today — android has no guardian picker yet (windows leads it); wiring one is the
     * android leg of the family-safety track.
     */
    suspend fun adminInviteCodesCreate(
        code: String,
        tier: String,
        uses: Long,
        guardianActor: ByteArray? = null,
        ageBand: String? = null,
    ): String = adminRpc().inviteCodesCreate(code, tier, uses, guardianActor, ageBand)

    /** `fauna.admin.invite_codes.delete` — remove a code. */
    suspend fun adminInviteCodesDelete(code: String) = adminRpc().inviteCodesDelete(code)

    /** `fauna.admin.invite_requests.list` — pending + decided invite requests. */
    suspend fun adminInviteRequestsList(): List<FfiAdminInviteRequest> =
        adminRpc().inviteRequestsList()

    /**
     * `fauna.admin.invite_requests.approve` — admit the requester, creating the
     * account at `tier` (null ⇒ the nest default). Returns the resolved
     * `{ actor_id, handle, tier }`.
     */
    suspend fun adminInviteRequestsApprove(
        id: Long,
        tier: String? = null,
        label: String? = null,
        guardianActor: ByteArray? = null,
        ageBand: String? = null,
    ): FfiAdminInviteRequestApproveReply =
        adminRpc().inviteRequestsApprove(id, tier, label, guardianActor, ageBand)

    /** `fauna.admin.invite_requests.deny` — deny a request with an optional reason. */
    suspend fun adminInviteRequestsDeny(id: Long, reason: String? = null) =
        adminRpc().inviteRequestsDeny(id, reason)

    /**
     * `fauna.admin.services.list` — the `bridge`/`pairing` service
     * flags backing the `admin-service-{bridge,pairing}` toggles
     * (admin.md § 5 Services). The DNS toggle is NOT a flag — it rides the
     * shared `DnsManagementMachine` (see [dnsSetAllManaged]).
     */
    suspend fun adminServicesList(): FfiAdminServiceFlags = adminRpc().servicesList()

    /** `fauna.admin.services.update` — flip one service flag (`bridge` /
     *  `pairing`). The UI re-reads `servicesList` to reflect it. */
    suspend fun adminServicesUpdate(name: String, enabled: Boolean) =
        adminRpc().servicesUpdate(name, enabled)

    /** `fauna.admin.set_serving_port` — set the deployment-wide client-facing API
     *  serving port (the `admin-nest-serving-port-input`; nest/common.md § Serving
     *  ports). `port` is a u16 in [1, 65535] (the VM validates the range first).
     *  The caller re-reads `nestSetupStatus().servingPort` to reflect the applied
     *  value; the new port binds on the next nest restart. The symmetric twin of
     *  the CalDAV port — a `fauna.admin.*` call, no shared policy machine. */
    suspend fun adminSetServingPort(port: UShort) = adminRpc().setServingPort(port)

    /** `fauna.admin.request_host_restart` — expedite the host Ubuntu box's
     *  idle-gated reboot (installers/vps.md § Host OS Maintenance § 4). Backs the
     *  admin-nest `nest-os-restart-now-button`; writes a flag the host reboot-
     *  coordinator consumes. Rejected (`no_host`) on a nest with no maintenance
     *  mount (dev / desktop / bare-metal) — surfaced on the page error element. */
    suspend fun adminRequestHostRestart() = adminRpc().requestHostRestart()

    /** `fauna.admin.region.get` folded through the shared `admin_region_view`
     *  (region-blocking.md § Region determination) — the one call
     *  `admin-nest-region-section` needs to paint all five fields. Free
     *  function, not an `FfiAdminClient` method (the `LocalizedText`-touching
     *  export stays cleanly out of the Go mail-bridge build — same reason
     *  `seedRotateRoster` above is one). */
    suspend fun adminRegionStatus(): FfiAdminRegionView = com.fauna.ffi.adminRegionStatus(nestRpc())

    /** `fauna.admin.region.set` — `admin-nest-region-save-button` (`region`
     *  already validated by [com.fauna.ffi.adminParseRegionCode]) and
     *  `admin-nest-region-withdraw-button` (`region = null`) both call
     *  through here; re-validates regardless of whether the caller already
     *  did — the only path that can reach the wire. Re-declaring also
     *  retires the previous region's feature-policy document nest-side. */
    suspend fun adminSetRegion(region: String?) = com.fauna.ffi.adminSetRegion(nestRpc(), region)

    /** `fauna.oauth.issuer_key_status` folded through the shared
     *  `issuer_key_view` (authorization-server.md § The issuer) — the one read
     *  `admin-nest-oauth-section` paints its key rows from. Throws on failure:
     *  the caller words it on the section's own reason line, never on the
     *  page (any read failure, transport or nest, leaves the rest of
     *  admin-nest painting). Free function for the same reason
     *  [adminRegionStatus] is one. */
    suspend fun adminIssuerKeyStatus(): FfiIssuerKeyView =
        com.fauna.ffi.adminIssuerKeyStatus(nestRpc())

    /** `fauna.oauth.rotate_issuer_key` — `admin-nest-oauth-rotate-button`,
     *  dispatched AND worded by the shared fold: the returned sentence is the
     *  section's verdict, success or failure (the FFI face never throws; only
     *  a missing connection here can). */
    suspend fun adminRotateIssuerKey(): uniffi.fauna_core.LocalizedText =
        com.fauna.ffi.adminRotateIssuerKey(nestRpc())

    /** `admin-nest-oauth-confirm-button` — exactly [arm]'s kind
     *  (`fauna.oauth.force_rotate_issuer_key` /
     *  `fauna.oauth.force_rotate_session_secret`), dispatched and worded like
     *  [adminRotateIssuerKey]. The caller disarms before calling. */
    suspend fun adminForceRotateIssuer(arm: FfiIssuerForcedArm): uniffi.fauna_core.LocalizedText =
        com.fauna.ffi.adminForceRotateIssuer(nestRpc(), arm)

    /** `fauna.admin.logs` — the nest's in-memory `fauna-log` ring snapshot
     *  (observability.md § Surfaces), rendered by the admin Logs page with the
     *  same widget as the client's own Settings → Logs page. No clear (there is
     *  no admin RPC to wipe the nest ring). */
    suspend fun adminLogs(): List<uniffi.fauna_log.LogEntry> = adminRpc().logs()

    /**
     * `fauna.admin.custody_hosting.list` — every hosting row on this nest
     * (account-data-plane.md § Two-sided bounds), already
     * folded by the shared `admin_hosting_rows` projection (heaviest hold first,
     * tie-broken on `(host, owner, grant)`) so every lift app renders the same
     * rows in the same order — do NOT re-sort or re-derive `receiptState`.
     */
    suspend fun adminHostingList(): List<FfiAdminHostingRow> = adminRpc().custodyHostingList()

    /**
     * `fauna.admin.custody_hosting.remove` — drop one row, keyed by the
     * `(host, grant)` pair an [adminHostingList] row carries (never a painted
     * index — a re-read can reorder rows). `removed = false` is an honest no-op
     * (the row was already gone), not a failure.
     */
    suspend fun adminHostingRemove(hostActorId: String, grantId: ByteArray): FfiAdminHostingRemoveReply =
        adminRpc().custodyHostingRemove(hostActorId, grantId)

    // -- Internal HTTP helpers --

    private fun authorizedRequest(url: String): Request.Builder {
        val builder = Request.Builder().url(url)
        token?.let { builder.addHeader("Authorization", "Bearer $it") }
        return builder
    }

    private suspend fun get(path: String): String {
        ensureAuthenticated()
        val request = authorizedRequest("$nodeUrl/$path").build()
        return execute(request)
    }

    private suspend inline fun <reified T> getDecoded(path: String): T {
        val body = get(path)
        return json.decodeFromString(body)
    }

    private suspend fun postMultipartBlob(
        path: String,
        sidecarCbor: ByteArray,
        sealedBytes: ByteArray,
    ): String {
        ensureAuthenticated()
        val multipart = MultipartBody.Builder()
            .setType(MultipartBody.FORM)
            .addFormDataPart("sidecar", null, sidecarCbor.toRequestBody(cborMediaType))
            .addFormDataPart("bytes", null, sealedBytes.toRequestBody(octetMediaType))
            .build()
        val request = authorizedRequest("$nodeUrl/$path")
            .post(multipart)
            .build()
        return execute(request)
    }

    private suspend fun putJson(path: String, body: RequestBody): String {
        ensureAuthenticated()
        val request = authorizedRequest("$nodeUrl/$path")
            .put(body)
            .addHeader("Content-Type", "application/json")
            .build()
        return execute(request)
    }

    private suspend fun deleteRequest(path: String): String {
        ensureAuthenticated()
        val request = authorizedRequest("$nodeUrl/$path")
            .delete()
            .build()
        return execute(request)
    }

    private suspend fun execute(request: Request): String =
        suspendCancellableCoroutine { cont ->
            val call = httpClient.newCall(request)
            cont.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onResponse(call: Call, response: Response) {
                    val body = response.body?.string() ?: ""
                    if (response.isSuccessful) {
                        cont.resume(body)
                    } else {
                        cont.resumeWithException(
                            ApiException("HTTP ${response.code}: $body")
                        )
                    }
                }
                override fun onFailure(call: Call, e: IOException) {
                    cont.resumeWithException(e)
                }
            })
        }

    private suspend fun executeBytes(request: Request): ByteArray =
        suspendCancellableCoroutine { cont ->
            val call = httpClient.newCall(request)
            cont.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onResponse(call: Call, response: Response) {
                    if (response.isSuccessful) {
                        cont.resume(response.body?.bytes() ?: ByteArray(0))
                    } else {
                        cont.resumeWithException(
                            ApiException("HTTP ${response.code}")
                        )
                    }
                }
                override fun onFailure(call: Call, e: IOException) {
                    cont.resumeWithException(e)
                }
            })
        }

}

class ApiException(message: String) : Exception(message)

// ── Bluesky thread helpers ──────────────────────────────────────────────────

/**
 * Project a wire [FfiBlueskyPost] (a flat display projection of the proto
 * `BlueskyPost`) onto the unified [PostDetail] the feed/post-detail UI renders.
 * `postId` carries the bridge-side opaque id by default; [blueskyGetThread]
 * overrides it with the navigated Fauna post id for the focal post.
 */
private fun FfiBlueskyPost.toPostDetail(): PostDetail = PostDetail(
    postId = id,
    author = authorDisplayName?.takeIf { it.isNotBlank() } ?: authorHandle,
    createdAt = parseIsoToEpochMillis(createdAt),
    body = text,
    tags = emptyList(),
    hasMedia = hasMedia,
    isReply = replyParent != null,
    source = source.ifEmpty { "bluesky" },
    likeCount = likeCount.toInt(),
    repostCount = repostCount.toInt(),
    replyCount = replyCount.toInt(),
    isLiked = viewerLiked,
    isReposted = viewerReposted,
    replyToId = replyParent,
    uri = atUri,
    cid = cid,
    rootUri = replyRoot,
    rootCid = null,
)

/**
 * Project a native Fauna [DecodedPost] (from `fauna.posts.get` → [decodePostFull])
 * onto the unified [PostDetail] the feed/post-detail UI renders. [navPostId] is the
 * id the UI navigated with — echoed so like/repost/reply ride `fauna.posts.interact`
 * on the right post. `created_at` is microseconds (`fauna_core::Timestamp`) → the
 * UI's millisecond absolute-time formatter. Content-only: `fauna.posts.get` resolves
 * the post body, not interaction counts, so counts/viewer-state stay at their zero
 * defaults (the detail screen omits zero counts). `author` is the hex actor id — the
 * same projection the feed card renders ([FfiFeedPostItem.author]).
 */
internal fun DecodedPost.toPostDetail(navPostId: String, source: String): PostDetail = PostDetail(
    postId = navPostId,
    author = author,
    createdAt = createdAt / 1000, // fauna_core::Timestamp micros → epoch millis
    body = body,
    tags = tags,
    hasMedia = items.isNotEmpty(),
    isReply = references.any { it.refType == "reply" },
    source = source.ifEmpty { "fauna" },
    replyToId = references.firstOrNull { it.refType == "reply" }?.postId,
)

/** Parse an ISO-8601 timestamp (with `Z` or an offset) to epoch millis; 0 on failure. */
private fun parseIsoToEpochMillis(iso: String): Long = runCatching {
    java.time.OffsetDateTime.parse(iso).toInstant().toEpochMilli()
}.recoverCatching {
    java.time.Instant.parse(iso).toEpochMilli()
}.getOrDefault(0L)
