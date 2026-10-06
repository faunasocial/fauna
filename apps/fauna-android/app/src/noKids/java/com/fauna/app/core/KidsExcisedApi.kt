package com.fauna.app.core

import com.fauna.ffi.FfiNestClient
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * The [ApiClient] members the **kids** build type excises — **the built half**
 * (`family-safety.md` § The account age band, the kids-app bullet, item (4);
 * `dynamic-features.md` § Compile-time excision). [ApiClient] extends this
 * class; it is compiled into `debug`, `release`, `storeSafe` and `foss` from
 * `src/noKids/`, and the `kids` build type compiles the inert twin under
 * `src/kids/java/com/fauna/app/core/KidsExcisedApi.kt` in its place.
 *
 * Every member here names a declaration the kids `fauna-ffi` flavor
 * (`--no-default-features --features kids-safe,kids-floor`) does not export —
 * the feed, search, every bridge (mail, DNS, atproto, nostr), web publishing,
 * the subscriptions author calls, connected apps and the labeler catalog.
 * Kotlin has no inline compile-time exclusion, and a member of the shared
 * [ApiClient] cannot be removed per build type, so the members live in a
 * superclass that IS per build type (the `src/payments` / `src/noPayments`
 * posture, one shape over: a superclass rather than extension functions so
 * the unit tests keep stubbing them as ordinary [ApiClient] members). Call
 * sites are unchanged — `api.buildMailSettingsMachine()` reads the same.
 *
 * The twin declares only what `src/main` still names; everything else is
 * called solely from `src/noKids/`, which the kids build never compiles.
 */
abstract class KidsExcisedApi {
    // The concrete client whose connection these members ride. `ApiClient` is
    // the only subclass; its handles are `internal` for this file alone.
    private val api: ApiClient get() = this as ApiClient

    // The shared stateful Feed-page manager (libs/fauna-feed FeedManager façade).
    // A connection-bound singleton (the Feed page spans list/compose/detail routes
    // that share one snapshot), built lazily by [feedManager] and torn down by [detachKidsExcised].
    private var feedManager: com.fauna.ffi.FfiFeedManager? = null

    // The shared stateful Search-page manager (libs/fauna-client-search's
    // SearchManager façade over UniFFI, docs/goal/ui/search.md § State & data
    // shape). Same shape as [feedManager] and for the same reason:
    // `client.searchManager()` mints a brand-new manager (and therefore a
    // brand-new snapshot) on EVERY call, so this cache is what makes it a
    // per-session singleton rather than a fresh empty page on each access.
    // Built lazily by [searchManager], torn down by [detachKidsExcised].
    private var searchManager: com.fauna.ffi.FfiSearchManager? = null

    // The `atproto` page's AtprotoSettingsMachine. A connection-bound singleton,
    // same shape as [feedManager] and for the same reason [AtprotoSettingsHost]
    // documents: the S4-C custody check's one-convergence debounce
    // (`custody_suspect` in fauna-atproto-settings-machine's machine.rs) lives
    // INSIDE the machine instance, so a fresh machine per `settings/atproto`
    // visit — the naive per-`hiltViewModel()` pattern AtprotoVM used before —
    // resets it every time and the custody alarm can never confirm. Built lazily
    // by [buildAtprotoSettingsMachine] with ONE observer (AtprotoSettingsHost's),
    // torn down by [detachKidsExcised].
    private var atprotoSettingsMachine: uniffi.fauna_atproto_settings_machine.AtprotoSettingsMachine? =
        null

    /**
     * Detach the singletons above for [ApiClient.clearAuth]: drop their
     * observers and the references now, and return the close of their FFI
     * handles for the off-main teardown that follows `client.disconnect()`.
     */
    internal fun detachKidsExcised(): () -> Unit {
        val feedMgr = feedManager
        feedMgr?.clearObservers()
        feedManager = null
        val searchMgr = searchManager
        searchMgr?.clearObservers()
        searchManager = null
        atprotoSettingsMachine = null
        return {
            feedMgr?.close()
            searchMgr?.close()
        }
    }

    /**
     * The feed manager's `{"started": N, "completed": M}` reload counters as a
     * JSON string, or `null` if no manager has been built yet — a NON-BUILDING
     * peek at the private [feedManager] cache, unlike [feedManager] itself
     * (which constructs one as a side effect) or [FeedManagerHost]'s own
     * accessor (which additionally kicks off draft restore). Reached through
     * this accessor for the same reason every other TestAgent read is — the
     * FFI handle stays owned by this class.
     */
    fun feedReloadsJson(): String? = feedManager?.feedReloadsJson()

    /**
     * The `data.feed.posts` state-dump array as a JSON string, or `null` if no
     * manager has been built yet — a NON-BUILDING peek at the private
     * [feedManager] cache, unlike [feedManager] itself (which constructs one as
     * a side effect) or [FeedManagerHost]'s own accessor (which additionally
     * kicks off draft restore). `null` maps to the empty `posts` array on the
     * TestAgent side, the legitimate pre-auth zero. Reached through this
     * accessor for the same reason every other TestAgent read is — the FFI
     * handle stays owned by this class.
     */
    fun feedPostsJson(): String? = feedManager?.postsJson()

    /**
     * Author-side encrypted-mode auto-approve reconcile pump — the android twin
     * of linux `subscriptions_author.rs::start` + windows `SubscriptionsAuthorPump`
     * (monetization.md § The unifying model, grant path 2; § Pillar 1). On connect,
     * and then on a poll backstop, it runs — in order:
     *
     *   1. resume — re-drives any subscriber-removal whose crypto rotation +
     *      upload was interrupted by a crash (idempotent no-op when nothing is
     *      staged); heals the latent gap that this call was previously unwired
     *      on android.
     *   2. drain — auto-approves every queued `auto_approve` **subscribe**
     *      request, minting the covering KeyBlob. This is what makes an
     *      encrypted-mode **follow** frictionless: the nest cannot mint the
     *      KeyBlob, so a follow *enqueues* (`Queued`) even for the
     *      `auto_approve` rank-0 `followers` tier, and the author's own client
     *      grants it here — with no manual approve.
     *
     * **This shell owns only the scheduler** (`monetization.md` § Pillar 1 →
     * *Where the logic lives*: "Each app owns only the scheduler … An app MUST
     * NOT re-derive either"). Both halves above, *and the order between them*,
     * are one shared call — `subscriptionsReconcileOnce` — because the order is
     * load-bearing: a staged removal must be driven out before the drain mints
     * over the roster, or the fresh KeyBlob re-covers the subscriber being
     * removed. The cadence is likewise shared, via `subscriptionsAuthorPollSecs`
     * (which honours the `FAUNA_SUBS_POLL_SECS` e2e override that android's
     * former hard-coded 30 s literal could not). Same two-line loop as tui and
     * linux (`apps/fauna-tui/src/subscriptions_author.rs`).
     *
     * There is no subscribe-request push kind, so the poll is the delivery floor:
     * the connect-time first pass (before the first delay) grants a follow that
     * accumulated while the author was offline. Started from [ensureNestConnected]
     * — the single connected-client chokepoint — so it fires on **every**
     * authenticated session, including the E2E set_state login (the conversations
     * `startReceiveLoop` seam is E2E-skipped, mirroring why windows wires BOTH
     * login seams). Best-effort per call; a bad tick is reported in the returned
     * record, logged, never fatal.
     */
    internal fun startSubscriptionsAuthorPump(client: FfiNestClient, secretBytes: ByteArray) {
        api.subscriptionsAuthorJob = api.connectionScope.launch {
            // Shared policy, read once per pump exactly as tui/linux do.
            val pollMs = com.fauna.ffi.subscriptionsAuthorPollSecs().toLong() * 1_000L
            while (true) {
                try {
                    val pass = com.fauna.ffi.subscriptionsReconcileOnce(client, secretBytes)
                    // Each half is best-effort and independent, so each reports
                    // its own error in the record rather than failing the call.
                    pass.resumeError?.let {
                        ShellLog.w("SubscriptionsAuthor", "resume_pending_removals failed: $it")
                    }
                    if (pass.approved > 0u) {
                        ShellLog.i(
                            "SubscriptionsAuthor",
                            "auto-approved ${pass.approved} pending follow(s)",
                        )
                    }
                    pass.drainError?.let {
                        ShellLog.w("SubscriptionsAuthor", "drain_auto_approvals failed: $it")
                    }
                } catch (e: Exception) {
                    ShellLog.w("SubscriptionsAuthor", "reconcile_once failed: ${e.message}")
                }
                delay(pollMs)
            }
        }
    }

    /**
     * Start the app-scoped hands-off auto-renew cadence (tls-certificates.md
     * § C.3 C2). Builds its **own** `DnsManagementMachine` — independent of the
     * `admin-dns` page's persistent one — so a background `Refresh` never disturbs
     * the page's rendered red/green verdicts (the ACME account is shared via the
     * synced `fauna.state.dns`, so account-reuse holds across both). The loop fires
     * only **after** the first full interval, so a short-lived session (incl. the
     * e2e harness) never triggers an order. A non-admin connection no-ops
     * gracefully: the Admin-only `Refresh`/`RefreshCertStatus` error → empty
     * snapshot → `autoRenewScan()` empty → nothing issued. Mirror of linux
     * `start_ws_rpc` + `run_auto_renew_cadence_tick`.
     */
    internal fun startAutoRenewCadence() {
        api.autoRenewJob = api.connectionScope.launch {
            val machine = buildDnsManagementMachineWithCredentials() ?: return@launch
            while (true) {
                delay(uniffi.fauna_client_dns.autoRenewPollSecs().toLong() * 1000L)
                runAutoRenewCadenceTick(machine)
            }
        }
    }

    /**
     * One auto-renew cadence tick (tls-certificates.md § C.3 C2). The tick itself
     * — refresh-then-ask order, the skip-if-empty rule, the per-domain non-fatal
     * rule, and the trailing health re-read — lives in the shared
     * `DnsManagementMachine::{autoRenewScan,autoRenewIssue}`, so every native app
     * runs the identical sequence; this wrapper owns only what is genuinely
     * android's: the `target_nest_id` resolution ([resolveThisNestId] —
     * linked-nests state, D7, deliberately outside the DNS machine) and the log
     * sink.
     */
    private suspend fun runAutoRenewCadenceTick(machine: uniffi.fauna_client_dns.DnsManagementMachine) {
        try {
            val domains = machine.autoRenewScan()
            if (domains.isEmpty()) return
            val targetNestId = api.resolveThisNestId() ?: return
            val pass = machine.autoRenewIssue(domains, targetNestId)
            for (failure in pass.failed) {
                ShellLog.w("ApiClient", "auto-renew cadence: IssueCert ${failure.domain}: ${failure.error}")
            }
        } catch (e: Exception) {
            ShellLog.w("ApiClient", "auto-renew cadence tick: ${e.message}")
        }
    }

    /**
     * The Linked-nests machine with the mail-relay hook and the trust facet
     * wired in — [ApiClient.buildLinkedNestsMachine]'s preferred build. `null`
     * when the seams are unavailable (logged), and the caller falls back to
     * the plain machine, so list/link/unlink still work.
     */
    internal fun linkedNestsMachineWithMailRelay(
        client: FfiNestClient,
        secretBytes: ByteArray,
    ): uniffi.fauna_client_pair.LinkedNestsMachine? =
        try {
            com.fauna.ffi.buildLinkedNestsMachineWithMailRelayAndTrust(client, secretBytes)
        } catch (e: Exception) {
            ShellLog.w(
                "ApiClient",
                "nests: mail-relay/trust seams unavailable (${e.message}); building plain machine",
            )
            null
        }

    /**
     * Build the page-level Labeler-Catalog state machine over the live WS-RPC
     * connection (`fauna.labelers.{list,inspect,subscribe,unsubscribe}`) — backs
     * both the Community-labelers catalog page (all published labelers) and the
     * Personalization home's subscribed-labelers facet (the same `entries`,
     * client-filtered to `subscribed == true`). `observer` ticks on every
     * snapshot change. Built with the actor secret, so subscribing a `wasm` mail
     * labeler mints its per-labeler grant and unsubscribing revokes it (the
     * [buildMailSettingsMachine] shape). Returns null until connected to a nest
     * with a secret (mirrors [buildDevicesMachine] / [buildMediaMachine]).
     */
    fun buildLabelerCatalogMachine(
        observer: uniffi.fauna_labeler_catalog_machine.LabelerCatalogObserver,
    ): uniffi.fauna_labeler_catalog_machine.LabelerCatalogMachine? {
        val client = api.nestClient ?: return null
        val secretBytes = api.secret?.let { HexUtil.hexToBytes(it) } ?: return null
        return com.fauna.ffi.buildLabelerCatalogMachineWithGrants(client, secretBytes, observer)
    }

    /**
     * The shared, stateful Feed-page manager (`libs/fauna-feed` `FeedManager`
     * over the WS-RPC [FfiNestClient]) — the single observable surface the Feed
     * list / compose / post-detail screens render off (`docs/goal/ui/feed.md` §
     * State & data shape). Unlike the per-call `build*Machine` helpers, the
     * manager is a **connection-bound singleton** (the Feed page spans three
     * routes that must share one snapshot), owned here alongside the other FFI
     * clients and torn down in [clearAuth] so a re-login rebinds. Built lazily on
     * first access: it needs the actor secret (to build + sign posts on submit,
     * the same bytes [buildMailSettingsMachine] threads) and the live socket, and
     * registers [observer] once at build (a re-login rebuilds and re-registers
     * the same singleton observer). Returns null when not yet connected — the
     * caller renders an empty page and retries on the next gesture. The shared
     * `feed_manager` factory rejects a non-32-byte secret, caught here so a
     * malformed key degrades to an empty page rather than crashing the screen.
     */
    fun feedManager(
        observer: uniffi.fauna_feed.FeedSnapshotObserver,
    ): com.fauna.ffi.FfiFeedManager? {
        feedManager?.let { return it }
        val client = api.nestClient ?: return null
        val secretBytes = api.secret?.let { HexUtil.hexToBytes(it) } ?: return null
        val mgr = runCatching { client.feedManager(secretBytes) }
            .onFailure { ShellLog.w("ApiClient", "feedManager build failed: ${it.message}") }
            .getOrNull() ?: return null
        mgr.addObserver(observer)
        feedManager = mgr
        return mgr
    }

    /**
     * The shared stateful Search-page manager (docs/goal/ui/search.md § State &
     * data shape). Connection-bound singleton like [feedManager]: built lazily
     * on first access and registers [observer] once at build (a cache hit
     * ignores it). Unlike [feedManager] it needs no actor secret — searching
     * signs nothing. Returns null when not yet connected — the caller renders
     * an empty page and retries on the next gesture.
     */
    fun searchManager(
        observer: uniffi.fauna_client_search.SearchSnapshotObserver,
    ): com.fauna.ffi.FfiSearchManager? {
        searchManager?.let { return it }
        val client = api.nestClient ?: return null
        val mgr = client.searchManager()
        mgr.addObserver(observer)
        searchManager = mgr
        return mgr
    }

    /**
     * Register [manager]'s local sealed-index arm (backend 2) over this
     * connection's conversations session, or `false` when there is no session
     * yet or this actor has no mail — a **normal state, not an error**
     * (search.md § Implementation status today). The FFI twin of the two lines
     * tui runs at its post-auth hook; mirrors apple's
     * `APIClient.attachLocalSearchIndex(manager:)`. A `false` return is safe to
     * retry later (the manager mints the arm on a later query once its
     * resolver is registered — that registration happens synchronously inside
     * this call regardless of the eventual return value).
     */
    suspend fun attachLocalSearchIndex(manager: com.fauna.ffi.FfiSearchManager): Boolean {
        val client = api.nestClient ?: return false
        return runCatching { client.attachLocalSearchIndex(manager) }
            .onFailure { ShellLog.w("ApiClient", "attachLocalSearchIndex failed: ${it.message}") }
            .getOrDefault(false)
    }

    // ── Mail-settings family (libs/fauna-client-mail-settings, over UniFFI) ──
    //
    // The mail-settings hub + its 5 sub-pages (aliases / lists / list-members /
    // export / spam) each render a shared state machine; the Compose VMs hold no
    // mail logic (priority #2). All builders bind to the current WS-RPC connection
    // and return null until the socket is up (the page hydrates with retry, like
    // [buildLinkedNestsMachine]). The Linux lead is apps/fauna-linux/src/settings/
    // mail{,_aliases,_lists,_list_members,_export,_spam}.rs.

    /** Mail-settings hub (`fauna.bridges.*` credential/MSEK provisioning). Needs
     *  the actor secret + node URL to wrap/unwrap MSEK blobs client-side. */
    fun buildMailSettingsMachine(): uniffi.fauna_client_mail_settings.MailSettingsMachine? {
        val client = api.nestClient ?: return null
        val secretBytes = api.secret?.let { HexUtil.hexToBytes(it) } ?: return null
        return com.fauna.ffi.buildMailSettingsMachine(client, secretBytes, api.nodeUrl)
    }

    /** The `connected-apps` page's `ConnectedAppsMachine`
     *  (docs/goal/ui/connected-apps.md): the Requests tray, the roster, Connect
     *  an app and Blocked apps. Every call is a plain authenticated nest request,
     *  so it needs no actor secret; [mail] is the Mail & Calendar machine whose
     *  app passwords are rows of the roster (`null` builds a roster without
     *  them). Returns null until connected to a nest; each call builds a fresh
     *  machine bound to the current connection (mirrors [buildDevicesMachine]). */
    fun buildConnectedAppsMachine(
        observer: uniffi.fauna_client_connected_apps.ConnectedAppsObserver,
        mail: uniffi.fauna_client_mail_settings.MailSettingsMachine?,
    ): uniffi.fauna_client_connected_apps.ConnectedAppsMachine? =
        api.nestClient?.let { com.fauna.ffi.buildConnectedAppsMachine(it, observer, mail) }

    /** The `atproto` page's `AtprotoSettingsMachine` (docs/goal/ui/atproto.md):
     *  the integration-depth selector + F1 app-credential/session/kill-switch
     *  surface. Needs the actor secret — credential secrets are custodied
     *  client-side under the BackupKey derived from it, same as
     *  [buildMailSettingsMachine].
     *
     *  Connection-bound singleton, like [feedManager]: built once and reused
     *  across every `settings/atproto` visit, NOT rebuilt per call. [observer]
     *  is wired in only on the build that actually constructs the machine — a
     *  cache hit ignores it, exactly as [feedManager] does for a re-attach, and
     *  for the same debounce reason [AtprotoSettingsHost] documents. */
    fun buildAtprotoSettingsMachine(
        observer: uniffi.fauna_atproto_settings_machine.AtprotoSettingsObserver,
    ): uniffi.fauna_atproto_settings_machine.AtprotoSettingsMachine? {
        atprotoSettingsMachine?.let { return it }
        val client = api.nestClient ?: return null
        val secretBytes = api.secret?.let { HexUtil.hexToBytes(it) } ?: return null
        val machine = com.fauna.ffi.buildAtprotoSettingsMachine(client, secretBytes, observer)
        atprotoSettingsMachine = machine
        return machine
    }

    /**
     * Opportunistically refresh the published mail content-sealing epoch
     * schedule — the native twin of linux `FaunaClient::refresh_mail_epoch_schedule`,
     * fired at the same universal post-auth hook as [selfHealDeploymentSeedCustody] and
     * mirroring its best-effort, fire-and-forget posture
     * (`docs/goal/architecture/encryption-at-rest.md` § Capability tiering →
     * *Content-sealing epochs*). Delegates to the shared, idempotent
     * `MailSettingsMachine.refreshEpochSchedule`, which no-ops when mail isn't
     * enabled (no MSEK). A `null` machine (not yet connected, or no
     * secret) is a silent no-op — the next connect retries.
     */
    suspend fun refreshMailEpochSchedule() {
        buildMailSettingsMachine()?.refreshEpochSchedule()
    }

    /** Per-account aliases (`fauna.bridges.{list,create,update,revoke,delete}_account_alias`
     *  + generate_disposable — backend largely built → genuinely green). */
    fun buildMailAliasesMachine(): uniffi.fauna_client_mail_settings.MailAliasesMachine? =
        api.nestClient?.let { com.fauna.ffi.buildMailAliasesMachine(it) }

    /** Mailing lists (`fauna.bridges.*_account_list` — backend not yet built;
     *  actions surface the seam's `unimplemented` rejection via error-message). */
    fun buildMailListsMachine(): uniffi.fauna_client_mail_settings.MailListsMachine? =
        api.nestClient?.let { com.fauna.ffi.buildMailListsMachine(it) }

    /** One list's members (`fauna.bridges.*_list_member` — backend not yet built).
     *  Scoped to a single list_id; reached from a mail-lists row's members button. */
    fun buildMailListMembersMachine(
        listIdHex: String,
        listName: String,
    ): uniffi.fauna_client_mail_settings.MailListMembersMachine? =
        api.nestClient?.let { com.fauna.ffi.buildMailListMembersMachine(it, listIdHex, listName) }

    /** Mailbox-export wizard (`mail-export.md`), built **with key custody**: the
     *  actor secret opens every record and wraps the per-session blob key, and
     *  § Download flow writes the recovered archive into [mailExportSaveDir].
     *  ⚠ Custody obliges the caller to spawn `runExport()` once after a
     *  Start/Resume that lands `RUNNING` ([com.fauna.app.ui.viewmodel.MailExportVM]
     *  does) — custody without the spawn opens a session nothing drives.
     *  No secret → the custody-less build, never custody without a spawn:
     *  listing, Cancel and Discard keep working and Start refuses honestly. */
    fun buildMailExportMachine(): uniffi.fauna_client_mail_settings.MailExportMachine? {
        val client = api.nestClient ?: return null
        val secretBytes = api.secret?.let { HexUtil.hexToBytes(it) }
            ?: return com.fauna.ffi.buildMailExportMachine(client)
        return com.fauna.ffi.buildMailExportMachineWithKeyCustody(
            client,
            secretBytes,
            api.nodeUrl,
            mailExportActorHandle(),
            mailExportSaveDir().absolutePath,
        )
    }

    /** The signed-in account's handle as known right now (empty when not yet
     *  known). It names the export archive's root directory and saved file, and
     *  may arrive after the machine is built or change with the user, so
     *  [com.fauna.app.ui.viewmodel.MailExportVM] reads it at each gesture. */
    fun mailExportActorHandle(): String = api.sessionAccount.handle.orEmpty()

    /** Where § Download flow step 5 writes the recovered `.zip.zst`: the app's
     *  own download directory ([downloadsDir], the one every android download
     *  surface saves into) — the user-facing destination is the share sheet the
     *  screen opens over the finished file (`mail-export.md` § Implementation
     *  status today, the android paragraph). The shared sink writes
     *  `<name>.part` and renames it into place only once the archive is
     *  complete and terminated, so a refused download leaves nothing here to
     *  hand on. */
    fun mailExportSaveDir(): java.io.File = api.context.downloadsDir()

    /** Mailbox-IMPORT wizard (`mailbox-migration.md`). Unlike the export twin the
     *  backend is REAL end to end — the nest half shipped 2026-07-08 and the source
     *  half opens a live IMAP session against the foreign server — so a rejection
     *  here is a genuine nest or source answer, never an unbuilt-backend explanation.
     *  ⚠ A successful Start/Resume obliges the caller to spawn `runImport()` once;
     *  the shared machine deliberately does not self-spawn it. */
    fun buildMailImportMachine(): uniffi.fauna_client_mail_settings.MailImportMachine? =
        api.nestClient?.let { com.fauna.ffi.buildMailImportMachine(it) }

    /** Per-account spam training (`fauna.bridges.*_spam_*`). Needs the actor secret +
     *  node URL to unwrap/re-seal the sealed spam model client-side (the tier-1
     *  client-write path, mail-spam.md § Encrypted-mode interaction) — same threading
     *  as [buildMailSettingsMachine]. (Call-site update for the landed
     *  `build_mail_spam_machine` signature grow.) */
    fun buildMailSpamMachine(): uniffi.fauna_client_mail_settings.MailSpamMachine? {
        val client = api.nestClient ?: return null
        val secretBytes = api.secret?.let { HexUtil.hexToBytes(it) } ?: return null
        return com.fauna.ffi.buildMailSpamMachine(client, secretBytes, api.nodeUrl)
    }

    // ── Flat admin-mail pages (Bundle B; admin.md § 4/§ 6, mail-bridge-lifecycle.md
    //    § Pending approval). All Admin-class — dumb renderers of the shared
    //    libs/fauna-client-mail-settings machines over UniFFI. ──

    /** Flat `admin-mail` policy page (`fauna.bridges.get_mail_config` +
     *  get_alias_policy reads; set_mail_enabled + put_{spam,auth,submission,imap,
     *  outbound,alias}_policy writes — all live). */
    fun buildMailPolicyMachine(): uniffi.fauna_client_mail_settings.MailPolicyMachine? =
        api.nestClient?.let { com.fauna.ffi.buildMailPolicyMachine(it) }

    /** Flat `admin-calendar` page — the deployment-wide CalDAV-enable toggle, the
     *  sibling of `admin-mail`'s mail-enable toggle (`admin.md` § 8 Calendar;
     *  `caldav-server.md` § Independent enablement). Hydrates `caldav_enabled` from
     *  `fauna.bridges.get_mail_config` and writes via `fauna.bridges.set_caldav_enabled`
     *  (both Admin-class, live). */
    fun buildCaldavPolicyMachine(): uniffi.fauna_client_mail_settings.CaldavPolicyMachine? =
        api.nestClient?.let { com.fauna.ffi.buildCaldavPolicyMachine(it) }

    /** `admin-contacts` deployment-wide CardDAV-enable toggle (`admin.md` § Contacts;
     *  `carddav-server.md` § Independent enablement) — the contacts sibling of
     *  [buildCaldavPolicyMachine]. Hydrates `carddav_enabled` from
     *  `fauna.bridges.get_mail_config` and writes via `fauna.bridges.set_carddav_enabled`
     *  (both Admin-class, live). */
    fun buildCarddavPolicyMachine(): uniffi.fauna_client_mail_settings.CarddavPolicyMachine? =
        api.nestClient?.let { com.fauna.ffi.buildCarddavPolicyMachine(it) }

    /** `admin-files` deployment-wide WebDAV-enable toggle (`admin.md` § Files;
     *  `webdav-server.md` § Independent enablement) — the files sibling of
     *  [buildCarddavPolicyMachine]. Hydrates `webdav_enabled` from
     *  `fauna.bridges.get_mail_config` and writes via `fauna.bridges.set_webdav_enabled`
     *  (both Admin-class, live). No port field — WebDAV rides the shared DAV listener. */
    fun buildWebdavPolicyMachine(): uniffi.fauna_client_mail_settings.WebdavPolicyMachine? =
        api.nestClient?.let { com.fauna.ffi.buildWebdavPolicyMachine(it) }

    /** `admin-aliases` external forwarders (`fauna.bridges.{list,create,delete}_forwarder`
     *  + list_local_domains — backend built → genuinely green). */
    fun buildForwardersMachine(): uniffi.fauna_client_mail_settings.ForwarderMachine? =
        api.nestClient?.let { com.fauna.ffi.buildForwardersMachine(it) }

    /** `admin-bridges-pending` approval feed (`fauna.bridges.{list_pending,
     *  approve_pending,reject_pending}_bridges` + set_mail_enabled — backend real). */
    fun buildBridgeApprovalMachine(): uniffi.fauna_client_mail_settings.BridgeApprovalMachine? =
        api.nestClient?.let { com.fauna.ffi.buildBridgeApprovalMachine(it) }

    // ── admin-dns / admin-services DNS surface (Bundle B surface 4; admin.md § 5,
    //    dns-management.md § App surface, mail-multidomain.md § Per-domain
    //    catch-all). The DNS record matrix + managed-mode credentials ride the
    //    shared `DnsManagementMachine` (libs/fauna-client-dns); domain CRUD + the
    //    per-domain catch-all ride `LocalDomainMachine` (libs/fauna-client-mail-
    //    settings). The page merges the two snapshots by domain name (linux/web/
    //    windows pattern). Both build fresh against the current WS-RPC connection
    //    and return null until the socket is up (the page hydrates with retry). ──

    /** The credentialed `DnsManagementMachine` (`fauna.dns.{list_records,
     *  verify_records}` reads + the client-held `fauna.state.dns` credential store
     *  for managed-mode publish; the nest never sees the provider key). Needs the
     *  actor secret to seal/unseal the credential store. */
    fun buildDnsManagementMachineWithCredentials(): uniffi.fauna_client_dns.DnsManagementMachine? {
        val client = api.nestClient ?: return null
        val secretBytes = api.secret?.let { HexUtil.hexToBytes(it) } ?: return null
        return com.fauna.ffi.buildDnsManagementMachineWithCredentials(client, secretBytes)
    }

    /** The `LocalDomainMachine` (`fauna.bridges.{add,remove,restore}_local_domain`
     *  + `set_catch_all_actor`) backing the admin-dns per-domain CRUD + catch-all. */
    fun buildLocalDomainsMachine(): uniffi.fauna_client_mail_settings.LocalDomainMachine? =
        api.nestClient?.let { com.fauna.ffi.buildLocalDomainsMachine(it) }

    /** The `fauna.web.*` authoring client (web-content-hosting.md § Admin apex
     *  hosting / § Published-post management) — the per-user subdomain opt-in toggle (`web-settings` page, User-scoped)
     *  and the nest-wide admin apex-actor designation (`admin-web` page, Admin-class)
     *  over the shared `fauna-client-web` crate. Returns null until the WS socket is
     *  up; the page hydrates with retry. Mirrors [buildMailAliasesMachine]; the same
     *  `FfiWebClient` linux/web/windows/apple already consume (no new Rust). */
    fun webClient(): com.fauna.ffi.FfiWebClient? =
        api.nestClient?.let { com.fauna.ffi.buildWebClient(it) }

    /**
     * Run the feeders that have no page of their own, at the same universal
     * post-auth hook as [selfHealDeploymentSeedCustody] / [refreshMailEpochSchedule]
     * (`critical-alerts.md` § Mechanism → *Who runs the detector*). tui's
     * `critical_alerts::spawn_session_start_sweep` (`session::establish`) is
     * the reference; [com.fauna.ffi.runCriticalAlertSweep] is the one FFI
     * export android shares with windows/apple. Fire-and-forget:
     * sign-in must not fail, or even wait, on a nest that cannot answer the
     * recovery/directory planes.
     */
    suspend fun runCriticalAlertSweep() =
        com.fauna.ffi.runCriticalAlertSweep(api.backupNest(), api.backupSecret())

    /**
     * [runCriticalAlertSweep]'s repeating twin (`critical-alerts.md` §
     * Mechanism → *Who runs the detector*, ratified 2026-08-02): sweeps
     * immediately, then every `RE_SWEEP_INTERVAL_SECS` for as long as the
     * identity lives. Never returns under normal operation — callers MUST
     * launch this on a scope that outlives the caller
     * ([CriticalAlertsHost.startSweepLoop]), never a short-lived
     * ViewModel/Composable scope.
     */
    suspend fun runCriticalAlertSweepLoop() =
        com.fauna.ffi.runCriticalAlertSweepLoop(api.backupNest(), api.backupSecret())

    /** §1 create — encrypted-mode mint path: record the fresh period key, then create. */
    suspend fun subscriptionCreateTier(
        name: String,
        rank: UInt,
        description: String?,
        priceHint: String?,
        paymentUrl: String?,
        autoApprove: Boolean,
        // The machine-comparable price in sats (monetization.md § The asking
        // price) — independent of priceHint above. `null` leaves the tier
        // unbuyable by an inferring mechanism, the permanently-correct
        // default: a zap on it stays a tip.
        askingPriceSats: ULong? = null,
    ): Boolean = com.fauna.ffi.subscriptionsCreateTier(
        api.nestRpc(), api.ownerSecretBytes(), name, rank, description, priceHint, paymentUrl, autoApprove,
        askingPriceSats,
    )

    /** §2 approve — transparent mint+upload of a roster-covering KeyBlob (encrypted mode). */
    suspend fun subscriptionApproveRequest(
        request: com.fauna.ffi.FfiPendingRequest,
    ): com.fauna.ffi.FfiApproveReply =
        com.fauna.ffi.subscriptionsApproveSubscriber(api.nestRpc(), api.ownerSecretBytes(), request)

    /** §3 remove — rotate the period key + re-mint over the reduced roster (encrypted mode). */
    suspend fun subscriptionRemoveSubscriber(tierName: String, subscriberId: ByteArray) =
        com.fauna.ffi.subscriptionsRemoveSubscriber(
            api.nestRpc(), api.ownerSecretBytes(), tierName, subscriberId,
        )

    /**
     * Subscribe the viewer to [authorIdHex]'s [tier], **publishing** the viewer's
     * identity-seed ML-KEM ek (surface B, S4b) unconditionally (no capability
     * token), so the author can later wrap hybrid `KeyBlob`s to this subscriber.
     * `Approved` (plaintext / auto-approve) or `Queued` (encrypted — pending the
     * author's confirm); follow is a subscribe to the free "followers" tier. Mirrors linux `offers.rs::subscribe_to` / `mod.rs::follow`.
     */
    suspend fun subscriptionSubscribe(authorIdHex: String, tier: String): com.fauna.ffi.FfiSubscribeReply =
        com.fauna.ffi.subscriptionsSubscribePublishingEk(
            api.nestRpc(), api.ownerSecretBytes(), HexUtil.hexToBytes(authorIdHex), tier)

    /**
     * The deployment "Fauna controls DNS" master switch (`admin-service-dns-toggle`
     * on admin-services / `admin-dns-manage-all-toggle` on admin-dns). There is no
     * nest `dns` service flag — this folds every active domain's per-domain mode at
     * once over the shared `DnsManagementMachine`: opt each domain in/out via
     * `SetMode`, which publishes the records itself on opt-in. Mirrors Linux `client.dns_set_all_managed`
     * (apps/fauna-linux/src/client.rs); the canonical *read* projection is
     * `DnsManagementMachine.allDomainsManaged(activeDomains)`, never a re-coded fold.
     */
    suspend fun dnsSetAllManaged(managed: Boolean) {
        val machine = buildDnsManagementMachineWithCredentials()
            ?: throw ApiException("Not connected to nest (WS-RPC)")
        machine.dispatch(uniffi.fauna_client_dns.DnsAction.Refresh)
        val domains = machine.snapshot().domains.map { it.domain }
        for (domain in domains) {
            machine.dispatch(uniffi.fauna_client_dns.DnsAction.SetMode(domain = domain, managed = managed))
        }
    }

    /**
     * Flip the deployment-wide mail toggle (`fauna.bridges.set_mail_enabled`)
     * via the shared [BridgeApprovalMachine]'s `SetMailEnabled` action — the
     * same Admin-class path the mail-settings page uses, so this is idempotent
     * with it. Used by the post-onboarding enable-email hand-off
     * (onboarding.md §3b) and mirrors Linux's `client.rs::set_mail_enabled`.
     * The action wrapper carries the shared idempotent retry on transient WS
     * drops, so a brief reconnect during enable is tolerated.
     */
    suspend fun setMailEnabled(enabled: Boolean) {
        api.ensureAuthenticated()
        val client = api.nestClient ?: throw ApiException("Not connected to nest (WS-RPC)")
        com.fauna.ffi.buildBridgeApprovalMachine(client).dispatch(
            uniffi.fauna_client_mail_settings.BridgeApprovalAction.SetMailEnabled(enabled = enabled),
        )
    }

    /**
     * Flip the deployment-wide CalDAV toggle (`fauna.bridges.set_caldav_enabled`)
     * via the shared [BridgeApprovalMachine]'s `SetCalDavEnabled` action —
     * sibling of [setMailEnabled], gating CalDAV independently of email
     * (caldav-server.md § Independent enablement). Same Admin-class, idempotent
     * path the mail-settings page uses; mirrors Linux's
     * `client.rs::set_caldav_enabled`. Used by the post-onboarding
     * enable-caldav hand-off (onboarding.md §3b).
     */
    suspend fun setCaldavEnabled(enabled: Boolean) {
        api.ensureAuthenticated()
        val client = api.nestClient ?: throw ApiException("Not connected to nest (WS-RPC)")
        com.fauna.ffi.buildBridgeApprovalMachine(client).dispatch(
            uniffi.fauna_client_mail_settings.BridgeApprovalAction.SetCalDavEnabled(enabled = enabled),
        )
    }

    /**
     * Flip the deployment-wide CardDAV toggle (`fauna.bridges.set_carddav_enabled`)
     * via the shared [BridgeApprovalMachine]'s `SetCardDavEnabled` action — the
     * contacts sibling of [setCaldavEnabled], gating CardDAV independently of both
     * email and calendar (carddav-server.md § Independent enablement). Same
     * Admin-class, idempotent path; mirrors Linux's
     * `client.rs::set_carddav_enabled`. Used by the post-onboarding
     * enable-carddav hand-off.
     */
    suspend fun setCarddavEnabled(enabled: Boolean) {
        api.ensureAuthenticated()
        val client = api.nestClient ?: throw ApiException("Not connected to nest (WS-RPC)")
        com.fauna.ffi.buildBridgeApprovalMachine(client).dispatch(
            uniffi.fauna_client_mail_settings.BridgeApprovalAction.SetCardDavEnabled(enabled = enabled),
        )
    }

    /**
     * Flip the deployment-wide WebDAV toggle (`fauna.bridges.set_webdav_enabled`)
     * via the shared [BridgeApprovalMachine]'s `SetWebDavEnabled` action — the
     * files sibling of [setCarddavEnabled], gating WebDAV independently of email,
     * calendar, and contacts (webdav-server.md § Independent enablement). Same
     * Admin-class, idempotent path; mirrors Linux's
     * `client.rs::set_webdav_enabled`. Used by the post-onboarding
     * enable-webdav hand-off.
     */
    suspend fun setWebdavEnabled(enabled: Boolean) {
        api.ensureAuthenticated()
        val client = api.nestClient ?: throw ApiException("Not connected to nest (WS-RPC)")
        com.fauna.ffi.buildBridgeApprovalMachine(client).dispatch(
            uniffi.fauna_client_mail_settings.BridgeApprovalAction.SetWebDavEnabled(enabled = enabled),
        )
    }

    // -- Nostr succession-aftermath npub confirm --
    //
    // Thin wrappers over `libs/fauna-ffi/src/nostr_npub_confirm.rs` — zero
    // logic owed (priority #2), the predicate and the write both live in
    // shared Rust (`fauna_client_config::nostr_npub_confirm`).

    /** Is the caller owed an npub confirmation right now — the Nostr page's
     *  nav-enter read (tui `nostr.rs::refresh_and_check_npub` is the
     *  reference). Best-effort: any unhappy answer degrades to `false`
     *  rather than throwing. */
    suspend fun npubConfirmationOwed(): Boolean =
        com.fauna.ffi.npubConfirmationOwed(api.nestRpc(), api.ownerSecretBytes())

    /** Record the owner's "yes, that's my npub" confirmation. `now` is the
     *  caller's own clock, epoch seconds. */
    suspend fun confirmNostrNpub(now: Long) {
        com.fauna.ffi.confirmNpub(api.nestRpc(), api.ownerSecretBytes(), now)
    }

    // -- Sealed tier-1 spam training (mail-spam.md § Encrypted-mode interaction) --
    //
    // The three calls the moderation queue and the conversation "Mark as spam"
    // gesture make into the mail-settings machine, narrowed to kept types so
    // those two shared VMs stay in `src/main`: the kids twin answers "no sealed
    // model" and the callers fall through to their non-mail path.

    /** Whether the nest advertises `spam-model-sealed-at-rest` and mail is enabled
     *  for this actor; `false` with no mail-settings machine (not connected). */
    internal suspend fun sealedSpamWriteAvailable(): Boolean =
        buildMailSettingsMachine()?.sealedSpamWriteAvailable() ?: false

    /** Train the sealed spam model client-side over [text]; `true` iff the write
     *  landed sealed (`false` on the ServerPath race or with no machine). */
    internal suspend fun trainSpamModelClient(text: String, isSpam: Boolean): Boolean =
        buildMailSettingsMachine()?.trainSpamModelClient(text, isSpam)?.sealed ?: false

    /** [trainSpamModelClient] plus the sealed training-history row the `mail-spam`
     *  page renders; a no-op with no mail-settings machine. */
    internal suspend fun trainSpamModelClientMail(
        text: String,
        isSpam: Boolean,
        messageId: ByteArray,
        mailbox: String,
        subject: String,
    ) {
        buildMailSettingsMachine()?.trainSpamModelClientMail(text, isSpam, messageId, mailbox, subject)
    }
}
