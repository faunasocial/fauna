package com.fauna.app.core.conversations

import com.fauna.app.core.NotificationHelper
import com.fauna.app.core.ShellLog
import com.fauna.app.testing.TestAgent
import com.fauna.ffi.FfiContactOverlays
import com.fauna.ffi.FfiDraftsSync
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.autosaveDebounceMs
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_conversations.ConversationsManager
import uniffi.fauna_conversations.ConversationsSession
import uniffi.fauna_conversations.ConversationsSnapshot
import uniffi.fauna_conversations.SnapshotObserver
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Process-wide owner of the shared [ConversationsManager] (UniFFI). Mirrors
 * [com.fauna.app.core.OnboardingHost] — the android "shared-machine host"
 * pattern — and the Windows `ConversationsManagerHost` precedent: one manager
 * instance lives for the app's lifetime, all conversations state lives in
 * shared Rust, and Compose screens observe snapshot diffs back.
 *
 * Per docs/goal/ui/conversations.md §"Architectural rules" #1/#2: rendering is
 * observer-driven against the manager snapshot — no client-side state machine,
 * no client-side MLS.
 *
 * In E2E builds ([TestAgent.isE2EActive] — the `FAUNA_E2E_BRIDGE` intent-extra
 * signal android already uses) the host registers a mock backend for every
 * rail so the bridge's inject/send paths can route inbound without the test
 * constructing rail backends across the FFI. The *real* rails arrive with the
 * login-time [startConversationsSession], whose shared `conversations_session`
 * factory registers them in Rust (`register_backend` itself stays Rust-only
 * glue, not UniFFI-exported) and whose manager then supersedes the bare one —
 * in production always, under E2E only behind the `real_conversations` gate.
 */
@Singleton
class ConversationsManagerHost @Inject constructor(
    notifications: NotificationHelper,
) {
    private val _snapshot = MutableStateFlow<ConversationsSnapshot?>(null)

    /**
     * Compose screens `collectAsState()` this; every manager notification
     * pushes a fresh snapshot. Null only in the instant before construction
     * seeds the first snapshot (see [init]).
     */
    val snapshot: StateFlow<ConversationsSnapshot?> = _snapshot.asStateFlow()

    // The bare, backend-less manager built at construction — the observable
    // surface before login, and (under E2E) the one the mock backends install on.
    // Post-login it is superseded by the session's manager (see
    // [startConversationsSession]); the public [manager] getter returns whichever
    // is active, so the VM's `host.manager` access is unchanged.
    private val bareManager: ConversationsManager = ConversationsManager()

    // The live conversations session's manager — non-null only while a session is
    // active (login → logout). `@Volatile` because it is read from the observer's
    // arbitrary-thread `onChanged()` and the VM's compose thread.
    @Volatile
    private var sessionManager: ConversationsManager? = null

    /**
     * The shared manager — exposed for direct mutator calls from the VM. Returns
     * the live session's manager once logged in (via [startConversationsSession]),
     * else the bare pre-login manager. The instance swap is invisible to the VM,
     * which reads through this getter.
     */
    val manager: ConversationsManager get() = sessionManager ?: bareManager

    // ── The private contact overlay (contacts.md § The private overlay) ────
    //
    // Every name a surface keyed on a person shows — roster row, knock sender,
    // feed and subscription author, Profile header — comes from the shared
    // overlay projection through `FfiContactOverlays`; nothing here resolves a
    // name. The face is built over whichever manager is live NOW and rebuilt
    // when [manager] is a different instance: the login swap replaces the
    // manager, and a face held across it would read the old projection.
    private val overlayLock = Any()
    private var overlayFace: Pair<ConversationsManager, FfiContactOverlays>? = null
    private var overlayRevision: ULong? = null
    private val _overlayEpoch = MutableStateFlow(0L)

    /**
     * Moves whenever the names the overlay projection answers may have moved —
     * its content changed, or the manager it is read from was swapped. Screens
     * key their [contactOverlays] reads on it.
     */
    val overlayEpoch: StateFlow<Long> = _overlayEpoch.asStateFlow()

    /** The overlay projection of the live manager. Call it per read; never hold the result across a login or logout. */
    fun contactOverlays(): FfiContactOverlays = synchronized(overlayLock) {
        val live = manager
        val held = overlayFace
        if (held != null && held.first === live) {
            held.second
        } else {
            overlayRevision = null
            FfiContactOverlays(live).also { overlayFace = live to it }
        }
    }

    // Run on every manager notification and after each manager swap: advance
    // [overlayEpoch] when the projection's revision moved or the face was
    // rebuilt (a rebuilt face has no remembered revision).
    private fun refreshOverlays() {
        val face = contactOverlays()
        val revision = face.revision()
        synchronized(overlayLock) {
            if (overlayRevision != revision) {
                overlayRevision = revision
                _overlayEpoch.value += 1
            }
        }
    }

    // ── Draft persistence v2 (file-sync.md § Drafts Sync) ──────────────────
    //
    // The android twin of linux `conversations/drafts.rs`: pure trigger glue
    // over the shared `FfiDraftsSync` (which owns the seal, the WS-RPC
    // `fauna.drafts.{get,put}` calls, the launch gate, and the last-saved
    // baseline). [ApiClient] hands us the per-session handle when the nest
    // connects ([startDraftsSync]); we restore the owner's persisted drafts on
    // launch and let [observer] drive a debounced autosave after compose edits.
    // Held only while logged in; [stopDraftsSync] retires it on logout. No
    // interim draft store to retire — android consumes the shared manager's
    // `DraftStore` directly, like linux.
    private val draftsScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var draftsSync: FfiDraftsSync? = null
    private var draftsSaveJob: Job? = null

    // ── Conversations receive session ──────────────────────────────────────
    //
    // The android leg of the unified inbound path (mail-spam.md § Implementation
    // status item 6; conversations.md § Receiving into the conversations view).
    // On login [com.fauna.app.core.ApiClient] hands us the connected
    // [FfiNestClient]; we build a shared `ConversationsSession` over the
    // `conversations_session` UniFFI factory, adopt its manager as the
    // observable, and drive `start_receive_loop` so inbound mail (INBOX/Sent) —
    // and the on-device INBOX spam scorer wired into the shared
    // `NestMailInboundSource` — flow into the unified view with no per-app
    // glue (priority #2). Retired on logout by [stopConversationsSession]. The
    // android twin of apple `ConversationsVM.activate(session:)` + windows
    // `ConversationsManagerHost` real-manager registration.
    // `@Volatile` because the `session` getter reads it lock-free from the VM's
    // sharing coroutine while start/stop write it under `synchronized(this)`.
    @Volatile
    private var conversationsSession: ConversationsSession? = null
    private var receiveLoopJob: Job? = null

    // The client this session was built over, kept for the session's lifetime so
    // [successionWitnessStateJson] can reach the member-side succession report it
    // holds. Held here rather than passed to the reader because the *session* is
    // what makes the report meaningful, and this singleton already owns that
    // lifecycle — retired together in [stopConversationsSession].
    // `@Volatile` for the same reason `conversationsSession` is: the reader is
    // lock-free, the writers are under `synchronized(this)`.
    @Volatile
    private var nestClient: FfiNestClient? = null

    /**
     * The live conversations session — non-null only while a session is active
     * (login → logout). Exposed so the folder Sharing FFI (`folders_share` /
     * `folders_remove_member`) can reuse the SAME per-actor `MlsEngine` this
     * session holds over the one `mls_state.db` — never a second engine racing the
     * SQLite file (folders.md § Sharing → Where logic lives).
     */
    val session: ConversationsSession? get() = conversationsSession

    /**
     * Fired after every manager notification, alongside the snapshot update
     * and the draft autosave arm — lets [com.fauna.app.core.feed.FeedManagerHost]
     * re-read `own_rooms` on this plane's own change tick
     * (`ui/feed.md` § Encryption at rest → *Room-restricted — the app half*:
     * "the list is a projection of the conversations plane... notified only
     * when it changed") without this class depending on that one, which would
     * cycle back through [com.fauna.app.core.ApiClient]. A plain settable
     * callback rather than an observer list: exactly one listener exists.
     */
    var onTick: (() -> Unit)? = null

    /**
     * The running-app new-message banner (`conversations` outcome 11): the
     * shared tracker's decision, fired through [NotificationHelper]. Ticked from
     * [observer] on every manager notification, so it sees the bare manager and
     * each session's manager alike; reset at every identity change
     * ([stopConversationsSession]). `label` is the peer / group name and
     * `snippet` the message preview — the two fields every app's firing site
     * hands its platform.
     */
    private val banners = MessageBannerObserver { activity ->
        notifications.postMessageNotification(activity.label, activity.snippet, activity.threadId)
    }

    /**
     * The member side of a succession, as the e2e state protocol's
     * `data.succession_witness` key — the JSON string shared Rust renders
     * (`fauna_client_recovery::witness::state_json`, one owner for all 7 apps),
     * passed through rather than re-derived here.
     *
     * `null` before a conversations session exists, which is a *different*
     * reading from an empty report and must stay distinguishable: only the
     * second one indicts the inbound poll
     * (`succession-aftermath.md` § Propagation → *MLS groups*).
     *
     * A field read, no round trip — its caller is the test agent's ack path
     * (`e2e-conventions.md` point 11's second corollary).
     */
    fun successionWitnessStateJson(): String? = nestClient?.successionWitnessStateJson()

    // Held for the manager's (== this singleton's) lifetime so the Rust side
    // keeps a live callback. `onChanged()` fires from arbitrary Rust threads;
    // MutableStateFlow.value is thread-safe and `collectAsState()` observes on
    // the composition context, so no explicit main-thread marshaling is needed
    // (unlike the Windows INotifyPropertyChanged path). The same notification
    // re-arms the debounced draft autosave (a cheap no-op when no draftsSync is
    // attached or the snapshot is unchanged) and runs one banner tick.
    private val observer = object : SnapshotObserver {
        override fun `onChanged`() {
            _snapshot.value = manager.snapshot()
            scheduleDraftsSave()
            banners.tick(manager)
            refreshOverlays()
            onTick?.invoke()
        }
    }

    init {
        // Delegated rather than called inline: `installMockBackendsForTest()` is
        // a `test-helpers` UniFFI export, absent from the production-flavored
        // bindings the release APK is built against (testing.md § convention 15),
        // and this file compiles into that APK. The debug twin does the real
        // install under [TestAgent.isE2EActive]; the release twin is a no-op.
        TestAgent.installMockBackendsIfE2E(bareManager)
        bareManager.addObserver(observer)
        _snapshot.value = bareManager.snapshot()
    }

    /**
     * Build the shared conversations receive session for the just-connected nest
     * and start its receive loop — the android leg of the unified inbound path
     * (mail-spam.md § Implementation status item 6). Called once per session by
     * [com.fauna.app.core.ApiClient]'s connect path, BEFORE [startDraftsSync] so
     * the drafts restore targets the session's `DraftStore`. Mirrors apple
     * `ConversationsVM.activate(session:)` + windows `ConversationsManagerHost`.
     *
     * The `conversations_session` factory wires both rails (FaunaMls DMs + the
     * SMTP send sink), the inbound push source, the INBOX/Sent mail read-feeds
     * (`NestMailInboundSource`, which holds the shared on-device spam scorer), and
     * the scheduling / inbox-drain / folder-gate sinks over this one connection,
     * returning a session whose `manager()` becomes our observable.
     * `start_receive_loop` is a detached Rust task; we launch it fire-and-forget
     * on [draftsScope] (the apple `Task { await startReceiveLoop() }` twin) and
     * retire it in [stopConversationsSession].
     *
     * Skipped under plain E2E — the mock backends installed on [bareManager] at
     * construction stand in for the real rails, so deterministic
     * `conversations_inject_inbound` DM tests aren't disturbed. Under the
     * `real_conversations` launch-time gate ([TestAgent.isRealConversationsActive],
     * the `FAUNA_E2E_REAL_CONVERSATIONS` intent extra — android's twin of the
     * windows/macOS/iOS launch gate), the real session registers unconditionally
     * here, same as production. A build failure is logged and left non-fatal:
     * mail receive must never degrade the rest of the app.
     */
    fun startConversationsSession(
        nestClient: FfiNestClient,
        selfAddress: String,
        secretBytes: ByteArray,
        mlsDbPath: String,
        predecessorBackupKeys: List<ByteArray>,
        recordingDevice: String?,
    ) {
        if (TestAgent.isE2EActive && !TestAgent.isRealConversationsActive) return
        synchronized(this) {
            if (sessionManager != null) return
            val session = try {
                // `indexLeaseDevice = null`: a phone never builds the content
                // index (it queries the synced copy — the ratified build-vs-query
                // split, `content-index.md` § Where the index is built), so there
                // is nothing here to coordinate and no `index` lease to seat.
                // `null` is the *correct* answer on this target, not a stub
                // (`participants.md` § Coordination primitive → *The `index` kind
                // under the lease*); shared Rust gates the seat on the same
                // constant, so a stray id here could not seat one anyway.
                // `predecessorBackupKeys` feeds the `__mls` post-succession
                // re-seal (`mls_sync_launcher`'s `predecessors` param) — the
                // caller resolves it once off `AccountStores.predecessorBackupKeys`
                // (`sync-agent.md` § Credential model → *Retired owner keys
                // after an identity succession*), keyed on THIS session's own
                // actor — never `AccountStores.activeActorHex()`, which can
                // disagree during an append-mode sign-in or a switch race; empty for every identity that never
                // succeeded, which costs nothing.
                nestClient.conversationsSession(
                    selfAddress,
                    secretBytes,
                    mlsDbPath,
                    null,
                    predecessorBackupKeys,
                    recordingDevice,
                )
            } catch (e: Exception) {
                ShellLog.w("ConversationsSession", "build failed: ${e.message}")
                return
            }
            conversationsSession = session
            this.nestClient = nestClient
            val m = session.manager()
            sessionManager = m
            m.addObserver(observer)
            _snapshot.value = m.snapshot()
            receiveLoopJob = draftsScope.launch { session.startReceiveLoop() }
        }
        refreshOverlays()
    }

    /**
     * Retire the receive session on logout. Hands the conversations-engine role
     * over FIRST, then tears down: [startConversationsSession] always builds
     * over the FRESH-MANAGER `conversationsSession(...)` factory arm, never
     * `conversations_session_over_manager`, so the shared factory's own
     * hand-over (`account-runtime.md` § Multi-instance concurrency) never runs
     * for android — it is handed no manager to retire in the first place
     * (`FfiNestClient::build_conversations_session`'s `if let Some(m) = &manager`
     * guard, `nest_client.rs:1967-1968`). So a same-process re-login or account
     * switch would otherwise ask a successor `MlsEngine::new` for the role lock
     * over `mls_state.db` while this predecessor still held it, refused
     * `ServedElsewhere` — masked today only by [startConversationsSession]'s
     * `if (sessionManager != null) return` guard, which refuses every
     * same-process rebuild outright rather than exposing it.
     * `retireConversationsEngine()` on the OUTGOING [sessionManager], before
     * nulling it, is the android twin of windows'
     * `ConversationsManagerHost.ResetForActorChange` call; best-effort like
     * windows', since teardown must never block a logout.
     * Then cancels the loop-launch coroutine and closes the
     * [ConversationsSession] UniFFI object, then reverts the observable to
     * [bareManager]. The Rust receive loop — kept alive by the extra session
     * `Arc` the factory stashes in the nest client — then exits on the
     * session-closed **signal** (`ConversationsSession::closed()`, shared Rust)
     * once [com.fauna.app.core.ApiClient] also closes the nest client. It used to
     * wait for its next liveness tick instead; that is the tick-bound shape the
     * 2026-08-27 fix retired fleet-wide (`account-scoping.md`, the
     * `tui (in-memory)` ledger row), and android inherits the fix through shared
     * Rust with no glue of its own.
     */
    fun stopConversationsSession() {
        synchronized(this) {
            try {
                sessionManager?.retireConversationsEngine()
            } catch (e: Exception) {
                ShellLog.w("ConversationsSession", "retire failed (best-effort): ${e.message}")
            }
            receiveLoopJob?.cancel()
            receiveLoopJob = null
            conversationsSession?.close()
            conversationsSession = null
            nestClient = null
            sessionManager = null
            // The next identity's restored threads are not new messages: a fresh
            // tracker seeds on them silently (MessageBannerObserver's reset doc).
            banners.resetForIdentityChange()
            _snapshot.value = bareManager.snapshot()
        }
        refreshOverlays()
    }

    /**
     * Wire draft persistence for the just-connected session: restore the owner's
     * persisted conversation drafts on launch (so the composer reflects drafts
     * left on this or another of the user's devices), then let [observer] drive
     * the debounced autosave. Called once per session by
     * [com.fauna.app.core.ApiClient]'s connect path; [stopDraftsSync] retires it
     * on logout. Mirrors linux `drafts::start`. A restore failure is logged and
     * left non-fatal — the `FfiDraftsSync` launch gate then stays closed, so a
     * later autosave can't clobber the unread blob (no-data-loss).
     *
     * The target manager and its identity epoch are read HERE, before the
     * load starts — never after it returns. [manager] is a getter over the
     * CURRENT session, so a reply the outgoing account's fetch delivers after
     * a switch would otherwise fill the incoming account's manager; the
     * epoch-checked restore (`restoreDraftsAt`) also refuses it on a manager
     * cleared in between (`account-scoping.md` § The scoping taxonomy — the
     * writers of account-scoped state retire with the drop).
     */
    fun startDraftsSync(sync: FfiDraftsSync) {
        synchronized(this) { draftsSync = sync }
        val target = manager
        val epoch = target.identityEpoch()
        draftsScope.launch {
            try {
                sync.load()?.let { target.restoreDraftsAt(epoch, it) }
            } catch (e: Exception) {
                ShellLog.w("ConversationsDrafts", "restore drafts failed: ${e.message}")
            }
        }
    }

    /**
     * Retire the autosave on logout. The [FfiDraftsSync] handle itself is closed
     * by [com.fauna.app.core.ApiClient] (which owns the connection lifecycle).
     */
    fun stopDraftsSync() {
        synchronized(this) {
            draftsSaveJob?.cancel()
            draftsSaveJob = null
            draftsSync = null
        }
    }

    /**
     * Debounced persist after a compose change: each manager notification
     * re-arms an [autosaveDebounceMs] timer and only the last one fires, so a
     * burst of keystrokes coalesces into one upload. `saveIfChanged` then no-ops
     * before the launch restore completes (the no-data-loss gate) and for an
     * unchanged snapshot, so a tick fired by a non-draft manager change costs
     * only a cheap snapshot compare. `synchronized` because `onChanged()` fires
     * from arbitrary Rust threads — it serializes the job swap against
     * [startDraftsSync] / [stopDraftsSync].
     */
    private fun scheduleDraftsSave() {
        val sync = synchronized(this) {
            val s = draftsSync ?: return
            draftsSaveJob?.cancel()
            s
        }
        val job = draftsScope.launch {
            delay(autosaveDebounceMs().toLong())
            try {
                sync.saveIfChanged(manager.draftsSnapshotBytes())
            } catch (e: Exception) {
                ShellLog.d("ConversationsDrafts", "save drafts failed (transient): ${e.message}")
            }
        }
        synchronized(this) {
            // A `stopDraftsSync` may have raced in between; don't resurrect it.
            if (draftsSync != null) draftsSaveJob = job else job.cancel()
        }
    }
}
