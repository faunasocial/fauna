import Foundation

/// The common core of the two shells' `serializeState()` implementations —
/// every TestAgent observable keyed off state already represented by a shared
/// FaunaKit/FFI type (`SessionState`, `FaunaClient`, `ConversationsSession`,
/// `FfiFeedManager`) rather than the two platforms' own `AppState`/`MacAppState`.
///
/// Lives in FaunaKit so macOS + iOS cannot drift (priority #1/#2) — was ~90
/// lines duplicated verbatim in `FaunaApp.swift`/`FaunaMacApp.swift` until this
/// harvest pass found it. Takes the
/// already-shared objects directly rather than `AppState`/`MacAppState` (no
/// common protocol between them) — the same shape `ConversationsSendTestCommand`
/// takes `vm: ConversationsVM` instead of the app's whole state.
///
/// **Not here:** every platform-divergent observable — `nav` (tab vs. sidebar
/// shells), `photo_backup` (iOS gates on `#if os(iOS)` + a different source
/// property than macOS's own engine), macOS-only `reopens_handled`/
/// `spawned_instances`/`webdav_serve_reply`, and the `data`/
/// `machine_method_result` keys each shell still computes itself. Each shell
/// merges those into the dict this returns; dictionary key order carries no
/// meaning, so the merge order need not match `serializeState`'s original
/// source order.
@MainActor
public enum AppStateObservables {
    public static func commonState(
        session: SessionState,
        isAdmin: Bool,
        liveClient: FaunaClient?,
        conversationsSession: ConversationsSession?,
        feedManager: FfiFeedManager?,
        devicesMachine: DevicesMachine?,
        inboxMode: String?
    ) -> [String: Any] {
        var state: [String: Any] = [:]

        state["session"] = [
            "authenticated": session.isAuthenticated,
            "node_url": session.nodeUrl as Any,
            "actor_id": session.actorId as Any,
            "secret_hex": session.secretHex as Any,
            "handle": session.handle as Any,
            "device_id": session.deviceId as Any,
            "is_admin": isAdmin,
        ]

        #if DEBUG
        // Convention 14's `barrier` observables — `BarrierTestCommand` is
        // itself `#if DEBUG`, hence the guard rather than a bare merge.
        state.merge(BarrierTestCommand.stateFragment) { _, new in new }
        #endif

        // Convention 14's counter observables — see `AutomationCounters.swift`
        // for why these two are neither `Testing/`-scoped nor `#if DEBUG`.
        state["session_generation"] = SessionGeneration.count
        state["activation_gestures"] = ActivationGestures.count

        // Convention 14's `alert_sweep_passes` observable — a straight
        // read off the shared `CriticalAlerts` registry.
        state["alert_sweep_passes"] = [
            "started": criticalAlertsRegistry().sweepPassesStarted(),
            "completed": criticalAlertsRegistry().sweepPassesCompleted(),
        ]

        // Convention 14's `mls_folded_commits` + `conv_receive_cycles`
        // observables — UniFFI reads off the logged-in
        // `ConversationsSession`, JSON-decoded because `state` is itself
        // re-encoded to JSON at the wire boundary (a raw JSON string would
        // double-encode). No session (never logged in) publishes neither key —
        // convention 11.
        if let conversationsSession {
            if let data = conversationsSession.mlsFoldedCommitsJson().data(using: .utf8),
               let folded = try? JSONSerialization.jsonObject(with: data) {
                state["mls_folded_commits"] = folded
            }
            if let data = conversationsSession.convReceiveCyclesJson().data(using: .utf8),
               let cycles = try? JSONSerialization.jsonObject(with: data) {
                state["conv_receive_cycles"] = cycles
            }
        }

        // Convention 14's `account_pump_cycles` observable — the SHARED
        // `fauna_client_account_runtime` JSON; do not invent a per-platform
        // counter. No live client yet returns the shared-Rust contract's own
        // pre-session shape (`account_pump_cycles_json`'s `None` branch)
        // rather than omitting the key — matching windows'
        // `AccountPumpCyclesForSerialization` (apple lacked this fallback,
        // which made the pre-login `account_runtime_role_or_skip` capability
        // probe misread "not logged in yet" as "no account-store leg").
        if let json = liveClient?.api.accountPumpCyclesJson(),
           let data = json.data(using: .utf8),
           let cycles = try? JSONSerialization.jsonObject(with: data) {
            state["account_pump_cycles"] = cycles
        } else {
            state["account_pump_cycles"] = [
                "started": 0, "completed": 0, "runtime": false, "holder": false,
            ] as [String: Any]
        }

        // Convention 14's `feed_reloads` observable — a UniFFI read
        // off the live `FeedVM.manager`. No manager (never authenticated)
        // publishes no key.
        if let data = feedManager?.feedReloadsJson().data(using: .utf8),
           let reloads = try? JSONSerialization.jsonObject(with: data) {
            state["feed_reloads"] = reloads
        }

        // The Devices/Folders refresh barrier (`fauna_e2e_agent::
        // DEVICES_REFRESHES_KEY`, the `feed_reloads` twin) — a UniFFI read off
        // the session's one `DevicesMachine` (`DevicesMachineVM`, app-scene
        // level). The zero triple before the machine exists is the legitimate
        // "none yet"; an absent key would tell the witness apple has no leg.
        if let data = devicesMachine?.refreshesJson().data(using: .utf8),
           let refreshes = try? JSONSerialization.jsonObject(with: data) {
            state["devices_refreshes"] = refreshes
        } else {
            state["devices_refreshes"] = ["started": 0, "completed": 0, "committed_gen": 0]
        }

        // Convention 14's `serving_enablement` observable — a UniFFI free-function read (`fauna_e2e_agent::
        // SERVING_ENABLEMENT_KEY`'s JSON text; no live client needed, unlike
        // `feed_reloads` above), so it is published unconditionally — the
        // per-actor run-record shape (`{started, completed, runs: [...]}`)
        // starts empty pre-auth rather than absent, and `runs` is what apple's
        // `MailEnableGlue.applyPostClaimServingEnablement` leg now feeds.
        if let data = servingEnablementJson().data(using: .utf8),
           let enablement = try? JSONSerialization.jsonObject(with: data) {
            state["serving_enablement"] = enablement
        }

        #if DEBUG
        // Every new-message OS banner this app process actually raised, plus the
        // diff-tick counters that make a NEGATIVE read of that list sound — the
        // witness for `conversations` outcome 11 (`fauna_e2e_agent::
        // MESSAGE_BANNERS_KEY`, which owns the contract). Recording and JSON both
        // live in shared Rust, so this is a passthrough read like the observables
        // above, never an apple tally — identical on linux and tui.
        //
        // Published UNCONDITIONALLY, pre-auth included: the key's contract makes
        // absent (`null`) mean "this app has no firing leg at all", distinct from
        // `{"started": 0, …}`, so publishing only once a session exists would tell a
        // reader apple was unbuilt for the whole pre-login window. `#if DEBUG` is the
        // apple pairing for the `test-helpers` FFI flavour the free function lives in
        // (convention 15 — `mac-debug`/`apple-ffi-test` build it, release does not),
        // the same gate `BarrierTestCommand.stateFragment` above carries.
        if let data = messageBannersJsonText().data(using: .utf8),
           let banners = try? JSONSerialization.jsonObject(with: data) {
            state["message_banners"] = banners
        }
        #endif

        // The cross-app connection barrier's observable. The
        // boolean is `connectionIsOnline`, never `word == "connected"` — the
        // gate's polarity is asymmetric on purpose. No live client publishes
        // no key — convention 11.
        if let liveClient {
            let word = connectionStateWord(state: liveClient.connectionState)
            state["connection"] = ["state": word, "online": connectionIsOnline(state: word)]
        }

        #if DEBUG
        // The launch clock this process signs in on — the tui/linux/web `clock`
        // twin (`fauna_e2e_agent::CLOCK_KEY`, which owns the shape:
        // `{"offset_secs", "now_secs"}`) — the wrong-clock launch witness's
        // in-app control that the `FAUNA_E2E_CLOCK_OFFSET_SECS` seed reached
        // this process. `#if DEBUG` is the apple
        // pairing for the `test-helpers` FFI flavour the two getters live in,
        // same gate as `message_banners` above.
        state["clock"] = [
            "offset_secs": launchClockOffsetSecsForTest(),
            "now_secs": launchClockNowSecsForTest(),
        ]

        // The held session bearer's anchored lifetime — the `launch_token` twin
        // (`fauna_e2e_agent::LAUNCH_TOKEN_KEY`; shape `{"expires_in_secs", …}`,
        // owned by `fauna_e2e_contract::launch_token_json`). The UniFFI apps'
        // bearer is `FfiNestClient`'s, so this re-parses its JSON text (the
        // `message_banners` passthrough idiom above). No live client publishes
        // JSON `null` — convention 11 — as does a mint still in flight.
        if let text = liveClient?.api.launchTokenJsonForTest(),
           let data = text.data(using: .utf8),
           let token = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed]) {
            state["launch_token"] = token
        } else {
            state["launch_token"] = NSNull()
        }

        // The loud surfaces' two counters — `connection_reports` (every value
        // the connection indicator received) and `painted_errors` (every error
        // surface a painted frame showed), `fauna_e2e_agent::
        // CONNECTION_REPORTS_KEY` / `PAINTED_ERRORS_KEY`. Shared-Rust counting,
        // the same test-flavor gate as `clock` above.
        state.merge(E2eLoudSurfaces.stateFragment) { _, new in new }
        #endif

        #if DEBUG
        // Convention 17's `region-block-never-silent` counts (`RegionBlockRender`,
        // tui's/linux's `region::block_render_json`): the on-screen items the
        // region blocks, per each surface's verdict, against the block
        // placeholders painted. `#if DEBUG` — the witnesses feeding it are.
        state["region_block_render"] = RegionBlockRender.stateFragment
        #endif

        state["settings"] = [
            "inbox_mode": inboxMode as Any,
        ] as [String: Any]

        // Current UI error/warning/info — `AppMessages` is auto-populated by
        // `ErrorBanner.onAppear`/`onDisappear`.
        state["messages"] = [
            "error": AppMessages.errorForDisplay as Any,
            "warning": AppMessages.warning as Any,
            "info": AppMessages.info as Any,
        ] as [String: Any]

        // Test-agent command reply slots, read straight off the shared
        // `TestAgentReplies` holder rather than taken as per-slot parameters. Both
        // shells used to pass `TestAgentReplies.caldavMailboxReply` through by hand
        // while `webdav_serve_reply` was published by a macOS-only block in
        // `serializeState()` — so iOS answered neither, and that per-shell
        // forget-to-pass is precisely why iOS could not witness
        // `files-in-standard-apps` outcome 2. One read here means a new slot reaches
        // both shells the moment it exists.
        if let caldavMailboxReply = TestAgentReplies.caldavMailboxReply {
            state["caldav_mailbox_reply"] = caldavMailboxReply
        }
        if let webdavServeReply = TestAgentReplies.webdavServeReply {
            state["webdav_serve_reply"] = webdavServeReply
        }

        return state
    }

    /// The DATA half of the two shells' `serializeState()` pair — every
    /// TestAgent `data.*` observable keyed off state already represented by a
    /// shared FaunaKit/FFI type (`ConversationsVM`, `Contact`, `Knock`,
    /// `EventSummary`) rather than the two platforms' own `AppState`/
    /// `MacAppState`. Sibling to `commonState` above — that harvest pass found
    /// `serializeState`'s ~90-line common core; this one found `serializeData`'s
    /// smaller one hiding the same way, inside a function two earlier passes had
    /// already read end-to-end and correctly called "diverges throughout" — true
    /// of the WHOLE function, but not of every line inside it.
    ///
    /// **Not here:** `feed` (macOS's diag block carries four extra keys iOS's
    /// doesn't — genuinely divergent, not a dedup candidate) and `sync` (macOS
    /// reports a real DEBUG-gated sync-agent block; iOS has no sync agent at all
    /// and reports `NSNull()`) — each shell computes those itself and merges the
    /// result into what this returns.
    public static func commonData(
        conversationsVM: ConversationsVM,
        contacts: [Contact],
        knocks: [Knock],
        events: [EventSummary],
        notificationsUnreadCount: Int,
        liveClient: FaunaClient?
    ) -> [String: Any] {
        var data: [String: Any] = [:]

        // The identity succession's pre-switch group sweep — the machine-readable
        // twin of an outcome that renders as ID-less chrome, which is why
        // `test_identity_succession_ceremony.py` reads it as STATE (convention 11's
        // cross-app contract, republished from the shared `SweepStatus::state_json`
        // rather than re-encoded here). Survives the ceremony's own account switch
        // — see `SuccessionHandoff` for why — and is null, never an empty object,
        // when no succession ran on this app run.
        data["succession_sweep"] = SuccessionHandoff.sweepStateForSerialization()

        // The member-side succession report — `data.succession_witness`, the
        // FFI apps' whole state-contract obligation for the witness
        // (`succession-propagation.md` § Implementation status today ). Republished from the shared
        // `fauna_client_recovery::witness::state_json` string rather than
        // re-derived here, same UniFFI-passthrough idiom `commonState` uses for
        // `accountPumpCyclesJson`. Unlike that observable this key is never
        // absent: `null` means "no session yet", distinct from a session whose
        // witness has seen nothing (an empty-but-present report) — only the
        // latter indicts the inbound poll, the whole reason this key exists.
        data["succession_witness"] = successionWitnessForSerialization(
            json: liveClient?.api.successionWitnessStateJson()
        )

        // Conversations — the unified `data.conversation_threads` shape, via the
        // shared JSON passthrough, same UniFFI-mediated idiom as
        // `commonState`'s `mlsFoldedCommitsJson`/`convReceiveCyclesJson` above:
        // re-parse `ConversationsManager.conversationThreadsJson()` rather than
        // re-deriving the row shape in Swift. The legacy SwiftData-backed
        // `data.conversations`/`data.groups` are retired with the legacy DM/
        // Groups views (Session C).
        //
        // Off the MANAGER, not a session: the e2e login path deliberately never
        // activates a real `ConversationsSession` (keeps the `test-helpers` mock
        // backends live), so a session-only read reported an empty list forever
        // regardless of what `inject_inbound_for_test` staged.
        let conversationsManager = conversationsVM.manager
        let conversationsSnapshot = conversationsManager.snapshot()
        if let jsonData = conversationsManager.conversationThreadsJson().data(using: .utf8),
           let threads = try? JSONSerialization.jsonObject(with: jsonData) {
            data["conversation_threads"] = threads
        } else {
            data["conversation_threads"] = []
        }
        // `data.conversation_sort` — the list's active order, which the rows alone
        // cannot name (with nothing unread the unread order renders like
        // latest-activity). Re-parsed off the shared `conversationSortJson()` so no
        // app spells the order names; a bare JSON string, hence `.fragmentsAllowed`.
        if let sortData = conversationsManager.conversationSortJson().data(using: .utf8),
           let sort = try? JSONSerialization.jsonObject(with: sortData, options: [.fragmentsAllowed]) {
            data["conversation_sort"] = sort
        }
        // The manager's current selection (identity, not label) — lets an e2e
        // opener recognize a thread as already-open by id instead of only by
        // clicking its `conversation-item` row.
        data["selected_thread_id"] = conversationsSnapshot.selectedThreadId as Any? ?? NSNull()

        // Contacts
        data["contacts"] = contacts.map { c in
            [
                "peer_id": c.peerId,
                "status": c.status,
                "handle": NSNull(),
            ] as [String: Any]
        }

        // Knocks
        data["knocks"] = knocks.map { k in
            [
                "peer_id": k.sender,
                "summary": k.summary as Any,
                "timestamp": k.createdAt,
            ] as [String: Any]
        }

        // Events
        data["events"] = events.map { e in
            [
                "id": e.id,
                "summary": e.summary,
                "start": e.dtstart,
                "end": e.dtend,
                "rsvp_status": NSNull(),
            ] as [String: Any]
        }

        // Notifications
        data["notifications"] = [
            "unread_count": notificationsUnreadCount,
        ] as [String: Any]

        return data
    }

    /// Parses `APIClient.successionWitnessStateJson()`'s raw JSON into the
    /// `data.succession_witness` value — `NSNull()` on `nil`, the decoded
    /// report object otherwise. A pure function (no `FaunaClient` dependency)
    /// so the no-session case is pinnable directly, the same reason
    /// `SuccessionHandoff.sweepStateForSerialization()` stays a standalone
    /// read rather than inlined at `commonData`'s call site.
    static func successionWitnessForSerialization(json: String?) -> Any {
        guard let json,
              let jsonData = json.data(using: .utf8),
              let witness = try? JSONSerialization.jsonObject(with: jsonData)
        else { return NSNull() }
        return witness
    }

    /// A feed post's `link_previews` state key: every link preview in the body,
    /// in body order, with its state name (`resolving`/`resolved`/`failed` —
    /// the shared `RenderDocument::link_previews` over the FFI
    /// `renderDocumentLinkPreviews` face, render-model.md § D4). The card is
    /// absent while a preview is still resolving too, so this is what lets a
    /// test wait until a preview has FAILED before it reads "no card". One
    /// helper for both shells' `data.feed.posts[]` rows; tui's and linux's key.
    public static func feedPostLinkPreviews(_ document: RenderDocument) -> [[String: String]] {
        renderDocumentLinkPreviews(document: document).map { ["url": $0.url, "state": $0.state] }
    }
}
