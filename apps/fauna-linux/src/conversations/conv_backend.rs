//! Linux runtime wiring for the shared fauna-native conversations receive path.
//!
//! At login (AuthSuccess) this builds a [`ConversationsSession`]
//! (`libs/fauna-conversations`) over linux's existing singleton
//! [`ConversationsManager`] (`conversations::host::manager()` — the surface the
//! GTK UI + the e2e helpers observe), registers the real FaunaMls + SMTP +
//! inbound-mail backends, replenishes the local actor's key packages, and starts
//! the shared [`ConversationsSession::start_receive_loop`] — the one detached
//! receive task that drives BOTH rails (conv welcome/channel + inbound mail) into
//! the manager. This is the same path apple/windows/android reach through the
//! `fauna-ffi` `conversations_session` factory (priority #1/#2 — one native
//! receive path fleet-wide); it replaces linux's former bespoke `conv_backend`
//! FaunaMls loop + `mail_sink::start_inbound_poll` mail loop.
//!
//! All MLS + mail crypto stays in shared Rust (`fauna-mls` + the shared sources);
//! this file is transport glue only (`docs/goal/ui/conversations.md` §
//! Architectural rules #2).
//!
//! The live session is held in [`ACTIVE_SESSION`] so the receive loop's liveness
//! `Weak` stays upgradeable; a later login replaces it, dropping the prior session
//! so its loop exits (the re-injection guard — `start_receive_loop`). The same
//! holder carries the runtime handle the e2e wire-drivers (`e2e_*`) block on.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use fauna_client::NestClient;
use fauna_client_conversations::{
    IndexLeaseSeat, LauncherLocalIndex, MailKeyCache, NestBridgedGlue, NestConversationsRpc,
    NestFolderGate, NestInboxDrainSource, NestMailInboundSource, NestMailIndexLauncher,
    NestMlsReplicaTransport, NestOutboundMailSink, NestSchedulingSink, conv_push_source,
};
use fauna_client_mls_sync::{MlsStateSync, RestoreRetryEnd, orchestration};
use fauna_conversations::backend::RoomSeams;
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_conversations::{
    ConversationsManager, ConversationsSession, KEYPACKAGE_TARGET, ThreadId, TypedAddress,
};
use fauna_core::crypto::BackupKey;
use fauna_core::delegation::ParticipantClass;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::engine::MlsEngine;

/// Quiescence window before the MLS state replica (provider + touched history
/// slices) is re-sealed and uploaded, mirroring the Drafts Sync leg
/// ([`crate::conversations::drafts`]). Long enough to coalesce a burst of
/// engine/store mutations (a send, its inbound acks) into one upload; short
/// enough that a second device sees the change within a couple seconds (design
/// `2026-07-05-mls-cross-device-state-sync-design.md` §5). `save_*_if_changed`
/// dedups an unchanged snapshot, so a tick fired by a non-MLS change is cheap.
const REPLICA_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Process-lived holder for the active conversations session + the e2e runtime.
/// Holding the `Arc<ConversationsSession>` keeps its receive loop's liveness
/// `Weak` upgradeable; [`start_conversations_session`] replaces it on each login,
/// dropping the previous session so its loop exits (no accumulation across the
/// e2e session-cached driver's repeated session re-injection). `runtime` lets the
/// GTK command thread `block_on` the manager's async wire-drivers for the tier_3
/// real-wire e2e; `ready` gates the `data.conv_real_backend_active` field the e2e
/// polls after login.
static ACTIVE_SESSION: Mutex<ActiveSession> = Mutex::new(ActiveSession {
    session: None,
    runtime: None,
    ready: false,
    succession_witness: None,
    peer_anchor_harvest: None,
});

struct ActiveSession {
    session: Option<Arc<ConversationsSession>>,
    runtime: Option<tokio::runtime::Handle>,
    ready: bool,
    /// The member-side succession witness this login registered — the second
    /// handle to the object the session holds behind a `dyn`, kept because a
    /// `dyn SuccessionWitness` could not answer `observation()`. `None` when the
    /// identity secret would not parse, which is exactly the state
    /// [`witness_state_json`] must report rather than hide.
    succession_witness: Option<Arc<LinuxChainWitness>>,
    /// The producer half's report — what the peer-anchor harvest sweep managed
    /// per peer. Beside the witness rather than inside it because *absence of an
    /// entry* is its own reading (that peer was never attempted).
    peer_anchor_harvest: Option<Arc<fauna_client_recovery::harvest::HarvestLog>>,
}

/// linux's concrete in-group succession witness — the shared policy over the
/// shared anchors and the shared native dialer. tui's twin (`TuiChainWitness`)
/// is the same three types; the alias exists for the same reason its does, so
/// [`ActiveSession::succession_witness`] can call `observation()`.
pub type LinuxChainWitness = fauna_client_recovery::ChainWitness<
    fauna_client_recovery::ThreadParticipantAnchors,
    fauna_client_recovery::witness::NativeSuccessionChainSource,
>;

/// Build the shared conversations session at login and start the unified receive
/// loop. Called once per AuthSuccess from `app.rs`.
///
/// `self_address` is the logged-in `<handle>@<domain>` (the FaunaMls self-handle
/// and the SMTP `From:`); it seeds the session's live self-address cell,
/// possibly empty until the account cache is populated — in which case the
/// `DataMessage::IdentityRefreshed` handler pushes the resolved address through
/// `ConversationsSession::set_self_address` when the background silent sign-in
/// lands it (`conversations.md` § State & data shape → *Self-address: live,
/// never baked*). `mls_engine` is the **shared** engine from
/// `MlsManager::engine()` (one engine / one `mls_state.db`).
///
/// `secret_hex` is the actor's 32-byte identity seed (same source as
/// [`crate::conversations::drafts::start`]): it derives the `BackupKey` that seals
/// the cross-device MLS **state replica** (`docs/goal/behavior/devices.md`
/// § Cross-device MLS group-state sync, slice 5). On login this leg restores the
/// sealed `provider` + per-channel `history/<ch>` replicas (so a second device of
/// the same identity gains its conversations), injects the device-owned-epoch
/// [`FaunaCommitGate`] + the [`MlsSyncCursor`] resume seam, and debounce-saves the
/// replica after engine/store mutations. A malformed secret disables the plane
/// (single-device fallback — the gate's `None` path is today's behavior).
///
/// `mls_predecessors` is the retired identities' `BackupKey`s
/// (`FaunaClient::predecessor_backup_keys()`, resolved ONCE by the caller and
/// shared with the drafts rails — never a second registry walk) offered to
/// [`MlsStateSync::with_predecessors`], so a successor's `__mls` re-seal barrier
/// inside [`MlsStateSync::load`] actually has keys to re-seal with
/// (`docs/goal/behavior/succession-aftermath.md` § Re-key scope). `tx` feeds the
/// pass's [`MlsStateSync::with_reseal_sink`] progress into the Recovery kit
/// section via `AftermathUpdate::MlsReseal`, mirroring tui's `session.rs`
/// `SuccessionReseal { predecessors, sink }`.
#[allow(clippy::too_many_arguments)]
pub fn start_conversations_session(
    manager: Arc<ConversationsManager>,
    mls_engine: Arc<MlsEngine>,
    nest: Arc<NestClient>,
    self_address: String,
    secret_hex: &str,
    runtime: &tokio::runtime::Handle,
    mls_predecessors: Vec<BackupKey>,
    tx: crate::client::UiSender,
) {
    let self_actor = mls_engine.identity_actor_id();
    let rpc = Arc::new(NestConversationsRpc::new(Arc::clone(&nest)));
    // Wire the home-nest link-preview seam (render-model.md § D4) with the SAME object
    // (`NestConversationsRpc` impls both `ConversationsRpc` and `LinkPreviewRpc`), so a
    // conversation bubble's bare-url `LinkPreview` resolves through the manager.
    manager.set_link_preview_rpc(rpc.clone());
    let room_seams = RoomSeams::from_rpc(&rpc);
    let session = ConversationsSession::from_manager(
        manager,
        mls_engine,
        rpc,
        self_address,
        self_actor,
        // `None` when a test-capable build was told to suppress the push arm
        // (`FAUNA_E2E_SUPPRESS_CONV_PUSH`) to force the durable inbox-apply drain
        // backstop (layer-5 missed-push receive proof). The env read is compiled
        // out of a release build with no `e2e-agent` feature (convention 15), so
        // the production twin always returns the live source.
        conv_push_source(Arc::clone(&nest)),
    );

    // The room plane's four nest-backed seams, the SAME object again, registered
    // as one bundle so no seam can be left out (`RoomSeams`):
    // - the floor-roster REPORT (`conversation-rooms.md` § The floor roster):
    //   after every membership commit this device authors on a governed room,
    //   the backend reports the resulting roster to the room's home nest, which
    //   stores it as the floor roster the custody serve door reads. Unset, every
    //   owed report is tallied and dropped;
    // - the floor-roster READ: the id-keyed handle read that names a room member
    //   this device has never met. Unset, such a member keeps rendering as its
    //   elided actor id;
    // - the community class's GENERATION read (§ The three classes →
    //   *Community*): a community room's content seals under a room generation
    //   key that reaches this device as an X-Wing wrap, and this read fetches it.
    //   Its other half, the account-plane secret that opens the wrap, is
    //   registered at the account-store-ready edge (`crate::account_runtime::install`), because the store is
    //   assembled off the login path. Either unset, a `RoomSealed` record stays
    //   unopened;
    // - the community class's CEREMONY: founding a room through `room.create`,
    //   keying it through `room.publish_generation`, and the governance doors.
    //   Unlike the reads, unset is not a quiet skip: founding refuses by name,
    //   because a room founded on a device that cannot key it is a room nobody
    //   could ever send into.
    session.set_room_seams(room_seams);

    // Outbound mail (send) rail — registered here so a client that RECEIVES before
    // it ever sends still has the SMTP backend `ingest_inbound` needs. The backend
    // shares the session's live self-address cell, so an address that lands after
    // login reaches it through `set_self_address` (the `IdentityRefreshed`
    // handler) — no compose-path re-registration.
    session.register_smtp(Arc::new(NestOutboundMailSink::new(Arc::clone(&nest))));

    // One shared mail-key cache across every mail-keyed consumer this login
    // wires (the two read feeds + the content-index launcher), so the account's
    // mail custody is read once per launch rather than once each.
    let mail_keys = MailKeyCache::new(Arc::clone(&nest), crate::account_runtime::mail_store());

    // Inbound mail read-feeds (INBOX + Sent) — the shared lazily-keyed source
    // (derives the recipient HPKE secret from the mail custody on first poll; a
    // graceful no-op until mail is enabled). Both halves so mail the user
    // sent from another MUA also surfaces in the unified view.
    let (inbox, sent) = NestMailInboundSource::inbox_and_sent_over(
        Arc::clone(&nest),
        Arc::clone(&mail_keys),
        session.manager().refused_changes(),
    );
    session.register_mail_receive(inbox, sent);

    // The bridged rail — one backend for every bridge serving the account,
    // third-party principals and the nest's in-process legs (Nostr) alike
    // (`conversations.md` § Where logic lives → *The `Bridged` adapter*). The
    // shared glue is both seams: it resolves and sends, and reads the rooms
    // and the inbox; it seals to the bridge's key and opens under the same
    // mail-key cache the read feeds above use, so the MSEK stays in shared
    // Rust. The receive loop's ticker and the `conversation_changed` push
    // drive it.
    let bridged = NestBridgedGlue::new(Arc::clone(&nest), Arc::clone(&mail_keys));
    session.register_bridged(bridged.clone(), bridged);

    // Local content-index build (`content-index.md` § Ingest triggers, v1). The
    // launcher resumes this actor's mail index and registers the observer inside
    // `start_receive_loop`'s prologue — *before* the poll task spawns — which is
    // what makes the mailbox re-walk that same launch performs get indexed
    // instead of missed. The MSEK stays inside shared Rust: this glue passes a
    // `NestClient` and never sees a key.
    //
    // The seat puts this builder under the advisory `index` lease
    // (`participants.md` § Coordination primitive → *The `index` kind under the
    // lease*), which is what makes linux the runner-of-record on its own
    // Task-delegation row instead of leaving it reading Waiting-while-running.
    //
    // **`PluggedInDesktop`, always** — linux ships no AC-line monitor either (the
    // one it had went with the in-app upload driver, deleted 2026-07-29), and the
    // unknown-power default across every seat is deliberately *candidate*, so a
    // lone linux seat still builds. Not a knob: nobody would choose it, so it is
    // a constant at the wiring site. Failing to read the device id costs only the
    // coordination — the builder still indexes this launch's mail.
    let lease_seat = crate::sync::device_id()
        .ok()
        .map(|device_id| IndexLeaseSeat {
            device_id,
            class: ParticipantClass::PluggedInDesktop,
            pins: Arc::new(crate::account_runtime::handle_source()),
        });
    let index_launcher =
        NestMailIndexLauncher::new(Arc::clone(&nest), Arc::clone(&mail_keys), lease_seat);
    // The File arm's shared-set key resolver — what lets its reconcile walk render
    // the sealed names of sets shared **with** this actor (`content-index.md`
    // § Ingest triggers, v1 → *The files/media arms are SCOPED*: group-shared sets
    // are included). Injected because `fauna-client-folders`, which owns the
    // resolver, depends on `fauna-client-conversations` under its `mls` feature,
    // so the launcher cannot depend on it back; it derives the *owner* root
    // itself from the identity seed, so this covers only the shared half.
    //
    // The same shared resolver the Media page and `client.rs` already build over
    // the account's folder-key custody. A malformed secret leaves it unwired,
    // which costs shared-set rows and nothing else — they are skipped **without
    // burning the re-index guard**, so a later walk with the keys stages them.
    // …and the account's attested predecessor ids, beside it: the same reader
    // seat then admits a row a retired identity signed with no succession
    // lookup (writer-signed change records, ruling (8)(b) source (ii)).
    index_launcher
        .set_attested_predecessors(crate::attested_predecessor_ids_for_secret_hex(secret_hex));
    if keypair_from_hex(secret_hex).is_some() {
        index_launcher.set_folder_key_resolver(Arc::new(
            fauna_client_folders::NestFolderKeyResolver::new(
                Arc::clone(&nest),
                crate::account_runtime::folder_key_store(),
            ),
        ));
    }
    // Register the query-side resolver on the process-wide `SearchManager`
    // (`crate::search::host`, built in `build_main_window` — always live by
    // AuthSuccess) — the local-arm twin of `set_index_builder_launcher` just
    // below, wiring backend 2's query side the way tui's `attach_local_index`
    // does. A **resolver**, not an arm: registering is synchronous and always
    // possible, so it does not need to wait on the MSEK the way minting an arm
    // would (`content-index.md` § Ingest triggers, v1 → *An arm attaches when
    // its precondition arrives*) — the manager mints on the first query that
    // finds none. `conversations::host::manager()` re-reads the SAME singleton
    // `start_conversations_session`'s own `manager` param was moved from
    // (`ConversationsSession::from_manager` above consumed it), so this needs
    // no extra clone threaded down from the caller.
    if let Some(search_manager) = crate::search::host::manager() {
        search_manager.set_local_index_resolver(LauncherLocalIndex::new(
            Arc::clone(&index_launcher),
            crate::conversations::host::manager(),
        ));
    }
    // The posts trickle chokepoint (`content-index.md` § Ingest triggers, v1 —
    // the posts ruling): a nest-confirmed compose is staged into the local
    // index at once instead of waiting for the next reconcile walk. One clone
    // off the same kept `Arc` the resolver registration above already needed —
    // tui's `session.rs` (right after its own `attach_local_index` call) is the
    // template; the process-wide `crate::feed::host` slot is linux's twin of
    // tui's `app.feed.manager`, live by `AuthSuccess` (`build_main_window`
    // calls `feed::host::init` before any login can complete).
    if let Some(feed_manager) = crate::feed::host::manager() {
        feed_manager.set_post_index_observer(index_launcher.own_post_observer());
        // The room-post seam (`ui/feed.md` § Encryption at rest →
        // *Room-restricted — the app half*), wired here for the same reason:
        // the feed opens and seals room-restricted posts through this session,
        // which alone holds a room's keys and knows which rooms there are.
        // Without it every room post stays locked and no room is offered.
        feed_manager.set_room_post_keys(session.clone());
        attach_own_rooms_refresh(feed_manager, runtime.clone());
    }
    session.set_index_builder_launcher(index_launcher);

    // Scheduling drain — the mailbox-less CalDAV iMIP rail: the loop drains every
    // `WelcomeChannelKind::Scheduling` channel to this sink, which applies the iMIP
    // to the actor's calendar via the shared `CalDavClient` (caldav-server.md §
    // Server-side auto-schedule, Half-1). Same lazy-MSEK / graceful-no-op shape as
    // the mail source above (the CalDAV store seals under the same `mail.msek`).
    session.register_scheduling_sink(Arc::new(NestSchedulingSink::over(
        Arc::clone(&nest),
        Arc::clone(&mail_keys),
        Arc::downgrade(&session.manager()),
        session.manager().refused_changes(),
    )));

    // Durable inbox-apply backstop — the loop's ticker drains the per-actor
    // `fauna.inbox.*` queue, recovering a Welcome whose best-effort push was missed
    // (client offline at push time): the missed-push delivery guarantee
    // (`api-layers.md` § Inbox & Messaging, layer 3). Holds a `Weak` session to avoid
    // the session→source cycle (the session owns the source via `register_inbox_drain`).
    session.register_inbox_drain(Arc::new(NestInboxDrainSource::new(
        Arc::clone(&nest),
        Arc::downgrade(&session),
    )));

    // Recipient contact-gate for cross-user shared folders (`folders.md` §
    // Sharing — Recipient side: "auto for contacts, knock for strangers"). The
    // receive rail routes a `WelcomeChannelKind::Folder` welcome through this
    // gate, which reads the sharer's contact-status (`fauna.contacts.status`) and
    // returns Auto → join off the chat rail + ack; Knock → leave un-acked (the
    // staged `folder-pending-share`); Suppress → ack-and-drop. WITHOUT it every
    // folder welcome is retained un-acked (the pre-gate safe default,
    // `session.rs` § `register_folder_gate`), so a *contact's* share would wrongly
    // knock. The same shared `NestFolderGate` the FFI factory injects
    // (`fauna-ffi` `nest_client.rs`) — one gate fleet-wide (priority #2).
    session.register_folder_gate(Arc::new(NestFolderGate::new(Arc::clone(&nest))));

    // Member content-key custody ingest (Phase 0 — the read leg; `folders.md` §
    // Sharing): on join and on each rotation-commit receipt, the session fetches
    // the owner's sealed content-key envelope, opens it via the group epoch, and
    // folds the generations into this member's own `fauna.state.folder-keys` custody — so a
    // *member* (not just the owner) can decrypt a shared set's content. The same
    // shared `NestFolderCustodySink` the FFI factory injects (one seam
    // fleet-wide, priority #2). A malformed identity secret simply skips it (the
    // member can list but not decrypt — the pre-Phase-0 behaviour).
    //
    // No app hop rides the ingest: the rotated generation it joins into the
    // account's folder-key custody is itself the custody write the nest nudges
    // the desktop sync agent about (the `state-fleet` nudge), and the agent
    // re-resolves its running engines' keys on it (`on-demand-files.md`
    // § Shared sets on a capability host → *One mechanism*).
    if keypair_from_hex(secret_hex).is_some() {
        session.set_folder_custody_sink(Arc::new(
            fauna_client_folders::NestFolderCustodySink::new(
                Arc::clone(&nest),
                crate::account_runtime::folder_key_store(),
            ),
        ));
    }

    // The ACCOUNT-custody ceremony sink (T16 — a different plane from the
    // folder custody above): received offers / accepts / delivers / A7 receipts
    // are captured into `fauna.state.custody-ceremony`, and each moved ceremony schedules a drive
    // pass. The same shared registration the FFI factory makes (priority #2);
    // without it no offer ever reaches the consent card and no receipt folds.
    // The Devices page re-reads the facet on its own map edge, so there is no
    // repaint nudge to deliver.
    if let Some(keypair) = keypair_from_hex(secret_hex) {
        fauna_client_custody::register_ceremony_sink(
            &session,
            Arc::clone(&nest),
            *keypair.secret_bytes(),
            crate::account_runtime::handle,
            Arc::new(|| {}),
        );
    }

    // Cross-device MLS state-sync plane (`docs/goal/behavior/devices.md`
    // § Cross-device MLS group-state sync, slice 5). The `MlsStateSync` ctor is
    // non-async (only `load()` touches the wire); build it here so the debounce
    // autosave below can hold it, and hand it to the spawned task for the async
    // restore + gate/cursor wiring (which must complete *before* the first poll).
    // A malformed identity secret disables the plane — the client stays
    // single-device (the gate's `None` path is byte-for-byte today's behavior).
    //
    // `.with_predecessors` turns the post-succession `__mls` re-seal from a dead
    // barrier into a real one: empty for every identity
    // that never succeeded, so the ordinary path pays nothing. `.with_reseal_sink`
    // reports the pass's progress through `AftermathUpdate::MlsReseal` the same
    // way `succession_aftermath.rs`'s other legs do, so `recovery-kit-mls-reseal-status`
    // stops reading empty.
    let mls_sync = keypair_from_hex(secret_hex)
        .map(|keypair| Arc::new(build_mls_sync(&nest, &keypair, mls_predecessors, tx)));
    match &mls_sync {
        // The plane exists from here, so no key-package mint may publish until
        // `wire_mls_state_sync` injects its save-before-publish seam (or gives up).
        Some(_) => session.backend().expect_replica_restore(),
        None => tracing::warn!(
            "mls-sync: malformed identity secret; cross-device conversation sync disabled"
        ),
    }

    // Debounced replica autosave (design §5) — rides the same manager observer as
    // Drafts Sync, so every engine/store mutation (send, inbound, membership)
    // schedules a re-seal + upload after `REPLICA_DEBOUNCE` of quiescence. The
    // `MlsStateSync` launch gate no-ops every save until the spawned `load()`
    // completes, so attaching it here (before the restore) is safe.
    if let Some(sync) = &mls_sync {
        attach_replica_autosave(
            session.manager(),
            session.backend(),
            Arc::clone(sync),
            runtime.clone(),
        );
    }

    // In-group succession witness (`succession-aftermath.md` § Propagation →
    // MLS groups). Without it every `GroupMetaMessage::Succession` degrades to
    // the bare add — a member sees "someone added a stranger" where a peer
    // actually recovered their account. The policy (cached head first, then the
    // anchored walk to the old identity's home nest), the anchors and the
    // native dialer are all shared; linux supplies only the thread store the
    // anchors read handles from and the two handles the state provider reads.
    // The same three lines tui's `conv_backend` runs.
    // The anchors' durable store (`fauna.state.peer-anchors`) is lent late,
    // through the manager, by the shared store-ready registration
    // (`account_runtime`'s `conversation_seams::wire`) — nothing to hand in
    // here, so nothing to forget.
    let witness = {
        let witness = Arc::new(fauna_client_recovery::ChainWitness::new(
            fauna_client_recovery::ThreadParticipantAnchors::new(
                // `Weak`, never the manager itself: the witness is parked on
                // the backend this manager owns, so a strong handle here is a
                // cycle that outlives the session (the type's own doc).
                Arc::downgrade(&session.manager()),
            ),
            fauna_client_recovery::witness::NativeSuccessionChainSource,
        ));
        session.set_succession_witness(
            Arc::clone(&witness) as Arc<dyn fauna_conversations::backend::SuccessionWitness>
        );
        Some(witness)
    };

    // The peer-anchor harvest sweep — the producer half of the member-path
    // anchor (`identity-succession.md` § The succession statement → *the
    // peer-profile harvest*). A Welcome-joined roster row carries no handle, so
    // without this sweep the witness registered above has no anchor for exactly
    // the peers the in-group statement is about. Runs on the ordinary read path
    // (a roster sweep), never at verify time — harvest rule 4. Injected rather
    // than spawned here: this fn is called from the GTK thread, which holds no
    // tokio context, and `start_receive_loop` launches it first in its prologue
    // inside the login runtime (`set_peer_anchor_sweep_launcher`).
    let harvest_log = {
        let log: Arc<fauna_client_recovery::harvest::HarvestLog> = Default::default();
        session.set_peer_anchor_sweep_launcher(Arc::new(
            fauna_client_recovery::harvest::PeerAnchorSweep::new(
                Arc::downgrade(&session),
                Arc::clone(&nest),
                Arc::clone(&log),
                fauna_client_recovery::harvest::PEER_ANCHOR_SWEEP_INTERVAL,
            ),
        ));
        Some(log)
    };

    // Publish the session before starting the loop so its liveness `Weak` stays
    // upgradeable; dropping any prior session here stops that login's loop.
    {
        let mut g = ACTIVE_SESSION.lock().unwrap();
        g.session = Some(Arc::clone(&session));
        g.runtime = Some(runtime.clone());
        g.ready = true;
        g.succession_witness = witness;
        g.peer_anchor_harvest = harvest_log;
    }

    // Owned captures for the launch-time folder removal recovery below: the login
    // task drops + re-upgrades the session across the restore await, so it rebuilds
    // the author from these rather than borrowing this frame.
    let nest_for_resume = Arc::clone(&nest);
    let secret_for_resume = secret_hex.to_string();

    runtime.spawn(async move {
        // (Login-time key-package replenish is session-owned: `start_receive_loop`
        // below runs it AFTER the replica restore — a restore swaps the engine's
        // provider storage, so a package minted before it would lose its private
        // init key; `devices.md` § Cross-device MLS group-state sync.)
        //
        // Restore the cross-device MLS state replica and wire the
        // device-owned-epoch gate + cursor seam BEFORE the first poll (design §5:
        // restore-before-first-poll — so the loop resumes each channel from its
        // restored watermark, not seq 0). A no-op when the plane is disabled
        // (malformed secret): the receive loop runs single-device. A transient
        // load failure retries with backoff inside, so this task holds only
        // `Weak` session handles across the await — `ACTIVE_SESSION` owns the
        // session, and a re-injection replacing it (the e2e session-cached
        // driver) must end the retry rather than leave a pinned zombie task.
        if let Some(sync) = mls_sync {
            let weak_session = Arc::downgrade(&session);
            let backend = Arc::downgrade(&session.backend());
            let manager = Arc::downgrade(&session.manager());
            drop(session);
            wire_mls_state_sync(backend, manager, sync).await;
            let Some(session) = weak_session.upgrade() else {
                return;
            };
            // Launch-time crash recovery for interrupted folder member removals —
            // AFTER the restore, because the restore is what makes the removal gate
            // live (see the fn's doc).
            resume_folder_removals(&nest_for_resume, &secret_for_resume, &session).await;
            session.start_receive_loop().await;
        } else {
            session.start_receive_loop().await;
        }
    });
}

/// Replenish the local actor's one-time key-package pool to [`KEYPACKAGE_TARGET`]
/// through the durable manager surface — mint on the session `MlsEngine` and
/// `notify()` the replica autosave, the SAME path login
/// (`ConversationsSession::start_receive_loop`) and web
/// (`manager.ensureKeypackages`) drive. Fire-and-forget from the GTK thread onto
/// the login session's runtime.
///
/// This is the ONLY correct native replenish path. A raw
/// `MlsManager::generate_key_packages` + `client.publish_key_packages_real` mints
/// on the shared engine but never ticks the autosave observer, so a later
/// mid-session `MlsStateSync::resync_provider` — or a relaunch — restores an older
/// replica over the fresh private init keys and wipes them, stranding every peer
/// that fetched the published package (`docs/goal/behavior/devices.md`
/// § Cross-device MLS group-state sync; pinned by fauna-conversations
/// `keypackage_minted_via_ensure_keypackages_survives_a_provider_swap`). No-op
/// before login (no session runtime yet) or on a client with no FaunaMls backend
/// registered (`ensure_keypackages` returns `Ok(0)`).
pub fn replenish_key_packages() {
    let Some(runtime) = ACTIVE_SESSION.lock().unwrap().runtime.clone() else {
        return;
    };
    let manager = crate::conversations::host::manager();
    runtime.spawn(async move {
        match manager.ensure_keypackages(KEYPACKAGE_TARGET).await {
            Ok(n) if n > 0 => tracing::info!("replenished {n} key packages"),
            Ok(_) => {}
            Err(e) => tracing::error!("ensure_keypackages: {e}"),
        }
    });
}

/// Decode a 32-byte hex identity seed into an [`ActorKeypair`] (mirrors
/// `drafts::keypair_from_hex`; both delegate to the canonical
/// `ActorKeypair::from_secret_hex`). `None` on a malformed secret.
fn keypair_from_hex(secret_hex: &str) -> Option<ActorKeypair> {
    ActorKeypair::from_secret_hex(secret_hex).ok()
}

/// Assemble the `__mls` cross-device state-sync plane — factored out of
/// [`start_conversations_session`] so the call-site pin test below can call it
/// without building a whole session (mirrors `conversations::drafts::build_sync`
/// / `feed::drafts::build_sync` / `views::events::drafts::build_sync`).
///
/// `.with_predecessors` is a barrier, not a head start: an empty-predecessors
/// identity (the overwhelmingly common case) pays nothing, and a successor's
/// re-seal runs inside [`MlsStateSync::load`] itself, ahead of the first read
/// that would fail on it. `.with_reseal_sink` reports the pass's progress
/// through `AftermathUpdate::MlsReseal`.
fn build_mls_sync(
    nest: &Arc<NestClient>,
    keypair: &ActorKeypair,
    mls_predecessors: Vec<BackupKey>,
    tx: crate::client::UiSender,
) -> MlsStateSync {
    MlsStateSync::new(
        Box::new(NestMlsReplicaTransport::new(Arc::clone(nest))),
        keypair,
    )
    .with_predecessors(mls_predecessors)
    .with_reseal_sink(Box::new(move |progress| {
        tx.send(crate::app::UiMessage::Data(
            crate::app::DataMessage::AftermathProgress(
                crate::settings::recovery_kit::AftermathUpdate::MlsReseal(progress),
            ),
        ));
    }))
}

/// Restore the cross-device MLS state replica into the just-built session and
/// inject the device-owned-epoch plane — the linux trigger for the shared
/// [`orchestration::restore_and_wire_with_retry`] (which owns the restore +
/// gate/cursor assembly and the transient-failure backoff, design §5 + launch
/// resilience). Runs in the login task **before** `start_receive_loop` (which
/// replenishes key packages only after it — a mint before the seam is refused).
/// A *permanent* load failure (e.g. a replica the
/// nest refuses to serve, or one that does not decode) logs and leaves the client single-device
/// (the un-injected gate is today's optimistic behavior) rather than aborting
/// the receive loop; a transient one retries until the nest is reachable or the
/// session is torn down (the `Weak` handles — see the caller).
async fn wire_mls_state_sync(
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
    sync: Arc<MlsStateSync>,
) {
    match orchestration::restore_and_wire_with_retry(sync, backend, manager).await {
        RestoreRetryEnd::Wired(restored) => tracing::info!(
            "mls-sync: cross-device plane wired ({restored} channel(s) restored from replica)"
        ),
        RestoreRetryEnd::Failed(e) => {
            tracing::warn!("mls-sync: replica load failed ({e}); staying single-device")
        }
        RestoreRetryEnd::SessionDropped => {
            tracing::debug!("mls-sync: session dropped during launch retry")
        }
    }
}

/// Re-drive every crash-staged folder member removal at launch — linux's leg
/// of the shared
/// [`fauna_client_folders::FolderRemovalResume`] hook body (the tokio legs —
/// the FFI factory + tui — hand the same hook to the shared launcher; linux
/// drives it here because its restore trigger is its own).
///
/// **Ordering is load-bearing:** this runs *after* [`wire_mls_state_sync`],
/// because the removal gate **is** the session backend and the restore is what
/// injects its `CommitGate` — the hook's doc (`launch_resume.rs`) carries the
/// full rationale, including why the restore's failed path still resumes safely.
async fn resume_folder_removals(
    nest: &Arc<NestClient>,
    secret_hex: &str,
    session: &Arc<ConversationsSession>,
) {
    let Some(seed) = fauna_core::hex32::decode(secret_hex).ok() else {
        return;
    };
    // The recording device (the one the serve walk and the Media page record
    // under) also resumes an interrupted served-set walk in the same pass
    // (`webdav-server.md` § Key model (c)).
    fauna_client_folders::FolderRemovalResume::new(
        Arc::clone(nest),
        seed,
        crate::account_runtime::folder_key_store(),
        crate::account_runtime::mail_store(),
        crate::account_runtime::ledger_seam(),
    )
    .with_recording_device(crate::sync::device_id().ok().map(hex::encode))
    .with_predecessors(crate::attested_predecessor_ids_for_secret_hex(secret_hex))
    .run(session.backend())
    .await;
}

/// Attach the debounced MLS state-replica autosave — the save twin of
/// [`crate::conversations::drafts::attach_autosave`]. A manager observer ticks on
/// every state change; a generation-counter debounce coalesces a burst into one
/// upload after `REPLICA_DEBOUNCE` of quiescence, snapshots the provider + every
/// bound channel's history on the GTK main thread (where the engine/manager state
/// lives), then seals + uploads off-thread on the tokio runtime. The loop ends
/// when `manager.clear_observers()` (session rebuild) drops the sender, so the
/// autosave retires on logout/re-login exactly like Drafts Sync.
///
/// **This debounce is the steady-state coalescer, NOT the durability guarantee.**
/// The `ThreadStore` is RAM-only and is *seeded from* the replica at launch, so
/// anything that misses durable storage before a quit is gone — and a sender cannot
/// MLS-decrypt its own application messages, so an own message is user-irrecoverable.
/// What closes that window is `devices.md` § Durability rules **Rule 3
/// (durable-before-done)**: `manager::send` (shared Rust, all six legs) awaits a
/// `history/<ch>` CAS-save after appending the own message, through the
/// `HistoryPersist` seam `restore_and_wire` injects — so a quit at any moment after
/// the send action returns loses nothing, and this debounce only coalesces the
/// remaining (log-reconstructible or retryable) churn.
fn attach_replica_autosave(
    manager: Arc<ConversationsManager>,
    backend: Arc<FaunaMlsBackend>,
    sync: Arc<MlsStateSync>,
    runtime: tokio::runtime::Handle,
) {
    let rx = crate::conversations::observer::attach(&manager);
    let generation = Rc::new(Cell::new(0u64));
    crate::async_helper::spawn_wake_loop(rx, move || {
        arm_replica_debounce(&generation, &manager, &backend, &sync, &runtime);
        glib::ControlFlow::Continue
    });
}

/// Re-read the composer's rooms (`FeedSnapshot::own_rooms`) on every
/// conversations-plane change (`ui/feed.md` § Encryption at rest →
/// *Room-restricted — the app half*): the list is a projection of that plane,
/// so a room joined, bound or left reaches `compose-gate-tier-select` without
/// re-entering the feed — tui's `ConversationsChanged` arm is the template.
/// Idempotent: the manager notifies only when the list changed. One observer
/// per login, as [`attach_replica_autosave`]'s.
fn attach_own_rooms_refresh(
    feed: Arc<crate::feed::host::LinuxFeedManager>,
    runtime: tokio::runtime::Handle,
) {
    let rx = crate::conversations::observer::attach(&crate::conversations::host::manager());
    crate::async_helper::spawn_wake_loop(rx, move || {
        let feed = Arc::clone(&feed);
        runtime.spawn(async move { feed.refresh_own_rooms().await });
        glib::ControlFlow::Continue
    });
}

/// (Re)arm the one-shot replica-save debounce for the current mutation burst —
/// the MLS-state twin of `drafts::arm_debounce`, built on the same shared
/// [`crate::debounce::arm_generation_debounce`] timer primitive (not
/// drafts-shaped, so it doesn't go through `drafts_autosave::DraftsHost`).
/// The snapshot/upload split lives in the shared [`orchestration`] helper;
/// this owns only the GTK debounce + the main-thread→runtime hand-off.
fn arm_replica_debounce(
    generation: &Rc<Cell<u64>>,
    manager: &Arc<ConversationsManager>,
    backend: &Arc<FaunaMlsBackend>,
    sync: &Arc<MlsStateSync>,
    runtime: &tokio::runtime::Handle,
) {
    let manager = Arc::clone(manager);
    let backend = Arc::clone(backend);
    let sync = Arc::clone(sync);
    let runtime = runtime.clone();
    crate::debounce::arm_generation_debounce(generation, REPLICA_DEBOUNCE, move || {
        // Snapshot on the GTK main thread (engine + manager state lives here);
        // seal + upload off-thread on the runtime.
        let snapshot = orchestration::snapshot_replica(&backend, &manager, &sync);
        runtime.spawn(async move {
            if let Err(e) = orchestration::save_snapshot(&sync, &snapshot).await {
                tracing::warn!("mls-sync: replica autosave failed: {e}");
            }
        });
    });
}

// ── E2e real-wire drivers ─────────────────────────────────────────────────────
//
// Test-only surface. The tier_3
// `test_fauna_mls_real_roundtrip.py` drives the manager's async wire-drivers from
// the GTK bridge command thread, with the API-tier peer's `actor_id` injected
// directly (the `resolve_address` handle→actor chain stays deferred — see
// `backends/fauna_mls.rs::resolve_address`). Reached via the `conversations_*`
// bridge commands in `main.rs`. The real FaunaMls backend is now wired at login
// for every app (no mock-gate), so these just need the runtime handle the
// session build stashed.

/// Whether the conversations session has been built (set at AuthSuccess by
/// [`start_conversations_session`]). The tier_3 round-trip polls
/// `data.conv_real_backend_active` until this is true before driving a real send.
pub fn is_e2e_real_active() -> bool {
    ACTIVE_SESSION.lock().unwrap().ready
}

/// Readiness probe issued by `conversations_enable_real_faunamls`. The real
/// FaunaMls backend is wired unconditionally at login now, so there is nothing to
/// activate — the e2e simply waits on [`is_e2e_real_active`] (returned here for
/// the caller's convenience).
pub fn request_e2e_activation() -> bool {
    is_e2e_real_active()
}

/// `conversations_disable_real_faunamls` teardown: wipe the manager's threads so a
/// later snapshot conversations test (collection order can put one after the
/// real-wire test, sharing the session-cached app) starts clean. There is no mock
/// to restore now — every app drives the real backend.
///
/// Gated like the agent itself (`docs/goal/architecture/testing.md` convention 15):
/// it drives `clear_for_test`, which is absent from a release build, and its only
/// caller — `handle_test_command`'s `conversations_disable_real_faunamls` arm — is
/// already behind the same cfg, so no no-op twin is needed.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn disable_e2e_real_backend() {
    crate::conversations::manager().clear_for_test();
}

/// The runtime handle stashed at login, for `block_on`-ing the manager's async
/// wire-drivers from the GTK command thread.
fn e2e_runtime() -> Option<tokio::runtime::Handle> {
    ACTIVE_SESSION.lock().unwrap().runtime.clone()
}

/// The active [`ConversationsSession`] (the one started at AuthSuccess), if any.
/// The organizer scheduling dispatch (`client::invite_to_event`) clones it to back
/// the mailbox-less WS-RPC iMIP rail (`NestImipDispatch` →
/// `ConversationsSession::deliver_scheduling_imip`); `None` only before login /
/// after logout, in which case the invite falls back to email-only.
pub fn active_session() -> Option<Arc<ConversationsSession>> {
    ACTIVE_SESSION.lock().unwrap().session.clone()
}

/// The member side of a succession as a driver reads it — the field reads that
/// feed the shared renderer (`fauna_client_recovery::witness::state_json`,
/// which owns the shape and the reading order). The tui `witness_state_json`
/// twin, published as `data.succession_witness`.
///
/// `null` until a login has registered a witness. Every read here is a field
/// read off objects this process already holds — no round trip, because this
/// runs on the agent's ack path (e2e convention 11's second corollary), and on
/// linux that path is the GTK main thread.
pub fn witness_state_json() -> serde_json::Value {
    // Clone the three handles out and drop the guard before touching any of
    // them: `ACTIVE_SESSION` is taken by the login path too, so nothing that
    // could block belongs inside it.
    let (witness, harvest, session) = {
        let g = ACTIVE_SESSION.lock().unwrap();
        (
            g.succession_witness.clone(),
            g.peer_anchor_harvest.clone(),
            g.session.clone(),
        )
    };
    let (Some(witness), Some(harvest)) = (witness, harvest) else {
        return serde_json::Value::Null;
    };
    let counts = session
        .map(|s| s.backend().succession_statement_counts())
        .unwrap_or_default();
    fauna_client_recovery::witness::state_json(&witness.observation(), &harvest, &counts)
}

fn parse_actor_hex(actor_id_hex: &str) -> Result<ActorId, String> {
    ActorId::from_hex_labeled(actor_id_hex)
}

/// Block on `fut` using the stashed runtime. The GTK command thread is not a
/// tokio worker, so `Handle::block_on` is safe here; nest round-trips are
/// localhost-fast in the e2e.
fn block_on_e2e<F: std::future::Future>(fut: F) -> Result<F::Output, String> {
    let rt = e2e_runtime().ok_or("e2e runtime not available (login not complete)")?;
    Ok(rt.block_on(fut))
}

/// `conversations_accept_recipient`: the agent's twin of the GUI picker's
/// on-accept handler — **probe first, then commit**.
///
/// The production control (`views/conversations/detail.rs`, both the new-thread
/// and add-participant pickers) runs `resolve_recipient().await` and only then
/// `accept_current_recipient_chip()`. The agent used to call the commit alone,
/// which quietly made a whole class of recipients undrivable: without the probe
/// the only address available to commit is the format-only parse, and
/// `try_parse_typed_address` **cannot produce `TypedAddress::Fauna`** by design
/// (it needs an `ActorId` only the backend probe supplies). So a typed Fauna
/// handle or 64-hex actor id committed *no chip at all* over the driver, while
/// working fine for a real user — an agent that under-honours the command it was
/// given, the mirror of the over-honouring `select` case
/// (`e2e-conventions.md` convention 11's twin rule).
///
/// Falls back to the bare commit when no e2e runtime exists yet (pre-login, and
/// the mock-backend tests): those only ever commit Email-shaped addresses, which
/// the synchronous parse already handles.
pub fn e2e_accept_recipient() -> bool {
    let m = crate::conversations::manager();
    if let Some(rt) = e2e_runtime() {
        let m_resolve = m.clone();
        rt.block_on(async move { m_resolve.resolve_recipient().await });
    }
    m.accept_current_recipient_chip()
}

/// `conversations_real_resolve_send_new`: start a new FaunaMls conversation by
/// **resolving** the typed `recipient` through the real backend probe (no
/// `actor_id` injection) and send `body`. Types `recipient` into the new-thread
/// picker, drives `resolve_recipient` (which probes
/// `fauna.conversations.keypackage.count` to promote a 64-hex actor id to
/// `TypedAddress::Fauna`), commits the resolved chip, then `send_new_thread`
/// bootstraps the group (fetch key package → create MLS group → deliver Welcome
/// → post Application envelope). The end-to-end proof that the recipient picker
/// resolves a real actor — the same `resolve_recipient` →
/// `accept_current_recipient_chip` path the GUI on-Enter handler drives. Returns
/// the new thread id.
pub fn e2e_resolve_send_new(recipient: &str, body: &str) -> Result<Option<String>, String> {
    let m = crate::conversations::manager();
    m.start_new_conversation();
    m.set_new_thread_recipient_input(recipient.to_string());

    // Phase markers. This command `block_on`s five nest round-trips **on the GTK
    // main thread**, so a hang here stops the app acking anything at all — and a
    // no-ack is the least diagnosable failure the harness has (cost three runs establishing only that it was not a budget). These
    // lines make the app's own stderr name the phase it died in; the driver
    // surfaces that tail on an ack timeout (`drivers/linux.py` + `http_bridge`).
    tracing::info!("[e2e] resolve_send_new: resolve_recipient begin");
    let m_resolve = m.clone();
    block_on_e2e(async move { m_resolve.resolve_recipient().await })?;
    tracing::info!("[e2e] resolve_send_new: resolve_recipient done");
    if !m.accept_current_recipient_chip() {
        return Err(format!(
            "recipient '{recipient}' did not resolve to a chip (not a reachable Fauna actor?)"
        ));
    }

    m.set_new_thread_body(body.to_string());
    tracing::info!("[e2e] resolve_send_new: send_new_thread begin");
    let tid = block_on_e2e(async move { m.send_new_thread().await })?.map_err(|e| e.to_string())?;
    tracing::info!("[e2e] resolve_send_new: send_new_thread done");
    Ok(tid.map(|t| t.0))
}

/// `conversations_real_send`: send `body` on an existing thread (drives the
/// real `send`; a forked-but-unbound group bootstraps lazily here).
pub fn e2e_send(thread_id: &str, body: &str) -> Result<(), String> {
    let m = crate::conversations::manager();
    let tid = ThreadId(thread_id.to_string());
    m.set_compose_body(tid.clone(), body.to_string());
    block_on_e2e(async move { m.send(tid).await })?.map_err(|e| e.to_string())
}

/// `conversations_real_send_attachment`: send `body` plus one attachment on an
/// existing thread, driving the **real** seal + upload path
/// (`ConversationsManager::add_attachment` → `send` → `RailBackend::send`'s
/// `derive_blob_key(epoch_secret)` seal → `ConversationsRpc::blob_put` to the
/// nest's content-addressed `/api/v1/blob`). The cross-engine proof that a
/// FaunaMls attachment round-trips over the real wire: the receiver GETs the
/// sealed blob (`blob_get`), opens it under the message epoch key, and renders
/// `dm-attachment-image`. `docs/goal/ui/conversations.md` § Attachments.
pub fn e2e_send_with_attachment(
    thread_id: &str,
    body: &str,
    filename: &str,
    mime_type: &str,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let m = crate::conversations::manager();
    let tid = ThreadId(thread_id.to_string());
    m.set_compose_body(tid.clone(), body.to_string());
    m.add_attachment(
        tid.clone(),
        filename.to_string(),
        mime_type.to_string(),
        bytes,
    );
    block_on_e2e(async move { m.send(tid).await })?.map_err(|e| e.to_string())
}

/// `conversations_real_add`: add the peer (`actor_id` injected) to `thread_id`.
/// On a bound FaunaMls group this posts the MLS Commit + Welcome; on a 1:1 it
/// forks a fresh group (snapshot-only until its first `e2e_send`). Returns the
/// resulting (possibly new) thread id.
pub fn e2e_add(
    thread_id: &str,
    peer_actor_id_hex: &str,
    peer_handle: &str,
) -> Result<Option<String>, String> {
    let actor_id = parse_actor_hex(peer_actor_id_hex)?;
    let m = crate::conversations::manager();
    let tid = ThreadId(thread_id.to_string());
    m.open_add_participant(tid);
    m.set_add_participant_recipient_input(peer_handle.to_string());
    m.accept_add_participant_chip(TypedAddress::Fauna {
        handle: peer_handle.to_string(),
        actor_id,
    });
    let new_id = block_on_e2e(async move { m.confirm_add_participant().await })?;
    page_error()?;
    Ok(new_id.map(|t| t.0))
}

/// `conversations_real_remove`: remove the peer (`actor_id` injected) from a
/// bound FaunaMls group (posts the MLS Commit; no Welcome). `peer_handle` must
/// match the one used at `e2e_add` so the snapshot removal — which keys on
/// `TypedAddress::display()` (the handle) — drops the right chip; the wire op
/// finds the MLS leaf by `actor_id` regardless.
pub fn e2e_remove(
    thread_id: &str,
    peer_actor_id_hex: &str,
    peer_handle: &str,
) -> Result<(), String> {
    let actor_id = parse_actor_hex(peer_actor_id_hex)?;
    let m = crate::conversations::manager();
    let tid = ThreadId(thread_id.to_string());
    let addr = TypedAddress::Fauna {
        handle: peer_handle.to_string(),
        actor_id,
    };
    block_on_e2e(async move { m.remove_participant(tid, addr).await })?;
    page_error()
}

/// `conversations_real_rename`: rename a bound FaunaMls group (posts the
/// encrypted `GroupMeta::NameChanged` Application envelope).
pub fn e2e_rename(thread_id: &str, new_label: &str) -> Result<(), String> {
    let m = crate::conversations::manager();
    let tid = ThreadId(thread_id.to_string());
    let label = new_label.to_string();
    block_on_e2e(async move { m.rename_thread(tid, label).await })?;
    page_error()
}

/// Turn a page error the gesture just stamped into the `Err` its caller can
/// report. The three membership/label gestures above return `()`/`Option` and
/// signal a failed wire op **only** through the page-error slot, so without this
/// read-back the agent acks success for an op the nest refused — convention 11's
/// swallow, and the one that cost a whole triage pass (a `forbidden` Welcome
/// read downstream as "no Welcome delivered"). The tui twin lives at
/// `apps/fauna-tui/src/conversations/conv_backend.rs::page_error`.
fn page_error() -> Result<(), String> {
    match crate::conversations::manager().page_error_diagnostic() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Build this account's conversations engine — **releasing the role before it
/// builds**, the linux leg of `docs/goal/architecture/account-data-plane.md`
/// § Multi-instance concurrency → *The role is HANDED OVER in-process, never
/// waited out* (2026-08-29).
///
/// linux and tui construct their `ConversationsManager` themselves and so never
/// reach the shared native factory
/// (`fauna_ffi::FfiNestClient::conversations_session*`), which is where that
/// ruling was executed for macOS / iOS / windows / android — the FFI factory's
/// own comment says as much. So linux inherited none of it, and
/// [`start_conversations_session`] below leaves a `FaunaMlsBackend` registered on
/// the process-lifetime singleton for the whole life of the app: the rail holds
/// the engine, the engine holds the role lock beside `mls_state.db`, and neither
/// sign-out nor an account switch drops it (`clear_for_identity_change`
/// deliberately *preserves* backends). Every same-account re-login therefore
/// asked for a lock its own predecessor still held and was told
/// `ServedElsewhere` — "your conversations are open in another instance" about
/// an instance that was this one — and `app.rs`'s `AuthSuccess` arm dutifully
/// armed the honest standing refusal, leaving the conversations page blank over
/// a live engine.
///
/// The hand-over is the fix, not a wait: `retire_conversations_engine` drops the
/// rail registration and releases the role through `RailBackend::retire`, which
/// is what makes it independent of every remaining `Arc` — and there are always
/// several here (this module's `ACTIVE_SESSION`, `AppState::mls`, the widget
/// tree's clones). Idempotent and a no-op on a first login, when no MLS rail is
/// registered yet.
///
/// Cross-process exclusivity is untouched: another *instance's* engine is
/// unreachable from this process, keeps its lock, and this build is still
/// refused honestly — the case that refusal was written for
/// (`docs/goal/architecture/apps/account-scoping.md` § Concurrent instances).
pub fn build_session_engine(
    manager: &ConversationsManager,
    identity: ActorKeypair,
    db_path: &std::path::Path,
) -> Result<Arc<crate::mls::MlsManager>, fauna_mls::error::MlsError> {
    manager.retire_conversations_engine();
    crate::mls::MlsManager::new(identity, db_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The linux twin of tui's
    /// `a_same_actor_same_nest_session_patch_converges_without_reestablishing`
    /// — one layer down, at the landing site rather than the door.
    ///
    /// A same-account re-login — sign out then back in, a factory-reset
    /// re-onboard, or the measured double-login race — must get a WORKING
    /// conversations engine, not the `ServedElsewhere` refusal its own
    /// predecessor's still-registered rail provokes. The predecessor here is
    /// registered exactly as production registers it, through
    /// [`ConversationsSession::from_manager`] (what
    /// [`start_conversations_session`] calls), so the rail under test is the
    /// real [`FaunaMlsBackend`] and the release runs its real `retire`.
    ///
    /// Reds without the `retire_conversations_engine` call in
    /// [`build_session_engine`]: the second build asks `mls_state.db` for a
    /// role lock the first engine is still holding, and `app.rs` arms the
    /// standing "served in another instance" refusal on that verdict —
    /// blanking the conversations page over a live engine.
    #[test]
    fn a_same_account_relogin_gets_the_conversations_role_back() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let db_path = dir.path().join("mls_state.db");
        // Built from the secret each time rather than cloned: `ActorKeypair` is
        // signing key material and deliberately not `Clone` (the same reason
        // `fauna_client_recovery`'s ceremony builds it twice from one secret).
        let secret_hex = "07".repeat(32);
        let an_identity = || ActorKeypair::from_secret_hex(&secret_hex).expect("test identity");
        let manager = ConversationsManager::new();

        // First login: build the engine, then register its rail through the
        // shared session factory. That registration is what survives sign-out
        // and an account switch — `clear_for_identity_change` deliberately
        // PRESERVES backends — so it is still there at the next login.
        let first = build_session_engine(&manager, an_identity(), &db_path)
            .expect("the first login builds this account's engine");
        let engine = first.engine();
        let self_actor = engine.identity_actor_id();
        // Offline by construction: the pin never dials, and a session that
        // tried would fail fast rather than wait out a deadline.
        let nest = NestClient::new("http://127.0.0.1:9".to_string(), an_identity());
        let _predecessor = ConversationsSession::from_manager(
            Arc::clone(&manager),
            engine,
            Arc::new(NestConversationsRpc::new(nest)),
            "alice@nest.test".to_string(),
            self_actor,
            None,
        );

        // Second login, same account, same process — the predecessor rail is
        // still registered and its engine still holds the role.
        let second = build_session_engine(&manager, an_identity(), &db_path);

        assert!(
            second.is_ok(),
            "a same-account re-login was refused its own conversations role \
             ({:?}) — the post-auth hook built over a predecessor it never \
             handed the role over from",
            second.err()
        );
    }
}

#[cfg(test)]
mod succession_fallback_tests {
    use super::*;

    /// **The call-site pin for the `__mls` re-seal barrier.** `fauna-client-mls-sync`'s
    /// own tests prove [`MlsStateSync::load`]'s barrier *works*; none of them
    /// can see THIS app stop passing the walk — the exact vacuity the drafts
    /// rails' `succession_fallback_tests` catch on their own planes.
    ///
    /// Mutation: drop the `.with_predecessors(..)` in [`build_mls_sync`] and
    /// this reds.
    #[test]
    fn the_mls_plane_offers_the_accounts_retired_roots() {
        let (tx, _rx) = crate::client::ui_channel();
        let retired = vec![BackupKey::from_bytes([0x44u8; 32])];
        let sync = build_mls_sync(
            &NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            retired,
            tx,
        );
        assert_eq!(
            sync.predecessor_count(),
            1,
            "after a succession this plane is still sealed under a \
             predecessor's root; without the offered key the launch load \
             hard-errors and the successor's whole conversations plane stays \
             dark for the session"
        );
    }

    /// The overwhelmingly common path — an identity that never succeeded — is
    /// unchanged, so a red above is about the threading and not about
    /// construction in general.
    #[test]
    fn an_identity_that_never_succeeded_offers_nothing() {
        let (tx, _rx) = crate::client::ui_channel();
        let sync = build_mls_sync(
            &NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            Vec::new(),
            tx,
        );
        assert_eq!(sync.predecessor_count(), 0);
    }
}
