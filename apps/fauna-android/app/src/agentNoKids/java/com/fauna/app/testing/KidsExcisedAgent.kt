package com.fauna.app.testing

import android.content.Context
import androidx.compose.foundation.gestures.scrollBy
import com.fauna.app.core.ExifStripper
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.ffi.appOutboxDrainNowForTest
import com.fauna.ffi.criticalAlertSweepWakeForTest
import com.fauna.ffi.criticalAlertsRegistry
import com.fauna.ffi.offlineShareAdvanceClock
import com.fauna.ffi.offlineShareDropConnectionsForTest
import com.fauna.ffi.offlineShareHoldServes
import com.fauna.ffi.setDelegationClockOffsetSecs
import com.fauna.ffi.setOutboxDrainHoldForTest
import com.fauna.ffi.subscriptionsHoldAuthorDrainForTest
import com.fauna.ffi.subscriptionsReleaseAuthorDrainForTest
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject
import uniffi.fauna_feed.AttachedFile

/**
 * The collaborators [KidsExcisedAgent] reaches that the kids build's graph does
 * not bind — kept off [TestAgentEntryPoint], which both agent build types share.
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface KidsExcisedAgentEntryPoint {
    fun feedManagerHost(): FeedManagerHost
    fun atprotoSettingsHost(): com.fauna.app.core.atproto.AtprotoSettingsHost
}

/**
 * The agent arms that drive a plane the kids build excises — **the built half**
 * (`src/agentNoKids`, taken by `debug` only). `src/agentKids` holds the
 * refusing twin `kidsDebug` takes, signature for signature; the set of actions
 * routed here is [KidsExcisedActions.PLANES], shared by both. Each arm is the
 * one [TestAgent]'s dispatch used to carry inline, moved verbatim.
 */
object KidsExcisedAgent {
    @Volatile private var feedManagerHost: FeedManagerHost? = null
    @Volatile private var atprotoSettingsHost: com.fauna.app.core.atproto.AtprotoSettingsHost? = null

    /** Resolve the excised collaborators; called from [TestAgent.start]. */
    fun start(context: Context) {
        val entryPoint = EntryPointAccessors.fromApplication(context, KidsExcisedAgentEntryPoint::class.java)
        // The same Hilt @Singleton the Feed screens observe, so
        // `feed_inject_posts` drives the on-screen manager.
        feedManagerHost = entryPoint.feedManagerHost()
        // The same Hilt @Singleton the AT Protocol page observes — the D10
        // `atproto_delegation_advance_clock` command must repaint THAT
        // machine's snapshot, not a throwaway one.
        atprotoSettingsHost = entryPoint.atprotoSettingsHost()
    }

    /**
     * Run one [KidsExcisedActions.PLANES] command: `null` when honoured, else
     * the refusal ([TestAgent]'s dispatch contract).
     */
    suspend fun dispatch(action: String, cmd: JSONObject): String? {
        when (action) {
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
                val (key, message) = TestAgent.feedInjectErrorArgs(cmd)
                manager.injectErrorForTest(key, message)
            }
            "feed_seed_cue_rollup_for_test" -> {
                // Seeds a real `cues:v1` nest row for "Clear activity data" to
                // delete (a real round trip, unlike `feed_inject_posts`).
                // `{content_ids: string[]}`; forwards to the shared Rust seam
                // `FeedManager::set_cue_rollup_for_test`, awaited so the caller
                // reads the nest row with no poll. Twin of tui's, linux's and
                // apple's arms of the same name; a refusal is LOUD (convention 11).
                val manager = feedManagerFor(action).getOrElse { return it.message }
                val ids = cmd.optJSONArray("content_ids")
                val contentIds = (0 until (ids?.length() ?: 0)).map { ids!!.getString(it) }
                try {
                    manager.setCueRollupForTest(contentIds)
                } catch (e: Exception) {
                    return "feed_seed_cue_rollup_for_test threw ${e::class.simpleName}: ${e.message}"
                }
            }
            "feed_cue_verdict_for_test" -> {
                // The live engagement-cue engine's verdict for `{content_id}` and
                // whether it has hydrated from the nest yet — the JSON
                // `{hydrated, verdict}` (`verdict` kebab-case or null), read-only:
                // how a seat witnesses the rollup another device saved
                // (`engagement-cues.md` § At rest). Over the shared FFI face
                // `FfiFeedManager.cueVerdictForTest`; twin of tui's, linux's,
                // web's, windows' and apple's arms of the same name.
                val manager = feedManagerFor(action).getOrElse { return it.message }
                val contentId = cmd.optString("content_id", "")
                if (contentId.isEmpty()) return "feed_cue_verdict_for_test carried no `content_id`"
                TestAgent.machineMethodResult = manager.cueVerdictForTest(contentId)
            }
            // Hold / release this process's subscriptions author drain, so a
            // payment-backed subscribe request stands in the author's §2 while
            // the test reads its paid badge (`monetization.md` § Pillar 3). The
            // flag is shared Rust's process-wide one, which the in-process
            // author pump reads. Twins of tui's and linux's arms.
            "subscriptions_hold_author_drain" -> subscriptionsHoldAuthorDrainForTest()
            "subscriptions_release_author_drain" -> subscriptionsReleaseAuthorDrainForTest()
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
            "feed_cue_clock_freeze", "feed_cue_clock_thaw", "feed_cue_clock_advance",
            "feed_cue_probe", "feed_cue_scroll_by" -> {
                // The engagement-cue test clock (convention 14; the shared
                // tracker's seam, `fauna_feed::CueTracker` § The test clock,
                // through `FfiCueTracker`'s test-helpers faces). Twins of tui's,
                // linux's (`feed::viewport::test_clock`) and web's
                // (`#lib/feed-cues-e2e`) arms, with their reply shapes: `{ok}`
                // for a step, the `CueProbe` for the probe, `{ok, moved, probe}`
                // for the pixel step, and `{error}` for a refusal — which is
                // also reported on `error-message` (convention 11).
                val reply = try {
                    feedCueCommand(action, cmd)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    val reason = "$action: ${e.message}"
                    TestAgent.machineMethodResult = JSONObject().put("error", reason).toString()
                    return reason
                }
                TestAgent.machineMethodResult = reply.toString()
            }
            "app_outbox_drain_now" -> {
                // The app-only outbox drain, rung now — android's twin of tui's
                // and linux's arm (`fauna_e2e_agent::APP_OUTBOX_DRAIN_NOW` owns
                // the contract): the process outbox's own drain loop, for a test
                // that has just released the e2e drain hold, whose held pass
                // ended the loop. The FFI body spawns on the store runtime and
                // returns at once; a no-op pre-auth. The barrier is the effect.
                appOutboxDrainNowForTest()
            }
            "outbox_drain_hold" -> {
                // The in-process e2e outbox drain hold
                // (`fauna_e2e_agent::OUTBOX_DRAIN_HOLD`): this process hosts the
                // account runtime and inherits no harness environment, so the
                // hold file the desktop apps read cannot reach it. A missing or
                // non-boolean `held` is a loud refusal (convention 11).
                val held = cmd.opt("held") as? Boolean
                    ?: return "outbox_drain_hold needs a boolean `held`"
                setOutboxDrainHoldForTest(held)
            }
            "enable_caldav_mailbox" -> {
                // Mint the logged-in actor's shared MSEK + `default` mail credential
                // through the shared `MailSettingsMachine` — the twin of apple's
                // `CaldavMailboxTestCommand` and linux's `enable_caldav_mailbox_for_test`
                // (one recipe, shared Rust): a `password` mints a credential the test
                // knows (an organizer a stock CalDAV client AUTHs as), none generates
                // one (an attendee who never speaks raw CalDAV). Answered
                // asynchronously into `caldav_mailbox_reply`, cleared first, like
                // the `serve_enable_folder` arm above.
                TestAgent.caldavMailboxReply = null
                val api = TestAgent.apiClient
                    ?: return "enable_caldav_mailbox arrived before the ApiClient was wired"
                val password = cmd.optString("password", "").ifEmpty { null }
                TestAgent.pokeScope.launch {
                    TestAgent.caldavMailboxReply = try {
                        val machine = api.buildMailSettingsMachine()
                            ?: throw IllegalStateException("no nest connection for the mail-settings machine")
                        machine.use {
                            if (password != null) {
                                it.enableDavMailboxWithPassword("Default", password)
                            } else {
                                it.enableDavMailboxWithGeneratedPassword("Default")
                            }
                        }
                        JSONObject().put("ok", true)
                    } catch (e: Exception) {
                        JSONObject().put("ok", false).put("error", "${e::class.simpleName}: ${e.message}")
                    }
                }
            }
            "atproto_delegation_advance_clock" -> {
                // The D10 lapse journey's fake RENDER clock (convention 14) —
                // android's twin of linux `main.rs` / tui `automation.rs` /
                // apple `DelegationClockTestCommand.swift` / web
                // `#lib/atproto-delegation-e2e.ts`. All five drive the same
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
            "offline_share_advance_clock" -> {
                // The co-present ceremony's ADMISSION clock (convention 14) —
                // android's twin of tui `automation.rs` / linux `main.rs` /
                // apple `OfflineShareTestCommand.applyAdvanceClock`, all over
                // the shared seam `fauna_sync_engine::ceremony_clock`: the `now`
                // a receive-act expectation is minted and judged against. The
                // 15-minute window is a Rust constant, so "a lapsed receive
                // window refuses the initiator like a stranger" (`p2p.md`
                // § Offline share initiation) is reached only by moving this
                // clock. `0` resets; process-wide, nothing auto-resets it.
                // Nothing to repaint and nothing to refuse: the seat's own
                // listener reads the offset at admission time.
                offlineShareAdvanceClock(cmd.optLong("now_offset_secs", 0L))
            }
            "offline_share_drop_connections" -> {
                // Drop every connection a counterpart has open to THIS
                // session's ceremony listener, keeping it up, and report how
                // many (`actions/backups.py::drop_offline_share_connections`
                // reads the count off `machine_method_result`) — the link
                // between two devices failing part-way, which "the share picks
                // up again without the code a second time" needs. Twin of tui's
                // / linux's arms and apple's `applyDropConnections`; the seat
                // is the session slot `OfflineShareHost` binds through, so the
                // shared door reads it, never a UI-held handle. The slot is
                // cleared FIRST, so a refusal can never leave a stale
                // neighbour's value to be read as a count (convention 11).
                TestAgent.machineMethodResult = null
                val api = TestAgent.apiClient
                    ?: return "offline_share_drop_connections: no ApiClient wired " +
                        "(TestAgent.start has not run yet), so no ceremony seat to drop " +
                        "connections from"
                try {
                    val dropped = offlineShareDropConnectionsForTest(api.ownerSecretBytes())
                    TestAgent.machineMethodResult = dropped.toLong().toString()
                } catch (e: Exception) {
                    return "offline_share_drop_connections: ${e.message ?: e::class.simpleName}"
                }
            }
            "offline_share_hold_serves" -> {
                // Turn THIS process's share-plane SERVE hold on or off
                // (`fauna_sync_engine::share_serve_tally`): while it is on, the
                // next manifest a peer asks this seat for parks unanswered and
                // the `share_serve_tally` state key's `parked` count says so —
                // "part-way through a transfer" as a state a journey waits for
                // (convention 14), cut with `offline_share_drop_connections`,
                // then lifted: the parked request fails and is never counted as
                // served. Twin of tui's / linux's arms, windows' `TestAgent` and
                // apple's `applyHoldServes`, over the same shared door; the
                // serve side is the plane `SharePlaneHost` runs in this
                // process. Process-wide, nothing auto-resets it; a missing
                // `hold` reads as `false`, the lift. Cannot refuse.
                offlineShareHoldServes(cmd.optBoolean("hold", false))
            }
            "archive_import_pause_after", "archive_import_hold_after" -> {
                // Arm the mounted archive-import page's machine to pause (or
                // hold, until the user's own Pause or Cancel) after `records`
                // settled records — the restart-resume and pause/cancel
                // journeys' causal anchors (convention 14). Twins of tui's
                // `automation.rs` arms, windows' `TestAgent` and FaunaKit's
                // `ArchiveImportTestCommand`; the two setters are
                // `test-helpers` UniFFI exports, present in the debug face only.
                if (!cmd.has("records") || cmd.isNull("records")) {
                    return "$action: needs a numeric `records`"
                }
                val records = cmd.optLong("records", -1L)
                if (records < 0) return "$action: needs a numeric `records`"
                val machine = com.fauna.app.ui.viewmodel.ArchiveImportVM.current?.get()?.machineOrNull()
                    ?: return "$action: no archive-import page holds a machine"
                if (action == "archive_import_hold_after") {
                    machine.setHoldAfterRecordsForTest(records.toULong())
                } else {
                    machine.setPauseAfterRecordsForTest(records.toULong())
                }
            }
            else -> return "$action: not a kids-excised agent arm"
        }
        return null
    }

    /** `ApiClient.feedPostsJson()` — the non-building peek `data.feed.posts` reads. */
    fun feedPostsRaw(api: com.fauna.app.core.ApiClient?): String? = api?.feedPostsJson()

    /**
     * `feed_reloads` — convention 14's reload-barrier observable
     * (`fauna_e2e_agent::FEED_RELOADS_KEY`). `ApiClient.feedReloadsJson()` is a
     * non-building peek; no manager yet is a legitimate zero, the shared
     * derivation's own "None ⇒ zero, never null" contract
     * (`fauna_feed::feed_reloads_json`) — `null` is reserved for an app that
     * publishes no leg at all (the kids build). ⚠ The zero below is the ONE
     * place this shape is re-spelled rather than read, so it must grow with
     * the derivation: a SHORT zero reads as the no-leg refusal.
     */
    fun feedReloads(api: com.fauna.app.core.ApiClient?): Any =
        api?.feedReloadsJson()
            ?.let { j -> runCatching { org.json.JSONTokener(j).nextValue() as? JSONObject }.getOrNull() }
            ?: JSONObject().put("started", 0).put("completed", 0).put("committed_gen", 0)

    /**
     * `ward_rule_document_rules` — how many rules the guardian's document holds
     * on this device, read through the shared editor's own view (never counted
     * here); null for an unsupervised viewer or a policy with no document.
     */
    fun putWardRuleDocumentRules(data: JSONObject, store: com.fauna.app.core.ContentPolicyStore?) {
        data.put(
            "ward_rule_document_rules",
            store?.inputs?.value?.contentPolicy?.takeIf { it.document != null }?.let { policy ->
                runCatching {
                    com.fauna.ffi.FfiRuleEditor.forGuardian(policy).use { it.view().rows.size }
                }.getOrNull()
            } ?: JSONObject.NULL,
        )
    }

    /**
     * `fauna_e2e_agent::SHARE_SERVE_TALLY_KEY` — what this process's share
     * plane has served: the shared recorder's snapshot, a plain lock-and-clone
     * (convention 11's corollary), legal here.
     */
    fun putShareServeTally(data: JSONObject) {
        data.put("share_serve_tally", shareServeTallyJson())
    }

    /**
     * `alert_sweep_passes` — the sweep's own counters, bumped inside the shared
     * sweep on the ONE process-wide registry (`criticalAlertsRegistry()` hands
     * back the same `Arc` every call) — never an android-side count. A pair,
     * not one number: the waiter compares `completed` against the `started` it
     * read at plant time. A same-identity re-establish runs one pass, not a
     * second loop (the shared `startCriticalAlertSweep`'s claim).
     */
    fun putAlertSweepPasses(state: JSONObject) {
        val alerts = criticalAlertsRegistry()
        state.put(
            "alert_sweep_passes",
            JSONObject()
                .put("started", alerts.sweepPassesStarted().toLong())
                .put("completed", alerts.sweepPassesCompleted().toLong()),
        )
    }

    /**
     * `alert_sweep_wake`: end the current identity's re-sweep WAIT so the
     * production LOOP (`KidsExcisedNav`'s post-auth hook →
     * `run_critical_alert_sweep_loop`) sweeps again; the caller's barrier is
     * `alert_sweep_passes`, never this ack. Never a one-shot pass: that would
     * pass "announced without a restart" with the loop deleted.
     */
    fun alertSweepWake(action: String): String? =
        if (criticalAlertSweepWakeForTest()) {
            null
        } else {
            "$action: no sweep loop runs for this identity, so there is nothing to wake"
        }

    /**
     * `share_serve_tally` (`fauna_e2e_agent::SHARE_SERVE_TALLY_KEY`) — the
     * shared recorder's snapshot as `{manifests: {path: n}, chunks: {path: n},
     * parked: n, held: b}`, the shape tui's / linux's `share_serve_tally_json`
     * and apple's `serveTallyState` publish (`actions/backups.py::
     * share_serve_tally` reads it). Counts go out as JSON numbers, never a
     * boxed `ULong`.
     */
    internal fun serveTallyToJson(t: com.fauna.ffi.FfiServeTally): JSONObject =
        JSONObject().apply {
            put("manifests", JSONObject().apply { t.manifests.forEach { (k, v) -> put(k, v.toLong()) } })
            put("chunks", JSONObject().apply { t.chunks.forEach { (k, v) -> put(k, v.toLong()) } })
            put("parked", t.parked.toLong())
            put("held", t.held)
        }

    /** [serveTallyToJson] over this process's live tally — a lock-and-clone. */
    private fun shareServeTallyJson(): JSONObject = serveTallyToJson(com.fauna.ffi.shareServeTally())


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
     * One engagement-cue test-clock command (see its arm in [dispatchCommand]);
     * throws the refusal. The clock lives on the auth's ONE tracker
     * ([com.fauna.app.core.feed.FeedManagerHost.cueTracker]), so freeze, thaw
     * and advance work off the Feed page too; the probe's measuring sample and
     * the pixel step need the mounted list
     * ([com.fauna.app.ui.screen.feed.CueCapture.active]).
     *
     * Every sample taken here resolves only once the exposures it ended are
     * recorded — the engine's train and any put included — so an ack means
     * nothing more is on its way: android has no quit door a test can wait
     * out, so this is its barrier, as on web.
     */
    private suspend fun feedCueCommand(action: String, cmd: JSONObject): Any {
        val manager = feedManagerFor(action).getOrThrow()
        val tracker = feedManagerHost!!.cueTracker(manager)
        val capture = com.fauna.app.ui.screen.feed.CueCapture.active
        fun probe(): JSONObject = JSONObject(tracker.probeForTest())
        return when (action) {
            "feed_cue_clock_freeze" -> {
                tracker.freezeClockForTest(
                    android.os.SystemClock.elapsedRealtime().toULong(),
                    System.currentTimeMillis().toULong(),
                )
                JSONObject().put("ok", true)
            }
            "feed_cue_clock_thaw" -> {
                tracker.thawClockForTest()
                JSONObject().put("ok", true)
            }
            "feed_cue_clock_advance" -> {
                // Sample at once, so each step lands as exactly one sample's
                // credit (the 250 ms tick could fold two advances into one
                // capped sample).
                tracker.advanceClockForTest(cmd.optLong("ms", 0L).toULong())
                capture?.let { it.record(it.sample()) }
                JSONObject().put("ok", true)
            }
            "feed_cue_probe" -> {
                // Frozen, a sample credits nothing — it only measures — so the
                // probe reads the list as laid out now, not as the last tick saw it.
                if (capture != null && probe().optBoolean("frozen")) {
                    TestAgent.awaitLaidOutFrame()
                    capture.record(capture.sample())
                }
                probe()
            }
            "feed_cue_scroll_by" -> {
                // Scroll the post list's real offset by `px`, then answer after
                // the next frame's layout from a sample of its own, with the
                // probe as THAT sample left it — a later read could already show
                // the feed's re-rank in answer to an exposure the step ended.
                // `moved: false` at the end of the list.
                capture ?: throw IllegalStateException("the feed page is not mounted")
                val consumed = capture.listState.scrollBy(cmd.optDouble("px", 0.0).toFloat())
                TestAgent.awaitLaidOutFrame()
                val left = capture.sample()
                val after = probe()
                capture.record(left)
                JSONObject().put("ok", true).put("moved", consumed != 0f).put("probe", after)
            }
            else -> throw IllegalArgumentException("unhandled feed cue command")
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
    fun attachFeedComposeFile(path: String): String? {
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
}
