package com.fauna.app.testing

import android.content.Context
import androidx.compose.ui.text.AnnotatedString
import com.fauna.app.core.AppMessages
import com.fauna.app.core.AppState
import com.fauna.app.core.ExifStripper
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.app.data.db.Contact
import com.fauna.app.data.db.FaunaDatabase
import com.fauna.app.data.db.Knock
import com.fauna.app.data.db.SyncFile
import com.fauna.app.ui.components.ComposeFieldStyling
import com.fauna.app.ui.components.appliedRuns
import com.fauna.app.ui.navigation.navigateToDrawerRoute
import com.fauna.ffi.connectionIsOnline
import com.fauna.ffi.connectionStateWord
import com.fauna.ffi.contentTypeForFilename
import com.fauna.ffi.dialBudgetClearForTest
import com.fauna.ffi.launchClockNowSecsForTest
import com.fauna.ffi.launchClockOffsetSecsForTest
import com.fauna.ffi.setDelegationClockOffsetSecs
import com.fauna.ffi.setTrustClockOffsetSecs
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import kotlinx.coroutines.*
import okhttp3.*
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONArray
import org.json.JSONObject
import java.io.IOException
import uniffi.fauna_conversations.ConversationsManager
import uniffi.fauna_conversations.TypedAddress
import uniffi.fauna_conversations.bannerPassCompleted as ffiBannerPassCompleted
import uniffi.fauna_conversations.bannerPassStarted as ffiBannerPassStarted
import uniffi.fauna_conversations.messageBannersJsonText
import uniffi.fauna_conversations.recordFiredBanner as ffiRecordFiredBanner
import uniffi.fauna_feed.AttachedFile

@EntryPoint
@InstallIn(SingletonComponent::class)
interface TestAgentEntryPoint {
    fun onboardingHost(): OnboardingHost
    fun conversationsManagerHost(): ConversationsManagerHost
    fun feedManagerHost(): FeedManagerHost
    fun apiClient(): com.fauna.app.core.ApiClient
    fun screenTimeStore(): com.fauna.app.core.ScreenTimeStore
    fun atprotoSettingsHost(): com.fauna.app.core.atproto.AtprotoSettingsHost
    fun secureStorage(): SecureStorage

    /**
     * The multi-account registry — the only identity store
     * (`long-term-store.md` § Downgrade mirror + abandoned-append recovery). The
     * session door writes it and the `reset` / `logout` arms clear it, exactly
     * as a real sign-in and sign-out do.
     */
    fun accountRegistry(): com.fauna.ffi.FfiAccountRegistry

    /**
     * The ONE canonical actor-scoped drop (`account-scoping.md` § the in-memory
     * corollary). The agent's `reset` / `logout` arms are teardown sites like
     * any other and go through it, so they cannot drift from what production's
     * switch and sign-out do — the drift that was android's finding.
     */
    fun actorScope(): com.fauna.app.core.ActorScope

    /**
     * Where the agent registers its own actor-scoped state (the [AppState]
     * session override) for the canonical drop to clear. See
     * [TestAgent.registerSessionOverrideDrop].
     */
    fun accountStores(): com.fauna.app.core.AccountStores
}

/**
 * In-app test agent for the unified E2E state protocol.
 * Polls the bridge for commands and pushes app state back.
 * Activated when FAUNA_E2E_BRIDGE env var or intent extra is set.
 */
object TestAgent {
    private var job: Job? = null

    /**
     * True once [start] has run — i.e. the app launched in E2E mode (the
     * `FAUNA_E2E_BRIDGE` intent extra was present). Read by
     * [com.fauna.app.core.conversations.ConversationsManagerHost] to gate
     * `installMockBackendsForTest()`; the android twin of the Windows
     * `FAUNA_E2E_BRIDGE` env-var check. Process-global because the manager
     * host is a Hilt @Singleton constructed lazily, after [start] runs.
     */
    @Volatile
    var isE2EActive: Boolean = false
        private set

    /**
     * True when the `FAUNA_E2E_REAL_CONVERSATIONS` intent extra was present at
     * launch (forwarded from `conftest.py`'s `_apply_real_conversations_env`,
     * the same launch-time gate windows/macOS/iOS already read). Read by
     * [com.fauna.app.core.conversations.ConversationsManagerHost] to let the
     * REAL `ConversationsSession` register even under E2E, instead of the
     * mock backends [isE2EActive] normally installs — the android twin of
     * windows' unconditional-register-at-launch shape (no enable/disable
     * command; readiness rides on the consuming test's own nest-side poll).
     * Set directly from [MainActivity], not [start] — mirrors
     * [credentialFilePath]: both are read once at launch, before any bridge
     * command can arrive.
     */
    @Volatile
    var isRealConversationsActive: Boolean = false

    /**
     * On-device path to the e2e credential seed file, when the
     * `FAUNA_E2E_CREDENTIAL_FILE` intent extra was present. Read by
     * [com.fauna.app.di.LaunchModule.provideSecretBackend] to select
     * [com.fauna.app.core.FileSecretBackend] over the real encrypted store —
     * the android twin of windows' `FAUNA_E2E_CREDENTIAL_DIR` env-var check.
     *
     * Unlike [isE2EActive], [MainActivity] must set this **before**
     * `setContent()`, not from [start]: Hilt resolves the launch-persistence
     * chain (`LaunchModule.provideSecretBackend` → … → `LaunchMachine`) during
     * the very first composition, to drive initial nav routing — earlier than
     * [start] runs (it's called after `setContent()`, which is fine for
     * [isE2EActive] since that only gates conversations setup much later, once
     * authenticated).
     */
    @Volatile
    var credentialFilePath: String? = null

    @Volatile
    private var ready = true
    private val client = OkHttpClient.Builder()
        .connectTimeout(10, java.util.concurrent.TimeUnit.SECONDS)
        .readTimeout(10, java.util.concurrent.TimeUnit.SECONDS)
        .build()
    private val JSON_TYPE = "application/json".toMediaType()

    @Volatile private var onboardingHost: OnboardingHost? = null
    @Volatile private var conversationsManagerHost: ConversationsManagerHost? = null
    @Volatile private var feedManagerHost: FeedManagerHost? = null
    @Volatile private var apiClient: com.fauna.app.core.ApiClient? = null
    @Volatile private var screenTimeStore: com.fauna.app.core.ScreenTimeStore? = null
    @Volatile private var atprotoSettingsHost: com.fauna.app.core.atproto.AtprotoSettingsHost? = null
    @Volatile private var actorScope: com.fauna.app.core.ActorScope? = null

    /** See [TestAgentEntryPoint.accountRegistry]. `null` until [start] (unit tests). */
    @Volatile private var registry: com.fauna.ffi.FfiAccountRegistry? = null

    /**
     * Scope for fire-and-forget agent pokes whose work must not ride the poll
     * loop's own coroutine — today `account_pump_now`, whose pass does network
     * I/O that would otherwise stall every later command behind it. A
     * `SupervisorJob` so one failed poke cannot cancel the next; cancelled by
     * [stop] so a torn-down agent leaves nothing running.
     */
    private val pokeScope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    /**
     * The current root [FocusManager] — refreshed every composition by
     * [com.fauna.app.ui.navigation.FaunaNavHost] (a `BuildConfig.DEBUG`-gated
     * `SideEffect`), so `focus_move` moves the SAME focus ring Compose's own
     * hardware Tab/Shift-Tab handling drives. Unlike [onboardingHost] and its
     * siblings above, this is not a Hilt singleton reachable from [start] — it
     * is composition-local, so `FaunaNavHost` (`src/main`) writes it directly,
     * which is why this is `var`, not `private var`. `null` before the first
     * composition or after the activity tears down; the `focus_move` arm
     * refuses rather than silently doing nothing when it is absent.
     */
    @Volatile
    var focusManager: androidx.compose.ui.focus.FocusManager? = null

    /**
     * The open "more reactions" picker's pick handler — the very `onPicked`
     * its hosted emoji2 `EmojiPickerView`'s `setOnEmojiPickedListener` calls
     * (`ConversationDetailScreen.kt`'s `ReactionPickerDialog`, which registers
     * it while the sheet is shown and clears it on dismiss). `null` when no
     * picker is open. Composition-local like [focusManager], so the screen
     * writes it directly. Read by the `type_text`[dm-reaction-more-button] arm
     * ([pickMoreReaction]).
     */
    @Volatile
    var moreReactionPick: ((String) -> Unit)? = null

    /**
     * Last `call_machine_method` result — the JSON-serialized return value for a
     * reader method (`provisioning_snapshot`, `provider_base_url`), or null for a
     * setter. Read back by [serializeState] into `machine_method_result`, which
     * `HttpBridgeDriver.call_machine_method` (tests/e2e-unified/drivers/http_bridge.py)
     * expects — the native twin of web's value-returning `__fauna_callMachineMethod`.
     * Mirrors linux's `SharedState.machine_method_result`.
     */
    @Volatile private var machineMethodResult: String? = null

    /**
     * The last `serve_enable_folder` outcome — `{"ok": true, "served_sets": N}` or
     * `{"ok": false, "error": …}`, the shape linux/tui/apple publish and
     * `tests/e2e-unified/helpers/webdav_roundtrip.py::serve_enable_folder` polls
     * as `webdav_serve_reply`. `null` while a request is in flight (and before the
     * first), so the poll waits rather than reading a previous request's answer.
     */
    @Volatile private var webdavServeReply: JSONObject? = null

    fun start(context: Context, bridgeUrl: String, appState: AppState) {
        // Latch E2E mode for process-global consumers (e.g. the conversations
        // manager host) before the early-return guard, so a re-entrant call
        // still leaves the flag set.
        isE2EActive = true
        if (job != null) return
        val db = FaunaDatabase.getInstance()
        // Reach the Hilt-managed singleton OnboardingHost so call_machine_method
        // commands target the same machine the on-screen UI observes. Required
        // by docs/goal/behavior/onboarding.md §"E2E bridge contract".
        val entryPoint = EntryPointAccessors.fromApplication(
            context.applicationContext,
            TestAgentEntryPoint::class.java,
        )
        // The app's own credential accessor, so the session door's writes and
        // the state provider's reads resolve the device id through the same
        // registry + install store the UI does.
        val storage = entryPoint.secureStorage()
        onboardingHost = entryPoint.onboardingHost()
        // The same Hilt @Singleton the conversations screens observe, so
        // `conversations_inject_send_failure` drives the on-screen manager.
        conversationsManagerHost = entryPoint.conversationsManagerHost()
        // The same Hilt @Singleton the Feed screens observe, so
        // `feed_inject_posts` drives the on-screen manager.
        feedManagerHost = entryPoint.feedManagerHost()
        // The same Hilt @Singleton the real app UI drives — a `patch` session
        // login must reconnect THIS instance, not a throwaway one (see
        // applySessionPatch's reconnect step below).
        apiClient = entryPoint.apiClient()
        // The same Hilt @Singleton the real app UI drives — `screen_time_heartbeat`
        // must poke the same store the ward's screen-time-lock overlay reads.
        screenTimeStore = entryPoint.screenTimeStore()
        // The same Hilt @Singleton the AT Protocol page observes — the D10
        // `atproto_delegation_advance_clock` command must repaint THAT
        // machine's snapshot, not a throwaway one.
        atprotoSettingsHost = entryPoint.atprotoSettingsHost()
        // The same canonical drop production's switch / sign-out / factory reset
        // go through, so this agent's own teardown arms cannot drift from them.
        actorScope = entryPoint.actorScope()
        registry = entryPoint.accountRegistry()
        registerSessionOverrideDrop(appState, entryPoint.accountStores())
        // `painted_errors` is fed per frame from the composition, in process;
        // MainActivity hands itself in here, after `setContent()`.
        (context as? android.app.Activity)?.let { E2eLoudSurfaces.installPaintedErrorObserver(it) }
        android.util.Log.i("TestAgent", "Starting with bridge: $bridgeUrl")
        job = CoroutineScope(Dispatchers.IO).launch {
            pollLoop(bridgeUrl, appState, storage, db, appState.messages)
        }
    }

    /**
     * Register the agent's own actor-scoped state — the [AppState] `session`
     * override layer — with the canonical drop
     * ([com.fauna.app.core.ActorScope.dropActorScopedState]).
     *
     * These six fields are the `set_state` override the state provider reads
     * *ahead of* the real stores ([serializeState]), and `actor_id` / `handle`
     * have no `SecureStorage` fallback behind them at all. Only this agent's own
     * `reset` / `logout` arms used to clear them, so a test that seeded actor A
     * and then drove a **real UI account switch** to B kept being told it was
     * still A for the rest of the process — and `actor_id` also routes
     * [profileNavTarget]. That is the e2e twin of windows' `AppDataSnapshot` and
     * apple's `data.*` serialization: a stale state-provider answer reads
     * downstream as a product bug (`e2e-conventions.md` point 11).
     *
     * Registered next to the state, like every other closer, and idempotent —
     * the registry keys by name, so a re-entrant [start] replaces rather than
     * accumulates.
     *
     * Lives in the debug source set with the rest of the automation surface
     * (convention 15): production never writes these fields, so there is
     * nothing for a shipping build to drop.
     */
    private fun registerSessionOverrideDrop(
        appState: AppState,
        accountStores: com.fauna.app.core.AccountStores,
    ) {
        accountStores.registerCloser("e2e-session-override") {
            appState.session.isAuthenticated = false
            appState.session.nodeUrl = null
            appState.session.secretHex = null
            appState.session.deviceId = null
            appState.session.handle = null
            appState.session.actorId = null
        }
    }

    fun stop() {
        job?.cancel()
        job = null
        // Cancel the children, not the scope: [pokeScope] is a val on this
        // object, and cancelling its SupervisorJob would leave a dead scope that
        // silently swallows every later poke if the agent is restarted.
        pokeScope.coroutineContext[Job]?.cancelChildren()
    }

    /**
     * Install the conversations mock backends iff the app launched in E2E mode.
     *
     * Exists so [com.fauna.app.core.conversations.ConversationsManagerHost] —
     * which lives in `src/main` and is compiled into the release APK — never
     * names `installMockBackendsForTest()` itself: that seam is a `test-helpers`
     * UniFFI export, absent from the production-flavored bindings `just
     * android-ffi` stages into `src/release/` (testing.md § convention 15). The
     * release twin of this file is a no-op, so the call site compiles either
     * way — the same "same-signature no-op twin" shape linux uses for
     * `start_test_agent_if_enabled`.
     */
    fun installMockBackendsIfE2E(manager: ConversationsManager) {
        if (isE2EActive) manager.installMockBackendsForTest()
    }

    // ── The fired-banner log (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`) ────────
    //
    // [com.fauna.app.core.conversations.MessageBannerObserver] lives in
    // `src/main` and compiles into every shipping APK, while the three shared
    // recorders are `test-helpers` UniFFI exports the shipping bindings do not
    // carry — so it reaches them through these delegates, whose release twins
    // are no-ops (the [installMockBackendsIfE2E] shape). Unconditional within the
    // debug build, like apple's `#if DEBUG` and windows' `#if DEBUG` arms: the
    // log is process-global and appending to it costs nothing. The `ffi`-aliased
    // imports keep these same-named members from calling themselves.

    /** Bump the diff-tick start counter — before the tick's snapshot read. */
    fun bannerPassStarted() = ffiBannerPassStarted()

    /** Record a banner handed to the platform — at the firing site only. */
    fun recordFiredBanner(threadId: String, label: String) = ffiRecordFiredBanner(threadId, label)

    /** Bump the diff-tick completion counter — after the tick's last fire. */
    fun bannerPassCompleted() = ffiBannerPassCompleted()

    /**
     * Count one connection-state value the `connection-status` indicator
     * received (`fauna_e2e_agent::CONNECTION_REPORTS_KEY`) — called by
     * `ApiClient.startConnectionStatePump` on EVERY value, repeats included.
     * Production code, so it reaches the test-flavor counter through this
     * delegate, whose shipping twin is a no-op (the [bannerPassStarted] shape).
     */
    fun observeConnectionReport(state: com.fauna.ffi.FfiConnectionState) =
        E2eLoudSurfaces.observeConnectionReport(state)

    // ── Convention 14's negative-assert counters ─────────────────────────────
    //
    // `session_generation` (`fauna_e2e_agent::SESSION_GENERATION_KEY`) and
    // `activation_gestures` (`fauna_e2e_agent::ACTIVATION_GESTURES_KEY`) —
    // that crate owns both contracts: start at 0 for a fresh process, only ever
    // grow. The writers are production call sites (the one canonical teardown,
    // [com.fauna.app.core.ActorScope.dropActorScopedState], and the switcher
    // row's activation handler, [com.fauna.app.core.AccountReauth.activationGesture]),
    // so they reach these through the release twins' no-op shape — windows'
    // `E2eSessionCounters` is the same gated-real-plus-twin idiom. Atomic, not
    // `@Volatile` + `++`: incremented on the main thread, read on the poll
    // thread, and a lost update under-counts a teardown — the one direction
    // that turns a negative assert into a false pass.
    private val sessionGenerationCounter = java.util.concurrent.atomic.AtomicLong(0)
    private val activationGesturesCounter = java.util.concurrent.atomic.AtomicLong(0)

    /** Count one INITIATED session teardown — at the decision, synchronously. */
    fun recordSessionTeardown() {
        sessionGenerationCounter.incrementAndGet()
    }

    /** Count one COMPLETED switcher-row activation gesture, whatever it decided. */
    fun recordActivationGesture() {
        activationGesturesCounter.incrementAndGet()
    }

    /** The published `session_generation` value. */
    val sessionGeneration: Long get() = sessionGenerationCounter.get()

    /** The published `activation_gestures` value. */
    val activationGestures: Long get() = activationGesturesCounter.get()

    /**
     * Test seam: point the conversations arms at a manager host a Robolectric pin
     * owns, so they can be proven to actually MOVE the shared manager rather than
     * only to be *recognised*. [start] sets the real Hilt singleton.
     *
     * Without this the arms are only reachable with a null host, where every one
     * of them declines identically at the wiring check — a pin that cannot tell a
     * working arm from an empty one.
     */
    internal fun setConversationsManagerHostForTest(host: ConversationsManagerHost?) {
        conversationsManagerHost = host
    }

    /** Test seam, same shape as [setConversationsManagerHostForTest]: point the
     *  session-patch reconnect (see `applySessionPatch`) at a Robolectric pin's
     *  double instead of the real Hilt [com.fauna.app.core.ApiClient] singleton. */
    internal fun setApiClientForTest(client: com.fauna.app.core.ApiClient?) {
        apiClient = client
    }

    /** Test seam, same shape as [setApiClientForTest]: point `reset`/`logout`/
     *  `applySessionPatch`'s teardown call (see [dispatchCommand],
     *  `applySessionPatch`) at a Robolectric pin's double instead of the real
     *  Hilt [com.fauna.app.core.ActorScope] singleton — [start] only ever
     *  populates [actorScope] from `entryPoint.actorScope()`, so without this
     *  seam a test that never calls [start] leaves it `null` and every drop
     *  call below is a silent safe-call no-op. */
    internal fun setActorScopeForTest(scope: com.fauna.app.core.ActorScope?) {
        actorScope = scope
    }

    private suspend fun pollLoop(
        bridgeUrl: String,
        appState: AppState,
        storage: SecureStorage,
        db: FaunaDatabase?,
        messages: AppMessages = AppMessages(),
    ) {
        var lastCommandId = ""
        var pushCounter = 0

        while (true) {
            try {
                val cmd = fetchCommand(bridgeUrl)
                if (cmd != null) {
                    val id = cmd.optString("id", "")
                    val action = cmd.optString("action", "patch")
                    lastCommandId = id
                    android.util.Log.i("TestAgent", "Processing command $id (action: $action)")

                    ready = false
                    try {
                        withContext(Dispatchers.Main) {
                            processCommand(action, cmd, appState, storage, db)
                        }
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        // An arm that throws past its own catch must not wedge the
                        // agent. Without the `finally` below, `ready` stayed false
                        // forever and every later command timed out; and the throw
                        // itself only reached logcat. `NavController.navigate` on a
                        // route absent from the graph is the reachable case.
                        android.util.Log.e("TestAgent", "command '$action' threw: ${e.message}")
                        withContext(Dispatchers.Main) {
                            appState.messages.reportRefusedAgentCommand(
                                action,
                                "threw ${e::class.simpleName}: ${e.message}",
                            )
                        }
                    } finally {
                        ready = true
                    }
                    pushState(bridgeUrl, lastCommandId, appState, storage, db, messages)
                    pushCounter = 0
                } else {
                    // No command — push state every 5th cycle (~1s at 200ms interval)
                    pushCounter++
                    if (pushCounter >= 5) {
                        pushState(bridgeUrl, lastCommandId, appState, storage, db, messages)
                        pushCounter = 0
                    }
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                android.util.Log.e("TestAgent", "Poll error: ${e.message}")
                delay(1000)
                continue
            }
            delay(200)
        }
    }

    private fun fetchCommand(bridgeUrl: String): JSONObject? {
        val request = Request.Builder().url("$bridgeUrl/app/commands").build()
        client.newCall(request).execute().use { resp ->
            if (resp.code == 204 || !resp.isSuccessful) return null
            val body = resp.body?.string() ?: return null
            return JSONObject(body)
        }
    }

    private fun pushState(
        bridgeUrl: String,
        lastCommandId: String,
        appState: AppState,
        storage: SecureStorage,
        db: FaunaDatabase?,
        messages: AppMessages = AppMessages(),
    ) {
        val state = serializeState(appState, storage, db, messages)
        val payload = JSONObject().apply {
            put("last_command_id", lastCommandId)
            put("ready", ready)
            put("state", state)
        }
        val body = payload.toString().toRequestBody(JSON_TYPE)
        val request = Request.Builder()
            .url("$bridgeUrl/app/state")
            .post(body)
            .build()
        try {
            client.newCall(request).execute().close()
        } catch (_: IOException) { }
    }

    /** `data.contacts` (client_capabilities.py EXPECTED_FIELDS) — mirrors linux's
     *  `contacts` mapping in `main.rs`. Pure so it's directly Robolectric-testable
     *  without a live Room DB. */
    internal fun contactsToJson(rows: List<Contact>): JSONArray =
        JSONArray().apply {
            rows.forEach { c ->
                put(
                    JSONObject().apply {
                        put("peer_id", c.peerId)
                        put("status", c.status)
                        put("handle", c.handle ?: JSONObject.NULL)
                        put("node_url", c.nodeUrl ?: JSONObject.NULL)
                    },
                )
            }
        }

    /** `data.knocks` — mirrors linux's `knocks` mapping in `main.rs`. */
    internal fun knocksToJson(rows: List<Knock>): JSONArray =
        JSONArray().apply {
            rows.forEach { k ->
                put(
                    JSONObject().apply {
                        put("sender", k.sender)
                        put("sender_node", k.senderNode)
                        put("summary", k.summary)
                        put("timestamp", k.createdAt)
                    },
                )
            }
        }

    /** `data.sync.files` — mirrors linux's `sync_files` mapping in `main.rs`. */
    internal fun syncFilesToJson(rows: List<SyncFile>): JSONArray =
        JSONArray().apply {
            rows.forEach { f ->
                put(
                    JSONObject().apply {
                        put("path", f.path)
                        put("folder", f.folder)
                        put("size_bytes", f.sizeBytes)
                        put("state", f.state.name.lowercase())
                    },
                )
            }
        }

    /**
     * The state protocol's `messages` value: a populated object when the app-wide
     * funnel ([AppMessages]) has something to say, and `null` outright when it
     * does not.
     *
     * `errorForDisplay` puts a refused agent command ahead of the page's own
     * error, so a refusal rides this field and not only the banner (convention
     * 11).
     *
     * **Why it must be null and not a present-but-all-null object.**
     * `actions/__init__.py::_message_from_state` returns `None` only when the
     * `messages` key is ABSENT — a key present with a *null value* resolves to
     * `""`, which permanently short-circuits `error_text()`/`has_error()`'s
     * fallback to the `error-message` element. AppMessages is not android's only
     * error surface: page-scoped ViewModels own their own error flow
     * (`AdminDnsVM.error`, `AdminNestVM.error`, `MutedWordsVM.errorMessage`, …)
     * and render it into the page's own `error-message` element without ever
     * publishing to the funnel. While this object was unconditionally present,
     * every one of those page errors read back as `""` — a surfaced failure
     * indistinguishable from a swallowed one, which is the dropped-error-surface
     * class ([`e2e-conventions.md`] convention 2's rider, obligation (b); tui
     * fixed it in its own idiom, `apps/tui.md` § The page-module contract).
     * Deferring to the element when the funnel is silent makes those pages
     * readable without requiring every present and future screen to remember to
     * publish. This is the shape web ratified for the same defect
     * (`tests/e2e-unified/web-bridge/agent.js`, its `bannerMounted &&
     * bannerHasMessage` gate).
     */
    internal fun messagesJson(messages: AppMessages): Any {
        val error = messages.errorForDisplay()
        val warning = messages.warning.value
        val info = messages.info.value
        if (error == null && warning == null && info == null) return JSONObject.NULL
        return JSONObject().apply {
            put("error", error ?: JSONObject.NULL)
            put("warning", warning ?: JSONObject.NULL)
            put("info", info ?: JSONObject.NULL)
        }
    }

    /**
     * Re-parse [ApiClient.feedPostsJson]'s raw string into the `data.feed.posts`
     * array — the shared `fauna_feed::feed_posts_json` derivation, re-parsed
     * rather than re-derived (the `conversationThreadsJson`/`successionWitnessStateJson`
     * idiom above). `null` (no manager built yet, or a malformed string) maps
     * to an empty array — the legitimate pre-auth zero every app's `feed.posts`
     * starts at, never an absent key (android always publishes this leg).
     */
    /**
     * `conversations_inject_inbound`'s whole body: the command JSON to the shared parser
     * (`inject_inbound_from_test_json`). Envelope keys (`id`, `action`) ride along and
     * the parser ignores them. A throw is the refusal reason, never a log line — a
     * swallowed inject throw once read as a lost async race on apple, costing several
     * misdiagnosis sessions (testing.md point 11).
     */
    internal fun injectInbound(manager: ConversationsManager, cmd: JSONObject): String? =
        try {
            manager.injectInboundFromTestJson(cmd.toString())
            android.util.Log.i("TestAgent", "conversations_inject_inbound ok")
            null
        } catch (e: Exception) {
            android.util.Log.e("TestAgent", "conversations_inject_inbound failed: ${e.message}")
            "conversations_inject_inbound threw ${e::class.simpleName}: ${e.message}"
        }

    /**
     * `conversations_evict_attachment`'s whole body. Nothing evicted is a FAILED command,
     * never an ack (convention 11): a render asserted after a no-op evict witnesses nothing.
     */
    internal fun evictAttachment(manager: ConversationsManager, cmd: JSONObject): String? {
        val threadId = cmd.optString("thread_id")
        val filename = cmd.optString("filename")
        val evicted = manager.evictThreadAttachmentsForTest(threadId, filename)
        return if (evicted == 0u) {
            "no resident attachment named \"$filename\" in thread \"$threadId\""
        } else {
            null
        }
    }

    /**
     * [applied] — the compose field's displayed text as its styling handed it over — as
     * linux's `text-runs` JSON: `[{text, tags: [{name, weight, family, scale, left_margin,
     * invisible}]}]`, one tag per run listing what the run sets (a default is `null`).
     * `null` when no compose field is on screen.
     */
    internal fun composeTextRunsJson(applied: AnnotatedString?): String? {
        if (applied == null) return null
        val runs = JSONArray()
        for (run in appliedRuns(applied)) {
            val tag = JSONObject()
                .put("name", JSONObject.NULL)
                .put("weight", run.weight ?: JSONObject.NULL)
                .put("family", run.family ?: JSONObject.NULL)
                .put("scale", run.scale?.toDouble() ?: JSONObject.NULL)
                .put("left_margin", run.leftMargin?.toDouble() ?: JSONObject.NULL)
                .put("invisible", false)
            runs.put(JSONObject().put("text", run.text).put("tags", JSONArray().put(tag)))
        }
        return runs.toString()
    }

    internal fun feedPostsJson(raw: String?): JSONArray {
        return raw
            ?.let { j -> runCatching { org.json.JSONTokener(j).nextValue() as? JSONArray }.getOrNull() }
            ?: JSONArray()
    }

    internal fun serializeState(
        appState: AppState,
        storage: SecureStorage,
        db: FaunaDatabase?,
        messages: AppMessages = AppMessages(),
    ): JSONObject {
        val session = JSONObject().apply {
            // e2e-conventions.md convention 11 — "authenticated" means the
            // authenticated app is MOUNTED, not "a secret is on disk". A stored
            // credential routes a relaunch through the launch wizard just as
            // often as into the app (verify-404, an awaiting-manual-dns "Almost
            // ready"); `!appState.isOnboarding` is the same fact FaunaNavHost
            // itself branches on to decide whether the main app is mounted
            // (flips false only at AppLaunchVM's NavTarget.Authenticated or a
            // wizard exit). `appState.session.isAuthenticated` is the explicit
            // `set_state` override — production code never writes it, only the
            // login shortcut below (sets it alongside isOnboarding=false) and
            // reset/logout (clear it) do — so it wins whenever a test sets it
            // directly, mirroring windows' `_testAuthenticatedOverride` /
            // linux's `SessionOverride::authenticated`.
            put("authenticated", appState.session.isAuthenticated || !appState.isOnboarding)
            put("node_url", appState.session.nodeUrl ?: storage.nestUrl ?: JSONObject.NULL)
            put("secret_hex", appState.session.secretHex ?: storage.secretHex ?: JSONObject.NULL)
            put("device_id", appState.session.deviceId ?: storage.deviceId ?: JSONObject.NULL)
            put("actor_id", appState.session.actorId ?: JSONObject.NULL)
            put("handle", appState.session.handle ?: JSONObject.NULL)
        }

        val currentView = if (appState.isOnboarding) {
            "onboarding/identity-choice"
        } else {
            appState.navController?.currentDestination?.route ?: "conversations"
        }
        val nav = JSONObject().apply {
            put("stack", JSONArray().put(JSONObject().put("view", currentView)))
            put("modal", JSONObject.NULL)
        }

        // Read counts from Room (called on IO thread, synchronous is fine)
        val convCount = try { db?.conversationDao()?.countSync() ?: 0 } catch (_: Exception) { 0 }

        // Real per-row fields (app_capabilities.py EXPECTED_FIELDS) — mirrors
        // linux's contacts/knocks/sync serialization (main.rs). conversations
        // above stays a count-only placeholder (out of this pass's scope; a
        // pre-existing gap — app_capabilities.py declares android false for it).
        val contacts = try { contactsToJson(db?.contactDao()?.getAllSync() ?: emptyList()) } catch (_: Exception) { JSONArray() }
        val knocks = try { knocksToJson(db?.knockDao()?.getAllSync() ?: emptyList()) } catch (_: Exception) { JSONArray() }
        val syncFiles = try { syncFilesToJson(db?.syncFileDao()?.getAllSync() ?: emptyList()) } catch (_: Exception) { JSONArray() }

        // `data.conversation_threads` — the shared e2e state-serialization
        // contract (`state_json::conversation_threads_json`, tui/linux/web's
        // same shape). android is UniFFI-mediated, so it re-parses the JSON
        // string `ConversationsManager.conversationThreadsJson()` returns —
        // the established `conv_receive_cycles_json`/`mls_folded_commits_json`
        // passthrough idiom (the siblings, below) — rather than
        // re-deriving the row shape (thread_id/label/snippet/rail/flavor/
        // unread_count/participant_count/message_count/
        // message_subject_lines/participant_actor_ids/channel_id_hex) in
        // Kotlin. `conversationsManagerHost` is a DIFFERENT Room table
        // (`ConversationDao`, post-comment threads) — do not conflate them.
        //
        // Reads off `host.manager` (== `sessionManager ?: bareManager`), NOT
        // `host.session`: under a plain e2e run `startConversationsSession`
        // never activates (ConversationsManagerHost.kt's
        // `isE2EActive && !isRealConversationsActive` guard), so `session`
        // stays null forever while `conversations_inject_inbound` injects
        // into `host.manager` (`bareManager`, the mock-backends target) —
        // reading off `session` returned `[]` unconditionally, same bug
        // shape the 2026-08-27 apple regression hit;
        // manager-level `conversation_threads_json()` was added to
        // `libs/fauna-conversations::ConversationsManager` for that fix and
        // is reused here rather than re-deriving a second Kotlin path).
        val convThreadsManager = conversationsManagerHost?.manager
        val convThreads = convThreadsManager
            ?.let { m -> runCatching { org.json.JSONTokener(m.conversationThreadsJson()).nextValue() as? JSONArray }.getOrNull() }
            ?: JSONArray()

        // `data.succession_witness` — the MEMBER side of a succession: what this
        // seat's inbound poll did with the statements it saw, what its harvest
        // producer managed per peer, and what its witness made of each identity.
        // The same passthrough idiom as `conversation_threads` above: shared Rust
        // renders the shape (`witness::state_json`), android re-parses the string
        // rather than re-deriving a second Kotlin path.
        //
        // `null` — never `{}` — before a conversations session exists. The two
        // read differently and only the second indicts the poll, which is the
        // whole reason the observable was minted
        // (`succession-aftermath.md` § Propagation → *MLS groups*).
        val successionWitness = conversationsManagerHost
            ?.successionWitnessStateJson()
            ?.let { json -> runCatching { org.json.JSONTokener(json).nextValue() }.getOrNull() }

        // `data.feed.posts` — the shared `PostSummary` state dump every other
        // app already publishes (`fauna_feed::feed_posts_json`, `feed.md` §
        // Interaction bar), which `tests/e2e-unified/actions/feed.py`'s
        // id-keyed post readers rely on unconditionally. `ApiClient.feedPostsJson()`
        // is a non-building peek (never builds a manager or kicks off draft
        // restore, unlike [FeedManagerHost]'s own accessor) — see [feedPostsJson].
        val feedPosts = feedPostsJson(apiClient?.feedPostsJson())

        val data = JSONObject().apply {
            put("conversations", JSONArray().apply { repeat(convCount) { put(JSONObject()) } })
            put("conversation_threads", convThreads)
            put("succession_witness", successionWitness ?: JSONObject.NULL)
            put("contacts", contacts)
            put("knocks", knocks)
            put("feed", JSONObject().put("posts", feedPosts))
            // Events and notifications stay unpopulated: neither has a Room-backed
            // (or otherwise process-global) cache TestAgent can read synchronously
            // today — EventsVM/NotificationsVM are per-screen Hilt ViewModels, not
            // singletons like ConversationsManagerHost/OnboardingHost. Wiring either
            // for real needs a singleton state holder — a scoped follow-up, not
            // forced in here.
            put("events", JSONArray())
            put("notifications", JSONObject().put("unread_count", 0))
            put("sync", JSONObject().put("files", syncFiles))
        }

        val msgs = messagesJson(messages)

        // Reader methods (e.g. provisioning_snapshot) stash a JSON string in
        // machineMethodResult; re-parse it into a structured value here (mirrors
        // linux's `serde_json::from_str::<Value>`) so the driver reads a dict/
        // primitive, not a doubly-JSON-encoded string. Null when the last method
        // was a setter or nothing has run yet.
        val machineMethodResultValue = machineMethodResult?.let {
            try { org.json.JSONTokener(it).nextValue() } catch (_: Exception) { JSONObject.NULL }
        } ?: JSONObject.NULL

        // convention 14 observables, published TOP-LEVEL (same depth as `session`/
        // `nav`) — mirrors tui/linux/web's `conv_receive_cycles`/`mls_folded_commits`
        // exactly (`state_json.rs` owns the derivation; this only re-parses the
        // JSON string ConversationsSession's UniFFI twin returns, never re-derives
        // the counts). No session yet (pre-auth) is a legitimate zero, matching
        // `conv_receive_cycles_json`/`mls_folded_commits_json_for_session`'s own
        // "None ⇒ zero, never null" contract — `null` is reserved for an app that
        // does not publish the key at all.
        val convSession = conversationsManagerHost?.session
        val convReceiveCycles = convSession
            ?.let { s -> runCatching { org.json.JSONTokener(s.convReceiveCyclesJson()).nextValue() as? JSONObject }.getOrNull() }
            ?: JSONObject().put("started", 0).put("completed", 0).put("exit", JSONObject.NULL)
        val mlsFoldedCommits = convSession
            ?.let { s -> runCatching { org.json.JSONTokener(s.mlsFoldedCommitsJson()).nextValue() as? JSONObject }.getOrNull() }
            ?: JSONObject()

        // `account_pump_cycles` — the W3 (account-data-plane.md § Workstreams) account plane's convention-14 observable
        // (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`, beside the
        // `account_pump_now` poke below). Android HOSTS the runtime in-process
        // (`ApiClient.startAccountRuntime`), so unlike tui/linux there is no
        // co-located agent to lose the W5.1 election to: a healthy signed-in app
        // reports `runtime: true, holder: true`, which is the iOS branch of
        // `test_account_runtime_pump.py`'s contract.
        //
        // The JSON shape is produced by the SHARED
        // `fauna_client_account_runtime::account_pump_cycles_json` and only
        // re-parsed here — the same passthrough idiom as the two counters above,
        // so android cannot publish a second shape of one cross-app contract.
        // The four states are deliberately distinguishable and conflating them is
        // what made an earlier e2e fail on a healthy app: key ABSENT = no leg at
        // all (never android, now that this line exists); `runtime: false` = no
        // assembled runtime; `runtime: true, holder: false` = assembled but
        // another process holds the engine singleton, so the counters are frozen
        // *correctly*; both true = assembled and pumping.
        //
        // No connected client (pre-auth) reports the shared function's own `None`
        // branch verbatim — `{0, 0, runtime: false, holder: false}` — rather than
        // omitting the key, because omission means "this app has no leg" and
        // android does.
        val accountPumpCycles = apiClient?.accountPumpCyclesJson()
            ?.let { j -> runCatching { org.json.JSONTokener(j).nextValue() as? JSONObject }.getOrNull() }
            ?: JSONObject()
                .put("started", 0)
                .put("completed", 0)
                .put("runtime", false)
                .put("holder", false)

        // `feed_reloads` — convention 14's reload-barrier observable
        // (`fauna_e2e_agent::FEED_RELOADS_KEY`), tui/linux/web's own shape.
        // `ApiClient.feedReloadsJson()` is a non-building peek (never
        // `feedManager(observer)`/`FeedManagerHost.manager()`, both of which
        // construct a manager as a side effect); no manager yet is a
        // legitimate zero, the shared derivation's own "None ⇒ zero, never
        // null" contract (`fauna_feed::feed_reloads_json`) — `null`/absent is
        // reserved for an app that publishes no leg at all, and android does.
        // ⚠ The zero below is the ONE place this shape is re-spelled rather
        // than read, so it must grow with the derivation: it went stale within a
        // day of `committed_gen` joining the triple (2026-08-23), and a SHORT
        // zero reads as the no-leg refusal — the opposite of what it means.
        val feedReloads = apiClient?.feedReloadsJson()
            ?.let { j -> runCatching { org.json.JSONTokener(j).nextValue() as? JSONObject }.getOrNull() }
            ?: JSONObject().put("started", 0).put("completed", 0).put("committed_gen", 0)

        // `connection` — the cross-app **connection barrier**'s observable
        // (`fauna_e2e_agent::CONNECTION_KEY`), `{state, online}`. Every app
        // greys an `OnlineOnly` affordance while its transport word is offline
        // (`fauna_protocol::offline_class::affordance`, the rule `faunaGate`
        // binds), `"connecting"` is one of the offline words, and 276 of the
        // 628 registered kinds are `OnlineOnly` — so a test driving an
        // online-only control on a freshly launched app races the WS handshake
        // and loses under load. `helpers/connection.py::wait_until_online`
        // waits this out once per login.
        //
        // ⚠ The boolean is `connectionIsOnline`, NEVER `word == "connected"`.
        // The gate's polarity is asymmetric on purpose — online unless the word
        // is a *known* offline word, so a future state word leaves controls
        // live — and an equality test inverts it in the direction that HANGS a
        // barrier on exactly the case the rule was built to tolerate. The word
        // itself likewise comes from `connectionStateWord`, not a Kotlin
        // `when`, for the reason `OfflineGate.kt` gives at length.
        //
        // ⚠ **Unreachable rule ⇒ NULL, not "online" — the opposite of
        // `faunaGate`'s fail-open, deliberately.** A gate that cannot reach its
        // verdict must not block the user; a *barrier* that cannot reach it
        // must not wave a test through onto a desensitized app. So no client
        // yet, or a throw from the native lib, publishes `null` ("cannot answer
        // yet"), which the waiter polls through and finally fails loudly on —
        // never a silent pass.
        val connection = apiClient?.connectionState?.value
            ?.let { state ->
                runCatching {
                    val word = connectionStateWord(state)
                    JSONObject().put("state", word).put("online", connectionIsOnline(word))
                }.getOrNull()
            }
            ?: JSONObject.NULL

        // `message_banners` — the new-message banner log that witnesses
        // `conversations` outcome 11 (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`).
        // Shared Rust owns the shape (`notification::message_banners_json`); this
        // re-parses its UniFFI text face, the passthrough idiom of the keys above.
        // The log is process-global, so it needs no manager and no session: an
        // app that has built the firing publishes it from launch, zeros and all,
        // and the zeros are a DIFFERENT answer from the key's absence (the
        // constant's doc) — which is why a parse failure publishes `null`, the
        // reader's refusal, rather than a hand-spelled zero.
        val messageBanners = runCatching {
            org.json.JSONTokener(messageBannersJsonText()).nextValue() as? JSONObject
        }.getOrNull()

        return JSONObject().apply {
            put("session", session)
            put("nav", nav)
            put("settings", JSONObject())
            put("data", data)
            put("messages", msgs)
            put("machine_method_result", machineMethodResultValue)
            put("webdav_serve_reply", webdavServeReply ?: JSONObject.NULL)
            put("conv_receive_cycles", convReceiveCycles)
            put("mls_folded_commits", mlsFoldedCommits)
            put("account_pump_cycles", accountPumpCycles)
            put("feed_reloads", feedReloads)
            put("message_banners", messageBanners ?: JSONObject.NULL)
            put("connection", connection)
            // Top-level, like every app's: `helpers/waiting.py` reads both keys
            // at depth one (`session_generation` / `activation_gestures`).
            put("session_generation", sessionGeneration)
            put("activation_gestures", activationGestures)
            // The launch clock this process signs in on — the tui/linux/web/apple
            // `clock` twin (`fauna_e2e_agent::CLOCK_KEY` owns the shape:
            // `{"offset_secs", "now_secs"}`), the wrong-clock launch witness's
            // in-app control that the FAUNA_E2E_CLOCK_OFFSET_SECS seed reached
            // this process. Both getters are `test-helpers` seams, present only in
            // the debug flavor's bindings this source set compiles against.
            put("clock", JSONObject().apply {
                put("offset_secs", launchClockOffsetSecsForTest())
                put("now_secs", launchClockNowSecsForTest())
            })
            // `barrier_probe` / `barrier_ack_probe` — top-level, like every app's.
            BarrierTestCommand.putState(this)
            // `connection_reports` / `painted_errors` / `alert_sweep_passes` —
            // top-level, like every app's.
            E2eLoudSurfaces.putState(this)
        }
    }

    /**
     * Dispatch, then make a refusal **loud on the app's own `error-message`** —
     * convention 11's "honour it or fail loudly", in the one place every refusal
     * funnels through, so no future arm can add a silent `return`.
     *
     * A wrapper rather than a line at the end of the dispatch, for the same reason
     * tui splits `apply_command`/`dispatch_command`: an arm that declines early
     * (a bad payload, a manager that is still null) returns from the middle of the
     * `when` and would bypass an end-of-function hook.
     */
    internal suspend fun processCommand(
        action: String,
        cmd: JSONObject,
        appState: AppState,
        storage: SecureStorage,
        db: FaunaDatabase?,
    ) {
        val refusal = dispatchCommand(action, cmd, appState, storage, db)
        if (refusal != null) {
            appState.messages.reportRefusedAgentCommand(action, refusal)
        }
    }

    /** Mirrors `fauna_e2e_agent::FOCUS_MOVE_MAX_TIMES` — see `focus_move`'s arm. */
    private const val FOCUS_MOVE_MAX_TIMES = 256

    /**
     * Render a payload field for a refusal message — `absent` when the key is
     * missing (Kotlin `null`, per this file's `cmd.has(key)`-guarded reads),
     * `null` when it is present but JSON-null (`JSONObject.NULL`), else its
     * value — mirroring `fauna_e2e_agent::describe` (and apple's `describe`
     * twin) so a failing walk names the exact mistake.
     */
    /**
     * The on-screen feed manager a `feed_*` test arm drives, or the refusal that
     * names [action] and the missing collaborator: the host not wired yet
     * ([start] has not run), or no manager because the session is pre-auth.
     */
    private fun feedManagerFor(action: String): Result<com.fauna.ffi.FfiFeedManager> {
        val host = feedManagerHost ?: return Result.failure(
            IllegalStateException(
                "$action arrived before the FeedManagerHost was wired (TestAgent.start has not run yet)",
            ),
        )
        val manager = host.manager()
            ?: return Result.failure(IllegalStateException("$action: no feed manager (pre-auth)"))
        return Result.success(manager)
    }

    /**
     * `feed_inject_error`'s `{key?, message?}` → the `LocalizedText::key_arg`
     * pair, an absent or empty field falling back to the `feed.error_load`
     * carrier a real failed fetch leaves — the defaults tui, web and linux use.
     */
    internal fun feedInjectErrorArgs(cmd: JSONObject): Pair<String, String> {
        fun arg(k: String, default: String) =
            cmd.optString(k, "").takeIf { it.isNotEmpty() && !cmd.isNull(k) } ?: default
        return arg("key", "feed.error_load") to arg("message", "feed load failed")
    }

    private fun describeCommandField(v: Any?): String = when {
        v == null -> "absent"
        v == JSONObject.NULL -> "null"
        else -> v.toString()
    }

    /**
     * Run one agent command. Returns `null` when the command was **honoured**, or
     * the reason it was **refused** — an unknown action, a payload the arm cannot
     * use, a collaborator that is not wired yet, or a seam that threw.
     *
     * Every `return` of a non-null reason is a convention-11 refusal: it reaches
     * the driver on `error-message` via [processCommand]. Never return `null` for
     * a command that did nothing — that is the silent drop the convention exists
     * to prevent (it produces no error AND no effect, so the test fails three
     * steps later on a read that looks like a genuine product bug).
     */
    private suspend fun dispatchCommand(
        action: String,
        cmd: JSONObject,
        appState: AppState,
        storage: SecureStorage,
        db: FaunaDatabase?,
    ): String? {
        val state = cmd.optJSONObject("state")
        when (action) {
            "reset" -> {
                android.util.Log.i("TestAgent", "Executing reset")
                // Sign-out-shaped: `storage.clear()` below wipes the WHOLE
                // `fauna_secure_prefs` file — the same physical file the
                // account-store registry's SharedPrefsSecretBackend writes
                // the writer key into (SecureStorage's own doc comment) — so
                // this sweep takes the writer key with it, same reasoning as
                // iOS's `resetToFactory` (`sync-agent-credentials.md` §
                // Credential model → *The signed-out reconcile*). Retire the
                // enrollment nest-side FIRST, while the runtime still holds
                // the key.
                apiClient?.stopAccountRuntimeForSignOutAwaited()
                runCatching { registry?.clearAll() }
                storage.clear()
                try { db?.clearAllTables() } catch (_: Exception) {}
                // Tear down the LIVE session too, through the SAME canonical drop
                // production's sign-out uses (AccountSettingsVM.signOut) —
                // otherwise a reused app process carries a stale WS-RPC session,
                // a live custodian push loop, or a seeded session override across
                // the per-test boundary into whatever the next test's login
                // builds. Going through the one door is also what keeps this arm
                // from drifting out of step with production: the session-override
                // fields below are cleared BY the drop (see
                // [registerSessionOverrideDrop]), not by a list kept here.
                actorScope?.dropActorScopedState()
                appState.isOnboarding = true
                // The per-test boundary: `reset` is the ONLY clear point for the
                // refusal slot (AppMessages.refusedAgentCommand), so a refusal is
                // visible for exactly one test and cannot leak into the next test
                // of a reused app process.
                appState.messages.clear()
                appState.messages.clearRefusedAgentCommand()
                // The barrier probe's slots, so a token cannot leak into the
                // next test of a reused app process.
                BarrierTestCommand.clear()
                // A fresh dial budget, as every app's reset gives: a burst a
                // failure run spent must not carry into the next test of a
                // reused app process (`fauna_ws_substrate::dial_budget`).
                dialBudgetClearForTest()
            }
            "logout" -> {
                android.util.Log.i("TestAgent", "Executing logout")
                // Sign-out-shaped, same reasoning as the "reset" arm above.
                apiClient?.stopAccountRuntimeForSignOutAwaited()
                runCatching { registry?.clearAll() }
                storage.clear()
                // Mirrors production sign-out (AccountSettingsVM.signOut) by
                // calling the very same canonical drop — without this the live
                // WS-RPC session, and the custodian push loop behind it, outlive
                // the credentials that authorized them. The session-override
                // fields are cleared BY the drop (see
                // [registerSessionOverrideDrop]).
                actorScope?.dropActorScopedState()
                appState.isOnboarding = true
                BarrierTestCommand.clear()
            }
            "patch" -> {
                if (state == null) return "the command carried no `state` object to apply"
                state.optJSONObject("session")?.let { applySessionPatch(it, appState, storage) }
                state.optJSONObject("nav")?.let { nav ->
                    applyNavPatch(nav, appState)?.let { return it }
                }
                state.optJSONObject("compose")?.let { compose ->
                    applyComposePatch(compose)?.let { return it }
                }
                state.optJSONObject("type_text")?.let { typed ->
                    applyTypeTextPatch(typed)?.let { return it }
                }
            }
            "call_machine_method" -> {
                // docs/goal/behavior/onboarding.md §"E2E bridge contract": dispatch
                // the named OnboardingMachine method with the JSON-encoded
                // arg. Per machine_test_setter.py the helpers use this for
                // set_step_for_test, set_handle_check_snapshot_for_test,
                // set_invite_request_snapshot_for_test, set_provisioning_snapshot_for_test,
                // set_current_handle, etc.
                val methodName = cmd.optString("method")
                if (methodName.isNullOrEmpty()) return "the command carried no `method` name"
                val jsonArg = cmd.optString("json_arg") ?: ""
                val host = onboardingHost
                    ?: return "call_machine_method($methodName) arrived before the " +
                        "OnboardingHost was wired (TestAgent.start has not run yet)"
                try {
                    // The ASYNC value-returning dispatch (iOS's twin,
                    // `FaunaApp.swift` `callMachineMethod`): it also drives the
                    // machine's async readers (`request_age_nonce`, …) to
                    // completion, which the sync `callMachineMethodWithResult`
                    // answers with nothing.
                    machineMethodResult = host.machine.callMachineMethodAsync(methodName, jsonArg)
                    android.util.Log.i("TestAgent", "call_machine_method($methodName) ok")
                } catch (e: Exception) {
                    // Reason, not a logcat line: a swallowed-to-logcat exception here
                    // is indistinguishable from a real product bug to a test reading
                    // downstream state. It rides the nav-durable refusal slot rather
                    // than the general banner, which navigation wipes.
                    android.util.Log.e("TestAgent", "call_machine_method($methodName) failed: ${e.message}")
                    return "call_machine_method($methodName) threw ${e::class.simpleName}: ${e.message}"
                }
            }
            "device_set_state" -> {
                // e2e-only: android twin of tui's `device_set_state` automation
                // arm (`apps/fauna-tui/src/automation.rs`) and linux's
                // `"device_set_state"` bridge command
                // (`apps/fauna-linux/src/main.rs`) —
                // whether `device_id_hex`'s plane `fauna.state.device-set` row
                // reads Removed/Enrolled from THIS app's own account runtime.
                // Rides the same `machine_method_result` reader-value wire
                // contract `call_machine_method` uses.
                //
                // No connected client is a legitimate quiet "not found" — the
                // same default the shared `device_set_state_json` returns for
                // a `None` handle (tui/linux never refuse this either) —
                // rather than a convention-11 refusal.
                //
                // Called straight on the connected `FfiNestClient` from here,
                // never through an `ApiClient` wrapper: `device_set_state_json`
                // is a `test-helpers` UniFFI export, absent from the bindings
                // every shipping flavor compiles `src/main` against
                // (convention 15), so a `src/main` wrapper broke the release
                // and storeSafe builds. `nestRpc()` throws when not connected,
                // which is the same quiet "not found" as above.
                val deviceIdHex = cmd.optString("device_id_hex", "")
                machineMethodResult = apiClient
                    ?.let { runCatching { it.nestRpc() }.getOrNull() }
                    ?.deviceSetStateJson(deviceIdHex)
                    ?: JSONObject().put("found", false).toString()
            }
            "conversations_inject_inbound" -> {
                // The payload rides the command envelope top-level
                // (http_bridge.call_command) and goes to the shared parser WHOLE —
                // `ConversationsManager::inject_inbound_from_test_json`, the seam
                // tui/linux call in process and web/apple/windows hand the same
                // JSON. `recipients`, attachments, labels, `is_own` and
                // `force_subject_change` are the parser's to read, so a key added
                // to the payload reaches every app at once (test-helpers feature).
                val host = conversationsManagerHost
                    ?: return "conversations_inject_inbound arrived before the " +
                        "ConversationsManagerHost was wired (TestAgent.start has not run yet)"
                return injectInbound(host.manager, cmd)
            }
            "conversations_evict_attachment" -> {
                // `{thread_id, filename}` → the shared
                // `evict_thread_attachments_for_test`, which drops the bytes the
                // way the store's budget eviction does and redraws. Mirrors linux
                // `handle_conversations_evict_attachment` / tui's arm.
                val host = conversationsManagerHost
                    ?: return "conversations_evict_attachment arrived before the " +
                        "ConversationsManagerHost was wired (TestAgent.start has not run yet)"
                return evictAttachment(host.manager, cmd)
            }
            "compose_text_runs" -> {
                // The styling the compose field APPLIED — what
                // get_attr(dm-text-field, "text-runs") answers on android
                // (drivers/android.py routes that one attribute here, as windows's
                // driver does): the AnnotatedString its VisualTransformation last
                // handed the field, in linux's JSON shape. No field on screen is a
                // refusal, never an empty read.
                val runs = composeTextRunsJson(ComposeFieldStyling.applied)
                machineMethodResult = runs
                if (runs == null) return "compose_text_runs: no conversations compose field is on screen"
            }
            "feed_inject_posts" -> {
                // Mirrors linux `handle_feed_inject_posts` (main.rs) / windows
                // `FeedCommands.InjectPosts`. Forwards the raw `posts` JSON array
                // verbatim to `FfiFeedManager.injectPostsForTest` — shared Rust
                // deserializes it into `Vec<TestPostSpec>` (the SAME payload
                // every app's `feed_inject_posts` handler parses), so no spec
                // shape lives in Kotlin. The only way a tier_2 test reaches the
                // unverified-source-badge `Failed` arm (a real nest serves only
                // Unchecked/Verified).
                val host = feedManagerHost
                    ?: return "feed_inject_posts arrived before the " +
                        "FeedManagerHost was wired (TestAgent.start has not run yet)"
                val posts = cmd.optJSONArray("posts") ?: JSONArray()
                val manager = host.manager()
                if (manager == null) {
                    // No-op before auth, matching linux: the feed manager is
                    // built on the authed connection, so it does not exist yet
                    // pre-login. `seed_posts` (actions/feed.py) always injects
                    // post-login and re-injects until the count holds, so this
                    // is a defensive guard, not the expected path.
                    android.util.Log.d(
                        "TestAgent",
                        "feed_inject_posts: no feed manager yet (pre-auth)",
                    )
                } else {
                    try {
                        manager.injectPostsForTest(posts.toString())
                        android.util.Log.i(
                            "TestAgent",
                            "feed_inject_posts ok (${posts.length()} post(s))",
                        )
                    } catch (e: Exception) {
                        android.util.Log.e("TestAgent", "feed_inject_posts failed: ${e.message}")
                        return "feed_inject_posts threw ${e::class.simpleName}: ${e.message}"
                    }
                }
            }
            "feed_inject_error" -> {
                // Stamp `FeedSnapshot.error` — the state a failed background
                // fetch leaves — so FeedScreen paints it on `error-message` (no
                // product path fails a feed fetch on demand). `{key?, message?}`,
                // the one `LocalizedText::key_arg` carrier a real
                // `feed.error_load` uses. Twin of tui's, web's and linux's
                // `feed_inject_error`.
                val manager = feedManagerFor(action).getOrElse { return it.message }
                val (key, message) = feedInjectErrorArgs(cmd)
                manager.injectErrorForTest(key, message)
            }
            "feed_hold_next_reload", "feed_release_held_reload" -> {
                // Arm (or release) the feed manager's one-shot reload hold: the
                // NEXT reload publishes the list it kept or cleared, then parks
                // before its fetch until the release, so a test can read the
                // page while a refresh is in flight (`feed.md` § The read
                // model). The feed's gestures launch their reload on the VM's
                // scope and never block this agent on it, so nothing here has
                // to start-rather-than-await while a hold is armed. Twins of
                // tui's, web's and linux's arms of the same names.
                val manager = feedManagerFor(action).getOrElse { return it.message }
                if (action == "feed_hold_next_reload") {
                    manager.holdNextReloadForTest()
                } else {
                    manager.releaseHeldReloadForTest()
                }
            }
            "conversations_inject_send_failure" -> {
                // Stamp thread_id's compose into send_state = Failed { reason }
                // and select it (the observable state a backend send error
                // leaves), so the conversations page renders the page-level
                // `error-message` surface. Drives the shared
                // ConversationsManager::inject_send_failure_for_test
                // (test-helpers feature). Mirror of linux
                // handle_conversations_inject_send_failure (main.rs). Params ride
                // the command envelope top-level (http_bridge.call_command).
                val threadId = cmd.optString("thread_id")
                val reason = cmd.optString("reason")
                if (threadId.isEmpty()) {
                    return "conversations_inject_send_failure carried no `thread_id`"
                }
                val host = conversationsManagerHost
                    ?: return "conversations_inject_send_failure arrived before the " +
                        "ConversationsManagerHost was wired (TestAgent.start has not run yet)"
                try {
                    host.manager.injectSendFailureForTest(threadId, reason)
                    android.util.Log.i("TestAgent", "conversations_inject_send_failure($threadId) ok")
                } catch (e: Exception) {
                    // A refusal reason, not a log line (see the inject_inbound catch).
                    android.util.Log.e("TestAgent", "conversations_inject_send_failure failed: ${e.message}")
                    return "conversations_inject_send_failure threw ${e::class.simpleName}: ${e.message}"
                }
            }
            "conversations_inject_page_error" -> {
                // Stamp ConversationsSnapshot.error — the observable state a
                // failed membership/label wire op leaves (confirm_add_participant
                // / remove_participant / rename_thread) — so the page's
                // error-message surface can be asserted. The membership twin of
                // conversations_inject_send_failure, for the same reason: no
                // product path fails one of those ops on demand. Drives the
                // shared ConversationsManager::inject_page_error_for_test
                // (test-helpers feature). Mirrors linux
                // handle_conversations_inject_page_error / windows
                // ConversationsCommands.InjectPageError; payload {key, message}.
                val message = cmd.optString("message")
                if (message.isEmpty()) {
                    return "conversations_inject_page_error carried no `message`"
                }
                val key = cmd.optString("key").ifEmpty {
                    "conversations.unified.error_add_participant"
                }
                val host = conversationsManagerHost
                    ?: return "conversations_inject_page_error arrived before the " +
                        "ConversationsManagerHost was wired (TestAgent.start has not run yet)"
                try {
                    host.manager.injectPageErrorForTest(
                        uniffi.fauna_core.LocalizedText(key, mapOf("message" to message)),
                    )
                    android.util.Log.i("TestAgent", "conversations_inject_page_error ok")
                } catch (e: Exception) {
                    android.util.Log.e("TestAgent", "conversations_inject_page_error failed: ${e.message}")
                    return "conversations_inject_page_error threw ${e::class.simpleName}: ${e.message}"
                }
            }
            "conv_receive_now" -> {
                // The receive loop's run-one-cycle-now poke (convention 14's
                // `run_now`, e2e-conventions.md), android's twin of tui/linux/web's
                // arm (`fauna_e2e_agent::CONV_RECEIVE_NOW`). Signals the shared
                // loop, which runs the identical full-sweep its 30 s backstop
                // ticker runs — the real delivery path, never a per-rail shortcut.
                // Fire-and-forget: the barrier is `conv_receive_cycles` (in
                // `serializeState`), not this ack. No session yet (pre-auth) is a
                // legitimate quiet no-op — the consumer's own deadline poll is what
                // fails, naming the app (convention 11: honour or refuse, never
                // drop silently — this honours it as a no-op, it does not drop it).
                val session = conversationsManagerHost?.session
                if (session != null) {
                    session.convReceiveNow()
                } else {
                    android.util.Log.i("TestAgent", "conv_receive_now: no conversations session yet (pre-auth)")
                }
            }
            "account_pump_now" -> {
                // The account plane's run-one-pass-now poke (convention 14's
                // `run_now`), android's twin of tui's and linux's arm
                // (`fauna_e2e_agent::ACCOUNT_PUMP_NOW`). `reconcile_now` under the
                // FFI is the ticker's OWN work on demand, never a bypass, so a
                // test that pokes and then waits on `account_pump_cycles`
                // observes exactly the production pass.
                //
                // Fire-and-forget, matching tui/linux and the `conv_receive_now`
                // arm directly above: the barrier is the counters in
                // `serializeState`, not this ack. Awaiting it here would put a
                // whole pump pass — network included — on the agent's poll loop,
                // so one wedged pass would stall every later command and surface
                // as an unrelated timeout. The FFI method itself awaits; who
                // spawns is the caller's call, and here the caller is a dispatch
                // loop that must stay responsive.
                //
                // No runtime yet (pre-auth) is a legitimate quiet no-op, honoured
                // rather than dropped (convention 11) — `accountPumpNow` reports
                // `false` and the consumer's own deadline poll is what fails,
                // naming the app.
                pokeScope.launch {
                    if (!(apiClient?.accountPumpNow() ?: false)) {
                        android.util.Log.i("TestAgent", "account_pump_now: no account runtime yet (pre-auth)")
                    }
                }
            }
            "serve_enable_folder" -> {
                // Arrange a WebDAV-served, content-keyed folder for the logged-in
                // actor, optionally creating it first — the twin of apple's
                // `ServeEnableFolderTestCommand` and linux/tui's arm. The serve goes
                // through `ApiClient.serveSetFolder`, the same production face
                // `folder-webdav-toggle` drives (owner-only: no MLS group), so the
                // test seam arranges state without a path of its own. Answered
                // asynchronously into `webdav_serve_reply` (cleared first, so the
                // helper's poll never reads a previous request's reply); the
                // dispatch loop stays responsive while the serve's network round
                // trips run, as in the `account_pump_now` arm above.
                webdavServeReply = null
                val api = apiClient
                    ?: return "serve_enable_folder arrived before the ApiClient was wired"
                val folder = cmd.optString("folder", "")
                val create = cmd.optBoolean("create", true)
                pokeScope.launch {
                    webdavServeReply = try {
                        if (create) api.nestRpc().folders().create(folder, null)
                        val served = api.serveSetFolder(folder, null, true)
                        JSONObject().put("ok", true).put("served_sets", served.toLong())
                    } catch (e: Exception) {
                        JSONObject().put("ok", false).put("error", "${e::class.simpleName}: ${e.message}")
                    }
                }
            }
            // The cross-app conversations command table (convention 11): every arm
            // below drives the SAME shared `ConversationsManager` methods linux's
            // `conversations/conv_backend.rs` `e2e_*` helpers do — they are all
            // UniFFI-exported, so android owes no `libs/fauna-ffi` work here
            // (priority #2: the logic already lives in shared Rust).
            "conversations_real_resolve_send_new",
            "conversations_real_send",
            "conversations_real_send_attachment",
            "conversations_real_add",
            "conversations_real_remove",
            "conversations_real_rename",
            "conversations_create_mls_group",
            "conversations_accept_recipient" -> {
                val host = conversationsManagerHost
                    ?: return "$action arrived before the ConversationsManagerHost " +
                        "was wired (TestAgent.start has not run yet)"
                // `host.manager` is the SESSION manager once the real conversations
                // session is up (the `FAUNA_E2E_REAL_CONVERSATIONS` launch gate), and
                // the bare mock-backed one otherwise — the same getter the screens
                // read, so these commands drive exactly what the UI drives.
                return dispatchConversationsManagerCommand(action, cmd, host.manager)
            }
            "sync_inject_locations" -> {
                // DELIBERATE, DOCUMENTED REFUSAL (convention 11's second half).
                //
                // This is a `declared_absence`, not unbuilt work: on
                // linux/windows/macOS the command swaps the *live* bound-folder list
                // rendered by the sync-folders page, and `folder-location-*` is
                // **desktop-only `platform_elements` in ui.yaml** — android has no
                // user-bindable local folder tree at all (the mobile sync model;
                // `FoldersScreen.kt`'s header and `ui-actual-android.yaml`'s
                // `folders` block both state it). Its folder content is
                // machine-snapshot state plus read-through via
                // `FaunaDocumentsProvider`; `PhotoBackupEngine`/
                // `WatchedDirectoryManager` are upload-only and expose no list to
                // inject into. Its only caller today,
                // `test_folders.py::test_bind_location_nested_under_folder_windows`,
                // is `@pytest.mark.windows`, so nothing collecting on `--app android`
                // reaches it.
                //
                // Refused by NAME rather than left to the catch-all so a future
                // session can tell "declared absent" from "nobody has built it yet"
                // — the distinction the catch-all erases.
                return "sync_inject_locations is deliberately not implemented on android: " +
                    "it injects a live bound-folder list, and `folder-location-*` is " +
                    "desktop-only platform_elements in ui.yaml — android has no " +
                    "user-bindable local folder tree (mobile sync model), so there is " +
                    "no list to inject into"
            }
            "screen_time_heartbeat" -> {
                // testing.md convention 14's fake clock + `run_now` poke for the
                // screen-time usage heartbeat (family-safety.md § Screen time,
                // Slice E) — the android twin of linux `screen_time_heartbeat`
                // (main.rs) / web `$lib/screen-time-e2e.ts`. Advances the ward
                // client's clock by `minutes` of foreground use and runs one
                // production heartbeat step, so a tier_3 journey can prove the
                // budget half WITHOUT waiting on wall-clock time — a test that
                // slept for a real heartbeat would be DEFUNCT under § point 14,
                // not merely slow. The cadence and accrual rules themselves are
                // pure and already proven at tier_1
                // (`fauna_core::screen_time::tests`); this exercises the WIRING —
                // that the client really calls `fauna.family.usage_report` and
                // feeds the reply back into the lock.
                val minutes = cmd.optInt("minutes", 0)
                val store = screenTimeStore
                    ?: return "screen_time_heartbeat arrived before the " +
                        "ScreenTimeStore was wired (TestAgent.start has not run yet)"
                store.advanceTestClockAndTick(minutes)
            }
            "atproto_delegation_advance_clock" -> {
                // The D10 lapse journey's fake RENDER clock (convention 14) —
                // android's twin of linux `main.rs` / tui `automation.rs` /
                // apple `DelegationClockTestCommand.swift` / web
                // `$lib/atproto-delegation-e2e.ts`. All five drive the same
                // shared seam, `fauna_atproto_settings_machine::delegation_clock`.
                //
                // Needed because `expiring_soon`/`expired` sit ~76 and ~90 days
                // into the grant window: unreachable by waiting, and a `sleep`
                // for them is the defunct-test pattern, not merely a slow one.
                //
                // ⚠ NEVER the mint clock — `authorizeExternalApps` always stamps
                // a fresh cert with the real wall clock, so an offset left
                // behind lapses the very next delegation this process mints.
                // `0` resets; nothing auto-resets it.
                //
                // The offset alone repaints nothing (the page renders off the
                // machine's snapshot), so this pokes the same refresh the page's
                // own gestures use — the production repaint path, with only the
                // clock moved.
                val offset = cmd.optLong("now_offset_secs", 0L)
                setDelegationClockOffsetSecs(offset)
                val host = atprotoSettingsHost
                    ?: return "atproto_delegation_advance_clock arrived before the " +
                        "AT Protocol settings page was opened, so there is no delegation " +
                        "row to re-render. Navigate to Settings -> AT Protocol first."
                host.refreshSnapshot()
            }
            "trust_facet_advance_clock" -> {
                // The Nests trust facet's RENDER clock (convention 14) —
                // android's twin of tui `automation.rs` / linux `main.rs` / web
                // `$lib/trust-clock-e2e.ts`, all over the shared seam
                // `fauna_client_capabilities::trust_clock`: grant liveness, the
                // auto-renew due decision and custody receipt freshness, never
                // the mint clock. `0` resets; nothing auto-resets it. Nothing to
                // repaint here: the Nests page folds against the clock on its
                // hydrate, and the driver's wait re-navigates on every poll.
                setTrustClockOffsetSecs(cmd.optLong("now_offset_secs", 0L))
            }
            "focus_move" -> {
                // Convention 17 layer (c) — `e2e-systematic-ui-walks.md` § The
                // convention. Mirrors `fauna_e2e_agent::{FOCUS_MOVE,
                // focus_move_request}`: android cannot link that Rust crate
                // (same reason apple's FocusWalkTestCommand.swift re-spells
                // it), so the payload vocabulary is re-derived here against
                // its one documented home rather than re-invented — an app
                // that re-derives the parse is free to disagree about what
                // `{"times": "3"}` means, and the walk it feeds then measures
                // that disagreement instead of the app.
                //
                // ⚠ A present-but-malformed field is a refusal; only an
                // ABSENT `times` defaults (to 1) — never a silent fallback.
                val directionRaw = if (cmd.has("direction")) cmd.opt("direction") else null
                val direction = when (directionRaw) {
                    "next" -> androidx.compose.ui.focus.FocusDirection.Next
                    "prev" -> androidx.compose.ui.focus.FocusDirection.Previous
                    else -> return "focus_move: `direction` must be \"next\" or \"prev\", got " +
                        describeCommandField(directionRaw)
                }
                val times: Int
                if (cmd.has("times") && !cmd.isNull("times")) {
                    val raw = cmd.opt("times")
                    val n = (raw as? Number)?.toInt()
                    if (n == null || n < 0) {
                        return "focus_move: `times` must be a non-negative integer, got " +
                            describeCommandField(raw)
                    }
                    times = n
                } else {
                    times = 1
                }
                if (times > FOCUS_MOVE_MAX_TIMES) {
                    return "focus_move: `times` is $times, above the $FOCUS_MOVE_MAX_TIMES " +
                        "cap — the step loop runs on the thread that serves this agent, " +
                        "so a count that large stalls every later command rather than " +
                        "just this one"
                }
                val manager = focusManager
                    ?: return "focus_move: no window to move focus in (FaunaNavHost has " +
                        "not composed yet)"
                // The SAME call Compose's own hardware Tab/Shift-Tab handling makes —
                // never a private seam that sets a focus index directly, exactly the
                // door linux's `child_focus` and apple's `selectNextKeyView` also use.
                repeat(times) { manager.moveFocus(direction) }
            }
            BarrierTestCommand.BARRIER_ACTION, BarrierTestCommand.PROBE_ACTION -> {
                // Convention 14's causal anchor and its self-test probe — the
                // mechanism, its traps and why this rail already orders the ack
                // live in [BarrierTestCommand].
                BarrierTestCommand.apply(action, cmd)?.let { return it }
            }
            E2eLoudSurfaces.ALERT_SWEEP_WAKE, E2eLoudSurfaces.RECONNECT_BACKOFF -> {
                // The loud surfaces' two command seams — the sweep loop's wake
                // and the reconnect pace — live in [E2eLoudSurfaces].
                E2eLoudSurfaces.apply(action, cmd, apiClient?.nestClient)?.let { return it }
            }
            "switch_pane" -> {
                // DELIBERATE, DOCUMENTED REFUSAL (convention 11's second half — same
                // style as the sync_inject_locations arm above).
                //
                // `fauna_e2e_agent::SWITCH_PANE`'s own contract: "An app whose layout
                // genuinely has no two-region split refuses by name and with a
                // reason." Every app that DOES honour this command (tui, linux,
                // macOS) has a nav region and a content region PERMANENTLY mounted
                // simultaneously; switch_pane only moves keyboard focus between
                // them. Android's nav rail is a `ModalNavigationDrawer`
                // (`FaunaNavHost.kt`): opening it COVERS the content region rather
                // than sitting beside it, and while closed its content is off the
                // focus order entirely — there is never a moment where both
                // regions are simultaneously live to switch keyboard focus
                // between. That is a genuinely different UI shape from every app
                // that implements this command, not merely an unbuilt leg (mirrors
                // iOS's declared absence for the same command, for the analogous
                // structural reason: apple-e2e-automation.md § Declared platform
                // absences).
                return "switch_pane: not honoured on android — declared platform " +
                    "absence: the nav drawer is a ModalNavigationDrawer, mutually " +
                    "exclusive with the content region rather than a permanent " +
                    "two-region split (e2e-systematic-ui-walks.md § Implementation " +
                    "status today)"
            }
            else -> {
                // A test agent must NEVER silently drop a command it can't honour — a
                // dropped command yields no error AND no effect, indistinguishable from
                // a real product bug (the exact trap that once cost multiple sessions
                // misdiagnosing MLS at-rest data loss that was actually a swallowed
                // command).
                //
                // ⚠ `TestAgentRefusalSurfaceTest` keys its command-table ratchet on the
                // phrase "no arm" to tell this catch-all apart from a named arm that
                // declines. Keep those two words if you reword this.
                android.util.Log.e("TestAgent", "unrecognized command: $action")
                return "android's test agent has no arm for this action"
            }
        }
        return null
    }

    /**
     * The `conversations_*` commands that drive the shared [ConversationsManager]
     * directly — android's twin of linux's `conversations/conv_backend.rs`
     * `e2e_*` helpers, method-for-method. Split out of [dispatchCommand] so the
     * shared "which manager, host wired?" preamble is written once.
     *
     * Returns `null` when honoured, else the refusal reason ([dispatchCommand]'s
     * contract). Every path either performs the manager call or names why it
     * could not — never a bare `return`.
     *
     * **Why these need no `libs/fauna-ffi` work:** all fifteen manager methods
     * used below are already `uniffi::export`ed on `ConversationsManager`
     * (`libs/fauna-conversations/src/manager.rs`) and are the same ones
     * `NewThreadComposeScreen`/`ConversationDetailScreen` call — the async ones
     * arrive in Kotlin as `suspend fun`, which is why this function suspends
     * rather than blocking (linux `block_on`s only because GTK gives it no
     * coroutine to suspend in).
     */
    private suspend fun dispatchConversationsManagerCommand(
        action: String,
        cmd: JSONObject,
        manager: ConversationsManager,
    ): String? {
        // Flat payload shape (`http_bridge.call_command` puts params on the
        // command envelope top-level), same as the inject commands above.
        fun str(key: String): String = cmd.optString(key, "")
        fun threadId(): String? = str("thread_id").ifEmpty { null }

        try {
            when (action) {
                "conversations_real_resolve_send_new" -> {
                    // The end-to-end proof that the picker resolves a REAL actor:
                    // the same resolve_recipient -> accept_current_recipient_chip
                    // path NewThreadComposeScreen's on-send handler drives, then
                    // send_new_thread bootstraps the group (fetch key package ->
                    // create MLS group -> deliver Welcome -> post Application).
                    val recipient = str("recipient")
                    manager.startNewConversation()
                    manager.setNewThreadRecipientInput(recipient)
                    manager.resolveRecipient()
                    if (!manager.acceptCurrentRecipientChip()) {
                        return "recipient '$recipient' did not resolve to a chip " +
                            "(not a reachable Fauna actor?)"
                    }
                    manager.setNewThreadBody(str("body"))
                    manager.sendNewThread()
                }
                "conversations_real_send" -> {
                    val tid = threadId()
                        ?: return "conversations_real_send carried no `thread_id`"
                    manager.setComposeBody(tid, str("body"))
                    manager.send(tid)
                }
                "conversations_real_send_attachment" -> {
                    // Drives the real seal + upload path (add_attachment -> send ->
                    // derive_blob_key(epoch_secret) seal -> blob_put).
                    val tid = threadId()
                        ?: return "conversations_real_send_attachment carried no `thread_id`"
                    val dataB64 = str("data_base64")
                    val bytes = try {
                        android.util.Base64.decode(dataB64, android.util.Base64.DEFAULT)
                    } catch (e: IllegalArgumentException) {
                        return "conversations_real_send_attachment's `data_base64` is not " +
                            "valid base64: ${e.message}"
                    }
                    manager.setComposeBody(tid, str("body"))
                    manager.addAttachment(tid, str("filename"), str("mime_type"), bytes)
                    manager.send(tid)
                }
                "conversations_real_add" -> {
                    // On a bound FaunaMls group this posts the MLS Commit + Welcome;
                    // on a 1:1 it forks a fresh group (snapshot-only until its first
                    // real_send).
                    val tid = threadId()
                        ?: return "conversations_real_add carried no `thread_id`"
                    val handle = str("peer_handle")
                    val actorId = parseActorHex(str("peer_actor_id_hex"))
                        ?: return "conversations_real_add's `peer_actor_id_hex` is not a " +
                            "64-char hex actor id: '${str("peer_actor_id_hex")}'"
                    manager.openAddParticipant(tid)
                    manager.setAddParticipantRecipientInput(handle)
                    manager.acceptAddParticipantChip(TypedAddress.Fauna(handle, actorId))
                    manager.confirmAddParticipant()
                }
                "conversations_real_remove" -> {
                    // `peer_handle` must match the one used at real_add: the snapshot
                    // removal keys on TypedAddress::display() (the handle), while the
                    // wire op finds the MLS leaf by actor_id.
                    val tid = threadId()
                        ?: return "conversations_real_remove carried no `thread_id`"
                    val handle = str("peer_handle")
                    val actorId = parseActorHex(str("peer_actor_id_hex"))
                        ?: return "conversations_real_remove's `peer_actor_id_hex` is not a " +
                            "64-char hex actor id: '${str("peer_actor_id_hex")}'"
                    manager.removeParticipant(tid, TypedAddress.Fauna(handle, actorId))
                }
                "conversations_real_rename" -> {
                    // Posts the encrypted GroupMeta::NameChanged Application envelope.
                    val tid = threadId()
                        ?: return "conversations_real_rename carried no `thread_id`"
                    manager.renameThread(tid, str("label"))
                }
                "conversations_create_mls_group" -> {
                    // Snapshot-level group fixture (test-helpers): participants are
                    // handles, actor ids left zero — mirrors linux
                    // `handle_conversations_create_mls_group`.
                    val participantsJson = cmd.optJSONArray("participants")
                        ?: return "conversations_create_mls_group carried no `participants` array"
                    val participants = (0 until participantsJson.length())
                        .mapNotNull { participantsJson.optString(it, "").ifEmpty { null } }
                        .map { TypedAddress.Fauna(it, ByteArray(32)) }
                    if (participants.isEmpty()) {
                        return "conversations_create_mls_group's `participants` array held no " +
                            "usable handles"
                    }
                    manager.createMlsGroup(participants)
                }
                "conversations_accept_recipient" -> {
                    // The manager decides which picker is active (add-participant
                    // overlay outranks new-thread compose) and whether its current
                    // text parses. Mirrors linux `handle_conversations_accept_recipient`
                    // / windows `AcceptVisibleRecipientPicker`.
                    if (!manager.acceptCurrentRecipientChip()) {
                        return "no recipient picker had text that parses into a chip " +
                            "(neither the add-participant overlay nor new-thread compose)"
                    }
                }
                // Defensive: unreachable, since [dispatchCommand] routes only the
                // eight actions above here. Kept so adding a name there without an
                // arm here refuses rather than falls through as honoured.
                else -> return "android's test agent has no arm for this action"
            }
        } catch (e: CancellationException) {
            // Cancellation is the agent shutting down, not a refusal — never
            // swallow it into the error surface (it would also break structured
            // concurrency in the poll loop).
            throw e
        } catch (e: Exception) {
            // A reason, not a logcat line: a swallowed throw here is exactly the
            // silent drop convention 11 exists to prevent — the test then fails
            // several steps later on a read that looks like a product bug.
            android.util.Log.e("TestAgent", "$action failed: ${e.message}")
            return "$action threw ${e::class.simpleName}: ${e.message}"
        }
        android.util.Log.i("TestAgent", "$action ok")
        return null
    }

    /**
     * Parse a 64-char hex `ActorId` into its 32 raw bytes, or `null` when the
     * text is not one. Mirrors linux `conv_backend::parse_actor_hex`.
     *
     * Returning `null` rather than a zero-filled fallback is load-bearing: the
     * MLS wire ops find a leaf **by actor id**, so a silently-zeroed id names the
     * all-zero actor and the command "succeeds" against the wrong (or no)
     * member — a silent drop wearing an effect (convention 11).
     */
    internal fun parseActorHex(hex: String): ByteArray? {
        if (hex.length != 64) return null
        val out = ByteArray(32)
        for (i in 0 until 32) {
            val hi = Character.digit(hex[i * 2], 16)
            val lo = Character.digit(hex[i * 2 + 1], 16)
            if (hi < 0 || lo < 0) return null
            out[i] = ((hi shl 4) or lo).toByte()
        }
        return out
    }

    private suspend fun applySessionPatch(session: JSONObject, appState: AppState, storage: SecureStorage) {
        // A patch that logs in IS an actor change in this process, so it owes the
        // same canonical drop the production switch owes — replacing the bare
        // `clearAuth()` the reconnect below used to do, which ran none of the
        // registered closers.
        //
        // Hoisted ABOVE the writes for two reasons: the drop clears the very
        // session-override fields this patch is about to seed (see
        // [registerSessionOverrideDrop]), so retire-then-seed is the only order
        // that survives; and the reconnect needs `nestClient` already nulled,
        // since `ensureNestConnected` is a no-op while it is non-null.
        //
        // The condition is the reconnect's own, evaluated against the values the
        // patch is about to install — deliberately NOT widened to "carries an
        // identity", so this fires on exactly the cases `clearAuth()` did.
        val patchLogsIn = session.has("authenticated") && session.getBoolean("authenticated")
        val incomingHex =
            if (session.has("secret_hex")) session.getString("secret_hex") else storage.secretHex
        val incomingUrl =
            if (session.has("node_url")) session.getString("node_url") else storage.nestUrl
        if (patchLogsIn && !incomingHex.isNullOrEmpty() && !incomingUrl.isNullOrEmpty()) {
            actorScope?.dropActorScopedState()
        }
        val patchUrl = if (session.has("node_url")) session.getString("node_url") else null
        val patchDeviceId = if (session.has("device_id")) session.getString("device_id") else null
        // The session patch is a registry write, exactly the shape a real
        // sign-in leaves behind (linux's session door): `addAccount` enrolls the
        // identity and writes whichever per-actor slots the patch carries (an
        // idempotent upsert that leaves an absent slot untouched) and
        // `setActive` moves the pointer, so the launch persistence, the session
        // account and the reconnect below all resolve the patched identity. A
        // patch carrying only a nest URL or a device id updates the active
        // account's slots the same way.
        if (!incomingHex.isNullOrEmpty() &&
            (session.has("secret_hex") || patchUrl != null || patchDeviceId != null)
        ) {
            registry?.let { reg ->
                try {
                    val actorId = reg.addAccount(incomingHex, patchUrl, patchDeviceId)
                    reg.setActive(actorId)
                } catch (e: Exception) {
                    android.util.Log.e(
                        "TestAgent",
                        "session patch registry write failed: ${e.message} — the rebuilt session reads the previous account",
                    )
                }
            }
        }
        if (session.has("secret_hex")) {
            appState.session.secretHex = session.getString("secret_hex")
        }
        if (patchUrl != null) {
            appState.session.nodeUrl = patchUrl
        }
        if (patchDeviceId != null) {
            appState.session.deviceId = patchDeviceId
        }
        if (session.has("handle")) {
            appState.session.handle = session.getString("handle")
        }
        if (session.has("actor_id")) {
            appState.session.actorId = session.getString("actor_id")
        }
        if (session.has("authenticated") && session.getBoolean("authenticated")) {
            appState.session.isAuthenticated = true
            appState.isOnboarding = false
            // The e2e login shortcut: mirror the real account-switch teardown +
            // rebuild (FaunaNavHost.kt's `onAccountSwitched` → the onboarding
            // LaunchedEffect's `connectActiveSession()`) rather than only flipping
            // Compose-observed state. Without this the LIVE `ApiClient` never
            // connects at all on a cold app (no android e2e login has ever
            // established a real WS-RPC session — `--client android` is
            // host-emulator-gated fleet-wide and has never run) and, on a
            // same-process actor switch, stays bound to the PREVIOUS actor while
            // `appState`/`storage` report the new one (the same "half-applied
            // patch" shape e2e convention 11 forbids, linux/web's `403 Forbidden`
            // regression — set-state-does-not-reauth-running-app memory — being
            // the sibling bug this mirrors on a different mechanism). The
            // identity drop at the top of this function is what makes the
            // rebuild take: `ensureNestConnected` is a no-op while `nestClient`
            // is still non-null, and the drop is what nulls it.
            val hex = storage.secretHex
            val url = storage.nestUrl
            if (!hex.isNullOrEmpty() && !url.isNullOrEmpty()) {
                apiClient?.let {
                    it.nodeUrl = url
                    try {
                        it.authenticate(hex)
                    } catch (e: Exception) {
                        android.util.Log.e("TestAgent", "session patch reconnect failed: ${e.message}")
                    }
                }
            }
        }
    }

    /** Returns `null` when the nav was applied, else the refusal reason
     *  ([dispatchCommand]'s contract). Each early exit here used to be a bare
     *  `return`: a `navigate_to` that quietly did nothing, after which the test
     *  asserted against whatever page it happened to still be on. */
    private fun applyNavPatch(nav: JSONObject, appState: AppState): String? {
        val stack = nav.optJSONArray("stack")
            ?: return "the `nav` patch carried no `stack` array"
        val first = stack.optJSONObject(0)
            ?: return "the `nav` patch's `stack` is empty"
        val view = first.optString("view").ifEmpty {
            return "the `nav` patch's first stack entry carried no `view`"
        }

        if (view.startsWith("onboarding/")) {
            appState.isOnboarding = true
            return null
        }

        appState.isOnboarding = false
        val nc = appState.navController
            ?: return "cannot navigate to \"$view\": no NavController is attached yet " +
                "(the app is still pre-auth or mid-launch)"
        // The state-protocol path onto another actor's profile (the equivalent
        // of a contacts-row tap-through — profile.md § Layout & flow → Another's
        // profile), mirroring linux's `test_agent::profile_nav_target`: an
        // `actor_id` naming the viewer must normalize back to the bare SELF
        // route, or the viewer's own profile renders in OTHER shape
        // (`profile-follow-button` where `profile-edit-button` belongs).
        val route = if (view == "profile") {
            val actorId = first.optString("actor_id").ifEmpty { null }
            val target = actorId?.let { profileNavTarget(it, appState.session.actorId) }
            target?.let { "profile/$it" } ?: view
        } else {
            // The sub-page rides the SECOND stack entry's `id`
            // (`[{"view":"settings"},{"view":"settings","id":"muted-words"}]`) —
            // reading only `stack[0]`, as this arm did until 2026-08-14, landed
            // every settings sub-page nav on the Settings hub and threw on
            // `{"view":"admin"}`, which android has no route for. Resolution is
            // a table, so it lives in [NavRouteResolver] where a unit test can
            // pin one row at a time.
            val subId = stack.optJSONObject(1)?.optString("id")?.ifEmpty { null }
            when (val r = NavRouteResolver.resolve(view, subId)) {
                is NavRouteResolver.Resolution.Refused -> return r.reason
                is NavRouteResolver.Resolution.Route -> r.route
            }
        }
        // The SAME edge the drawer crosses, not a second copy of its nav options
        // (`FaunaNavHost.kt::navigateToDrawerRoute`). Two copies is how linux's
        // canonical-entry bug hid: its agent reset the shell while its sidebar
        // did not, so every e2e assertion passed while the clicking user still
        // landed on the stale sub-page (ui/README.md § Navigation model,
        // qualification 4). One function means the state protocol cannot
        // disagree with the affordance it stands in for.
        //
        // A route the graph does not carry throws out of `navigate`, which
        // would bypass the refusal funnel entirely — convention 11 wants the
        // reason on `error-message`, not a stack trace in the bridge. The
        // resolver already refuses everything android knowingly cannot serve;
        // this catch is for the unknown view a future action adds.
        try {
            nc.navigateToDrawerRoute(route)
        } catch (e: IllegalArgumentException) {
            return "cannot navigate to \"$route\" (resolved from view=\"$view\"): " +
                "android's nav graph has no such destination — ${e.message}"
        }
        return null
    }

    /**
     * `compose.file`[target] — the one door every native app's file-attach
     * travels (`drivers/http_bridge.py::set_input_files`, `:996-1011`): a real
     * OS file-picker dialog cannot be driven by an in-process agent, so the
     * driver stages the already-picked file at `file` on disk and names which
     * composer via `target`. Mirrors `FaunaMacApp`/`FaunaApp` (iOS)'s
     * `applyComposePatch` dispatch — same four targets the shared driver sends,
     * same implement-or-refuse discipline (convention 11: a command MUST NOT
     * be silently dropped). `compose-file` (the feed composer) and
     * `attachment-button` (the conversations composers) are implemented below;
     * the profile pair names real android production doors
     * (`ProfileEditVM.stageAvatar`/`stageBanner`) that a TestAgent instance
     * cannot yet REACH — unlike [FeedManagerHost] and
     * [ConversationsManagerHost], `ProfileEditVM` has no singleton host, only a
     * plain per-navigation `@HiltViewModel` — so they refuse loudly rather than
     * silently no-op, which convention 11 treats identically to "not
     * implemented".
     */
    private fun applyComposePatch(compose: JSONObject): String? {
        val path = compose.optString("file").ifEmpty {
            return "the `compose` patch carried no `file`"
        }
        val target = compose.optString("target").ifEmpty { "compose-file" }
        return when (target) {
            "compose-file" -> attachFeedComposeFile(path)
            "attachment-button" -> attachConversationComposerFile(path)
            "profile-edit-avatar", "profile-edit-banner" ->
                "compose.file[$target]: not implemented on android yet — ProfileEditVM " +
                    "is a plain per-navigation @HiltViewModel with no singleton host a " +
                    "TestAgent instance can reach"
            else -> "compose.file: unknown target \"$target\""
        }
    }

    /**
     * `compose.file`[compose-file]: stage the picked file the same way the real
     * picker callback does (`FeedComposeScreen.kt`'s `filePickerLauncher`) — read
     * + EXIF-strip the bytes and hold them on [FeedManagerHost] for
     * `FeedVM.submitPost`'s seal-then-upload (never uploaded here — `ui/media.md`
     * § Encryption at rest), and stage the metadata via `updateCompose`. Reaches
     * the manager through the singleton host directly rather than through a
     * `FeedVM` instance: TestAgent holds no reference to whichever `FeedVM` the
     * compose screen currently shows, but a later REAL `post-submit-button`
     * click on that live instance reads [FeedManagerHost.pendingAttachmentBytes]
     * the same way, so the two paths agree regardless of which one staged it.
     */
    private fun attachFeedComposeFile(path: String): String? {
        val host = feedManagerHost
            ?: return "compose.file[compose-file] arrived before the FeedManagerHost " +
                "was wired (TestAgent.start has not run yet)"
        val manager = host.manager()
            ?: return "compose.file[compose-file]: no feed manager yet (pre-auth)"
        val file = java.io.File(path)
        val rawBytes = try {
            file.readBytes()
        } catch (e: Exception) {
            return "compose.file[compose-file]: could not read $path: ${e.message}"
        }
        val mimeType = java.net.URLConnection.guessContentTypeFromName(path)
            ?: "application/octet-stream"
        val bytes = ExifStripper.strip(rawBytes, mimeType)
        val current = manager.snapshot().compose
        host.stageAttachmentBytes(bytes)
        manager.updateCompose(
            current.text,
            current.tags,
            AttachedFile(
                name = file.name,
                size = rawBytes.size.toULong(),
                blobHash = null,
                mediaType = mimeType,
            ),
        )
        android.util.Log.i("TestAgent", "compose.file[compose-file]: staged $path")
        return null
    }

    /**
     * `compose.file`[attachment-button]: stand in for the conversations picker's
     * result callback — `ConversationDetailScreen`'s `attachmentPickerLauncher`
     * on an open thread, `NewThreadComposeScreen`'s on the new-thread composer —
     * and call exactly what it calls: EXIF-strip the bytes, then the shared
     * `addAttachment` / `addNewThreadAttachment`. Only the OS `GetContent()`
     * activity is bypassed; staging, send, seal and echo are the production path.
     *
     * Which composer is showing is read off the SHARED manager's snapshot, not
     * tracked by a second copy here: an active `newThreadCompose` first (the
     * new-thread screen is up), else `selectedThreadId` (what
     * `ConversationListScreen.onOpenThread` and search's thread hits select
     * before pushing `conversation/{threadId}`). That is the precedence linux's
     * agent (`main.rs`'s compose patch) and apple's
     * `ConversationsVM.attachComposerFile` use, over the same snapshot fields.
     *
     * The MIME comes from the shared `content_type_for_filename` catalog linux
     * (`guess_mime_type`) and apple (`mimeType(forPath:)`) resolve through, so
     * all three agents stage the same file under the same type; the real picker
     * uses `contentResolver.getType`, which a path on disk has no answer for.
     */
    private fun attachConversationComposerFile(path: String): String? {
        val host = conversationsManagerHost
            ?: return "compose.file[attachment-button] arrived before the " +
                "ConversationsManagerHost was wired (TestAgent.start has not run yet)"
        val file = java.io.File(path)
        val rawBytes = try {
            file.readBytes()
        } catch (e: Exception) {
            return "compose.file[attachment-button]: could not read $path: ${e.message}"
        }
        val mimeType = contentTypeForFilename(file.name)
        val bytes = ExifStripper.strip(rawBytes, mimeType)
        val manager = host.manager
        val snapshot = manager.snapshot()
        val selected = snapshot.selectedThreadId
        when {
            snapshot.newThreadCompose != null ->
                manager.addNewThreadAttachment(file.name, mimeType, bytes)
                    ?: return "compose.file[attachment-button]: the new-thread composer " +
                        "closed before the file could be staged on it"
            selected != null -> manager.addAttachment(selected, file.name, mimeType, bytes)
            else -> return "compose.file[attachment-button]: no conversation composer is " +
                "open — neither a new-thread compose nor a selected thread"
        }
        android.util.Log.i("TestAgent", "compose.file[attachment-button]: staged $path")
        return null
    }

    /**
     * `type_text`[target] — the typed text for a widget android's UiAutomator
     * bridge cannot type into (`drivers/android.py::AndroidBridgeDriver.type_text`
     * routes exactly those targets here, the way `set_input_files` routes a file
     * pick through `compose.file`). Same implement-or-refuse discipline as
     * [applyComposePatch] (convention 11).
     */
    private fun applyTypeTextPatch(typed: JSONObject): String? {
        val target = typed.optString("target").ifEmpty {
            return "the `type_text` patch carried no `target`"
        }
        val text = typed.optString("text")
        return when (target) {
            "dm-reaction-more-button" -> pickMoreReaction(text)
            else -> "type_text: no agent arm for \"$target\" — android types into a " +
                "field through the UiAutomator bridge, not the agent"
        }
    }

    /**
     * `type_text`[dm-reaction-more-button]: the fuller reaction picker is the
     * native emoji2 `EmojiPickerView` (`ui/conversations.md` § Reactions & message
     * delete → *Rendering / picker glue*), whose cells carry no test id — so the
     * typed emoji is picked through the picker's OWN pick handler
     * ([moreReactionPick], the `onPicked` its listener calls), which toggles the
     * reaction through the screen's one toggle path and dismisses the sheet, as a
     * human pick does. Linux's `GtkEmojiChooser` arm is the same shape. The sheet
     * must already be open: the click that opens it stays part of the journey.
     */
    private fun pickMoreReaction(text: String): String? {
        if (text.isEmpty()) return "type_text[dm-reaction-more-button]: no emoji to pick"
        val pick = moreReactionPick
            ?: return "type_text[dm-reaction-more-button]: the emoji picker is not open " +
                "— click its button first"
        pick(text)
        android.util.Log.i("TestAgent", "type_text[dm-reaction-more-button]: picked $text")
        return null
    }

    /** Mirrors linux's `test_agent::profile_nav_target`: resolve the
     *  state-protocol nav-stack `actor_id` against the viewer's own actor id.
     *  Comparison is trim + case-insensitive (the entry carries whatever the
     *  test seeded; `appState.session.actorId` is whatever the session patch
     *  wrote). A blank `actor_id`, or an unknown viewer identity, resolves to
     *  SELF (`null`) rather than to a profile for the empty actor — the whole
     *  point being that `ProfileVM.isSelf = targetActorId == null` must never
     *  render the viewer's own profile in OTHER shape. */
    internal fun profileNavTarget(entryActorId: String, selfActorId: String?): String? {
        val target = entryActorId.trim()
        if (target.isEmpty()) return null
        val me = selfActorId?.trim()
        val isSelf = !me.isNullOrEmpty() && me.equals(target, ignoreCase = true)
        return if (isSelf) null else target
    }
}
