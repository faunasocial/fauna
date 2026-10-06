//! tui runtime wiring for the shared fauna-native conversations receive path.
//!
//! At login (`session::establish`, the one post-auth hook) this builds a
//! [`ConversationsSession`] (`libs/fauna-conversations`) over the page's
//! [`super::ConversationsState`] manager — the surface the paint shell + the e2e
//! helpers observe — registers the real FaunaMls + SMTP + inbound-mail +
//! scheduling + inbox-drain + folder-gate backends, and starts the shared
//! [`ConversationsSession::start_receive_loop`]: the one detached receive task
//! that drives BOTH rails (conv welcome/channel + inbound mail) into the
//! manager, and replenishes the actor's key-package pool
//! (`direct-messages.md` § Key Package Management — the session tops up to the
//! shared target; no client-side publish step exists). This is the same path
//! linux wires in-process (`apps/fauna-linux/src/conversations/conv_backend.rs`,
//! the blueprint this file mirrors) and apple/windows/android reach through the
//! `fauna-ffi` `conversations_session` factory — one native receive path
//! fleet-wide (priority #1/#2).
//!
//! All MLS + mail crypto stays in shared Rust (`fauna-mls` + the shared
//! sources); this file is transport glue only (`docs/goal/ui/conversations.md`
//! § Architectural rules #2).
//!
//! Two deliberate differences from the linux blueprint:
//!
//! * **The session lives on [`super::ConversationsState`]**, not a process-wide
//!   static: the tui already scopes the manager to the session's lifetime via
//!   `App` (its stated improvement over linux's `OnceLock` shape), and the
//!   receive loop's liveness `Weak` retires the loop when a re-login replaces
//!   the state (the re-injection guard — `start_receive_loop`). No separate
//!   runtime handle is stashed either: the whole app is one tokio runtime, so
//!   the e2e wire-drivers are plain async fns the agent dispatch awaits
//!   directly, where linux must `block_on` from its GTK command thread.
//! * **The cross-device MLS state-sync plane rides the shared tokio launcher**
//!   (`fauna_client_mls_sync::launcher::tokio_launcher` — the same launcher the
//!   `fauna-ffi` factory injects for apple/windows/android, consumed here
//!   directly since the whole app is one tokio runtime), with the shared
//!   [`FolderRemovalResume`] post-restore hook. `start_receive_loop` runs it
//!   once before the first poll (restore-before-first-poll + the debounced
//!   replica autosave + the launch-time removal resume,
//!   `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync). A
//!   malformed secret leaves it unset and the client single-device, exactly as
//!   linux `conv_backend.rs`.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_conversations::{
    IndexLeaseSeat, MailKeyCache, NestBridgedGlue, NestConversationsRpc, NestFolderGate,
    NestInboxDrainSource, NestMailInboundSource, NestMailIndexLauncher, NestMlsReplicaTransport,
    NestOutboundMailSink, NestSchedulingSink, conv_push_source,
};
use fauna_client_folders::FolderRemovalResume;
use fauna_client_mls_sync::launcher::{PostRestoreHook, tokio_launcher};
use fauna_conversations::backend::{IndexBuilderLauncher, RoomSeams};
use fauna_conversations::{ConversationsSession, ThreadId, TypedAddress};
use fauna_core::delegation::ParticipantClass;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::engine::MlsEngine;
use tokio::sync::mpsc::UnboundedSender;

use super::ConversationsState;
use crate::app::{DataMessage, UiMessage};

/// Build the shared conversations session at login and start the unified
/// receive loop. Called once per successful `session::establish`, right after
/// [`super::init`] builds the manager (so in e2e mode the real FaunaMls + SMTP
/// registrations overwrite the mock rail entries, exactly as linux's
/// AuthSuccess wiring lands over its e2e mocks — `from_manager` is documented
/// idempotent for this).
///
/// `self_address` is the caller's best resolution of the logged-in account's
/// canonical `<handle>@<domain>` (`crate::session::session_self_address`),
/// possibly empty — construction never waits for identity resolution
/// (`conversations.md` § State & data shape → *Self-address: live, never
/// baked*). It seeds the session's live cell; when the background silent
/// challenge lands handle/domain later (or a server-side rename changes them),
/// the `DataMessage::SelfAddressRefreshed` handler pushes the new resolution
/// through `ConversationsSession::set_self_address`, healing both rails with
/// nothing rebuilt.
///
/// A failed MLS-engine init logs and leaves `real_session` unset: the page
/// keeps its honest degraded state (mocks in e2e, an empty snapshot otherwise)
/// rather than aborting login — mail and everything non-conversations still
/// works, mirroring linux's non-fatal engine-init arm.
#[allow(clippy::too_many_arguments)]
pub fn start_conversations_session(
    state: &mut ConversationsState,
    nest: Arc<NestClient>,
    secret_hex: &str,
    mail: Arc<dyn fauna_client_config::MailStore>,
    self_address: String,
    tx: &UnboundedSender<UiMessage>,
    mls_predecessors: Vec<fauna_client_mls_sync::BackupKey>,
    attested_predecessors: Vec<[u8; 32]>,
    runtime: crate::settings::AccountRuntimeSlot,
) {
    // One engine over one `mls_state.db`, persisted per-account under the
    // tui's namespaced config dir (`account-scoping.md` § The scoping
    // taxonomy) — `account_scope::account_state_dir`. A malformed secret can't resolve an actor id to scope
    // under; `start_with_db` below degrades that case to the unwired state
    // exactly as before, so the flat (unscoped) fallback path is harmless —
    // nothing ever opens an `MlsEngine` on it.
    let Some(dir) = crate::session::config_dir() else {
        tracing::error!("conv-backend: no config dir; conversations stay offline");
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let actor_id_hex = ActorKeypair::from_secret_hex(secret_hex)
        .ok()
        .map(|keypair| keypair.actor_id_hex());
    let db_path = match &actor_id_hex {
        Some(actor) => crate::account_scope::account_state_dir(actor).join("mls_state.db"),
        None => dir.join("mls_state.db"),
    };
    // This account's sync device id, for the index lease's seat — resolved
    // here, beside the other config-dir reads, so the seam below stays free
    // of them.
    let lease_device_hex = actor_id_hex
        .as_deref()
        .and_then(crate::media::device_id_hex);
    start_with_db(
        state,
        nest,
        secret_hex,
        mail,
        self_address,
        &db_path,
        lease_device_hex,
        tx,
        mls_predecessors,
        attested_predecessors,
        runtime,
    );
}

/// [`start_conversations_session`] minus the config-dir resolution — the
/// db-path seam the in-crate tests drive with a temp path instead of mutating
/// the process-global `XDG_CONFIG_HOME`. `lease_device_hex` is this account's
/// sync device id (`crate::media::device_id_hex`), resolved by the caller for
/// the same reason.
#[allow(clippy::too_many_arguments)]
fn start_with_db(
    state: &mut ConversationsState,
    nest: Arc<NestClient>,
    secret_hex: &str,
    mail: Arc<dyn fauna_client_config::MailStore>,
    self_address: String,
    db_path: &std::path::Path,
    lease_device_hex: Option<String>,
    tx: &UnboundedSender<UiMessage>,
    mls_predecessors: Vec<fauna_client_mls_sync::BackupKey>,
    attested_predecessors: Vec<[u8; 32]>,
    runtime: crate::settings::AccountRuntimeSlot,
) {
    // The account's folder-key custody over the settings slot — what the File
    // arm's key resolver, the custody-ingest sink and the removal resume read
    // and write (`fauna.state.folder-keys`).
    let folder_keys = crate::settings::folder_key_door(runtime.clone());
    // The grant log the removal resume's paywall keep-alive records in.
    let grant_log = crate::settings::nests::ledger_door(runtime.clone());
    let Some(manager) = state.manager.clone() else {
        tracing::warn!("conv-backend: no conversations manager at login");
        return;
    };
    let keypair = match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("conv-backend: invalid secret, conversations stay offline: {e}");
            return;
        }
    };
    // ── Release BEFORE build: hand the conversations-engine role over ─────
    //
    // `account-data-plane.md` § Multi-instance concurrency → *The role is
    // HANDED OVER in-process, never waited out* (2026-08-29). The ruling was
    // executed at the shared native factory
    // (`fauna_ffi::FfiNestClient::conversations_session*`), which macOS / iOS /
    // windows / android reach and tui does not — tui builds its manager itself,
    // so it inherited none of it. Every same-account re-login that finds a
    // previous rail still registered on this manager asks for a role lock its
    // own predecessor holds and is told `ServedElsewhere` — "your conversations
    // are open in another instance" about an instance that is this one, and the
    // page then carries that standing refusal for the rest of the process.
    //
    // `apply_session_patch`'s converge arm closes the *door* for the shape it
    // can see (an authenticated patch naming the live actor and nest), but not
    // this one: a sign-out drops `app.session` without clearing
    // `ConversationsState`, so the next login re-establishes with the
    // predecessor's rail still on the manager and nothing left to converge
    // against. Cross-process exclusivity is untouched — another *instance's*
    // engine keeps its lock and is still refused honestly, the case that
    // refusal was written for. Idempotent, and a no-op on a first login.
    manager.retire_conversations_engine();
    let engine = match MlsEngine::new(keypair, db_path) {
        Ok(e) => Arc::new(e),
        // The conversations-engine role is held by another instance — the
        // honest standing refusal, not a failure: the state is intact and the
        // holder is serving it (`account-data-plane.md` § Multi-instance
        // concurrency). The flag reaches `error-message` via
        // `sync_page_error`'s top-precedence arm; everything
        // non-conversations proceeds normally.
        Err(fauna_mls::error::MlsError::ServedElsewhere) => {
            state.served_elsewhere = true;
            tracing::info!(
                "conv-backend: this account's conversations are served in another \
                 instance — refusing the engine role honestly"
            );
            return;
        }
        Err(e) => {
            tracing::error!(
                "conv-backend: MLS engine init failed, conversations stay offline: {e}"
            );
            return;
        }
    };
    let self_actor = engine.identity_actor_id();

    let rpc = Arc::new(NestConversationsRpc::new(Arc::clone(&nest)));
    // Wire the home-nest link-preview seam (render-model.md § D4) with the SAME
    // object (`NestConversationsRpc` impls both `ConversationsRpc` and
    // `LinkPreviewRpc`), so a conversation bubble's bare-url `LinkPreview`
    // resolves through the manager.
    manager.set_link_preview_rpc(rpc.clone());
    let room_seams = RoomSeams::from_rpc(&rpc);
    let session = ConversationsSession::from_manager(
        manager,
        engine,
        rpc,
        self_address,
        self_actor,
        // `None` when a test-capable build was told to suppress the push arm
        // (`FAUNA_E2E_SUPPRESS_CONV_PUSH`) to force the durable inbox-apply
        // drain backstop (the layer-5 missed-push receive proof). The env read
        // is compiled out of a release build with no `e2e-agent` feature
        // (convention 15), so the production twin always returns the live source.
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
    //   registered at the account-store-ready edge (`App::AccountStoreReady`), because the store is
    //   assembled off the login path. Either unset, a `RoomSealed` record stays
    //   unopened;
    // - the community class's CEREMONY: founding a room through `room.create`,
    //   keying it through `room.publish_generation`, and the governance doors.
    //   Unlike the reads, unset is not a quiet skip: founding refuses by name,
    //   because a room founded on a device that cannot key it is a room nobody
    //   could ever send into.
    session.set_room_seams(room_seams);

    // Outbound mail (send) rail — registered here so a client that RECEIVES
    // before it ever sends still has the SMTP backend `ingest_inbound` needs.
    session.register_smtp(Arc::new(NestOutboundMailSink::new(Arc::clone(&nest))));

    // Inbound mail read-feeds (INBOX + Sent) — the shared lazily-keyed source
    // (derives the recipient HPKE secret from the mail custody on first poll;
    // a graceful no-op until mail is enabled). Both halves so mail the user
    // sent from another MUA also surfaces in the unified view.
    // One shared mail-key cache across every mail-keyed consumer this login
    // wires (the two read feeds + the content-index launcher below), so the
    // account's mail custody is read once per launch rather than once each.
    let mail_keys = MailKeyCache::new(Arc::clone(&nest), Arc::clone(&mail));
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
    // Held on the state as well as registered: the same object is the query
    // side of the index (`local_search_index`), which `crate::search` attaches
    // to the shared `SearchManager` once that exists later in the same post-auth
    // hook.
    //
    // The seat puts this builder under the advisory `index` lease
    // (`participants.md` § Coordination primitive → *The `index` kind under the
    // lease*), which is what makes tui the runner-of-record on its own
    // Task-delegation row instead of leaving it reading Waiting-while-running.
    //
    // **`PluggedInDesktop`, always.** tui ships no AC-line monitor, and the
    // unknown-power default across every seat is deliberately *candidate*
    // (`fauna-ffi` `LeaseRuntime::for_device` takes the same one until its
    // platform reports) — a lone tui seat must still build rather than stand
    // down forever waiting for a plugged-in peer that does not exist. Not a knob:
    // no user or admin would choose it, so it is a constant at the wiring site
    // (product invariants § the only configuration surface is the apps).
    //
    // No device id ⇒ no seat ⇒ the builder runs uncoordinated, exactly as it did
    // before the lease existed. A tui launch whose device-id store will not open
    // must still index its mail; losing the *coordination* is the cheap half.
    let lease_seat = lease_device_hex
        .and_then(|hex| fauna_core::hex32::decode(&hex).ok())
        .map(|device_id| IndexLeaseSeat {
            device_id,
            class: ParticipantClass::PluggedInDesktop,
            pins: {
                let slot = runtime.clone();
                Arc::new(fauna_sync_engine::account_runtime::SeatAccountStore::new(
                    Arc::new(move || slot.lock().ok().and_then(|handle| handle.clone())),
                ))
            },
        });
    let index_launcher =
        NestMailIndexLauncher::new(Arc::clone(&nest), Arc::clone(&mail_keys), lease_seat);
    // The File arm's shared-set key resolver — what lets its reconcile walk and
    // its query-time resolver render the sealed names of sets shared **with**
    // this actor (`content-index.md` § Ingest triggers, v1 → *The files/media
    // arms are SCOPED*: group-shared sets are included).
    //
    // Injected from app glue rather than built inside the launcher, and that is a
    // dependency fact rather than a preference: `fauna-client-folders` — which
    // owns `NestFolderKeyResolver` — depends on `fauna-client-conversations`
    // under its `mls` feature, so the launcher cannot depend on it back. It
    // derives the *owner* root itself from the identity seed; this covers only
    // the shared half.
    //
    // The same shared resolver the Media page and the devices page already build
    // over the account's folder-key custody — one resolver, because whoever can
    // open a set's bytes renders its names (`file-sync.md` § Sealed names &
    // paths). A malformed secret leaves it unwired, which costs shared-set rows
    // and nothing else: they are skipped **without burning the re-index
    // guard**, so a later walk with the keys stages them.
    // …and the account's attested predecessor ids, beside it: the same reader
    // seat then admits a row a retired identity signed with no succession
    // lookup (writer-signed change records, ruling (8)(b) source (ii)).
    index_launcher.set_attested_predecessors(attested_predecessors.clone());
    if ActorKeypair::from_secret_hex(secret_hex).is_ok() {
        index_launcher.set_folder_key_resolver(Arc::new(
            fauna_client_folders::NestFolderKeyResolver::new(
                Arc::clone(&nest),
                folder_keys.clone(),
            ),
        ));
    }
    session
        .set_index_builder_launcher(Arc::clone(&index_launcher) as Arc<dyn IndexBuilderLauncher>);
    state.index_launcher = Some(index_launcher);

    // Scheduling drain — the mailbox-less CalDAV iMIP rail: the loop drains
    // every `WelcomeChannelKind::Scheduling` channel to this sink, which applies
    // the iMIP to the actor's calendar via the shared `CalDavClient`
    // (caldav-server.md § Server-side auto-schedule, Half-1).
    session.register_scheduling_sink(Arc::new(NestSchedulingSink::over(
        Arc::clone(&nest),
        Arc::clone(&mail_keys),
        Arc::downgrade(&session.manager()),
        session.manager().refused_changes(),
    )));

    // Durable inbox-apply backstop — the loop's ticker drains the per-actor
    // `fauna.inbox.*` queue, recovering a Welcome whose best-effort push was
    // missed (`api-layers.md` § Inbox & Messaging, layer 3). Holds a `Weak`
    // session to avoid the session→source cycle (the session owns the source).
    session.register_inbox_drain(Arc::new(NestInboxDrainSource::new(
        Arc::clone(&nest),
        Arc::downgrade(&session),
    )));

    // Recipient contact-gate for cross-user shared folders (`folders.md`
    // § Sharing — "auto for contacts, knock for strangers"): a
    // `WelcomeChannelKind::Folder` welcome routes through this gate. Without it
    // every folder welcome is retained un-acked (the pre-gate safe default),
    // so a *contact's* share would wrongly knock.
    session.register_folder_gate(Arc::new(NestFolderGate::new(Arc::clone(&nest))));

    // Member content-key custody ingest (Phase 0 — the read leg; `folders.md`
    // § Sharing): on join and on each rotation-commit receipt, the session
    // fetches the owner's sealed content-key envelope, opens it via the group
    // epoch, and folds the generations into this member's own `fauna.state.folder-keys` custody
    // — so a *member* (not just the owner) can decrypt a shared set's content.
    // The same shared `NestFolderCustodySink` the FFI + linux factories inject
    // (one seam fleet-wide, priority #2). `secret_hex` already validated above; a
    // malformed one simply skips it (list-but-not-decrypt, the pre-Phase-0 state).
    //
    // No app hop rides the ingest: the rotated generation it joins into the
    // account's folder-key custody is itself the custody write the nest nudges
    // the desktop sync agent about (the `state-fleet` nudge), and the agent
    // re-resolves its running engines' keys on it (`on-demand-files.md`
    // § Shared sets on a capability host → *One mechanism*).
    if ActorKeypair::from_secret_hex(secret_hex).is_ok() {
        session.set_folder_custody_sink(Arc::new(
            fauna_client_folders::NestFolderCustodySink::new(
                Arc::clone(&nest),
                folder_keys.clone(),
            ),
        ));
    }

    // The ACCOUNT-custody ceremony sink (T16, W8.4 (account-data-plane.md § Workstreams) — a different plane from
    // the folder content-key custody above): every received offer / accept /
    // deliver / A7 receipt on a conversation channel is verified + durably
    // captured into `fauna.state.custody-ceremony` in one read-join before the
    // poll moves on — through the account runtime slot, resolved per payload
    // because the store lands after login — and the observer schedules the
    // `drive_ceremonies` "act" pass (`custody_glue::spawn_drive`). Without this
    // registration the whole custody facet renders permanently empty — the
    // payloads would wait in channel history, tallied `no_sink`.
    if let Ok(keypair) = ActorKeypair::from_secret_hex(secret_hex) {
        let own_actor = keypair.actor_id();
        session.set_custody_ceremony_sink(Arc::new(
            fauna_client_conversations::StoreCustodyCeremonySink::new(
                fauna_client_config::ResolvingLedgerStore::new(move || {
                    runtime.lock().ok().and_then(|handle| handle.clone())
                }),
                own_actor,
            )
            .with_observer(Arc::new(crate::custody_glue::TuiCeremonyObserver {
                tx: tx.clone(),
            })),
        ));
    }

    // In-group succession witness (`identity-succession.md` § Propagation →
    // MLS groups). Without it every `GroupMetaMessage::Succession` degrades to
    // the bare add — a member sees "someone added a stranger" where a peer
    // actually recovered their account. The policy (cached head first, then the
    // anchored walk to the old identity's home nest) is shared; tui supplies
    // only the two things that cannot be: the thread store it reads handles
    // from, and its own native dialer.
    // The anchors' durable store (`fauna.state.peer-anchors`) is lent late,
    // through the manager, by the shared store-ready registration
    // (`wire_account_store_seams` below) — nothing to hand in here, so
    // nothing to forget.
    {
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
        // The second handle is the app's only route to the witness's own
        // report — see `ConversationsState::succession_witness`.
        state.succession_witness = Some(witness);
    }

    // Cross-device MLS state-sync plane (`docs/goal/behavior/devices.md`
    // § Cross-device MLS group-state sync) — the tui leg: the shared tokio
    // launcher over this session's backend/manager/rpc, with the shared
    // folder removal-resume hook. `start_receive_loop` runs it once, before
    // the first poll (restore-before-first-poll + gate/cursor injection + the
    // debounced replica autosave). `secret_hex` already validated above
    // (`from_secret_hex`), so the size guards here are belt-and-suspenders —
    // a malformed secret leaves the plane unset and the client single-device,
    // exactly as linux `conv_backend.rs`.
    let seed = fauna_core::hex32::decode(secret_hex).ok();
    // The recording device (the one the serve walk and the Media page record
    // under) also resumes an interrupted served-set walk in the same pass
    // (`webdav-server.md` § Key model (c)).
    let post_restore = seed.map(|seed| {
        Arc::new(
            FolderRemovalResume::new(
                Arc::clone(&nest),
                seed,
                folder_keys.clone(),
                Arc::clone(&mail),
                Arc::clone(&grant_log),
            )
            .with_recording_device(crate::media::device_id_hex_for_secret(seed))
            .with_predecessors(attested_predecessors.clone()),
        ) as Arc<dyn PostRestoreHook>
    });
    if let Some(launcher) = tokio_launcher(
        Box::new(NestMlsReplicaTransport::new(Arc::clone(&nest))),
        seed.as_ref().map(<[u8; 32]>::as_slice).unwrap_or(&[]),
        session.backend(),
        session.manager(),
        post_restore,
        fauna_client_mls_sync::SuccessionReseal {
            predecessors: mls_predecessors,
            // The re-seal reports from inside the replica's own load, the one
            // place the work provably happens — see
            // `MlsStateSync::with_reseal_sink`.
            sink: Some(Box::new({
                let tx = tx.clone();
                move |progress| {
                    let _ = tx.send(UiMessage::Data(DataMessage::MlsResealProgress(progress)));
                }
            })),
        },
    ) {
        session.set_mls_sync_launcher(launcher);
    }

    // The peer-anchor harvest sweep — the producer half of the member-path
    // anchor (`identity-succession.md` § The succession statement → *the
    // peer-profile harvest*; the gate). A Welcome-joined roster row
    // carries no handle, so without this sweep the witness registered above
    // has no anchor for exactly the peers the in-group statement is about.
    // Runs on the ordinary read path (a roster sweep), never at verify time —
    // harvest rule 4.
    // Injected, not spawned: `start_receive_loop` launches it first in its
    // prologue, inside the runtime it already owns, so every app inherits the
    // sweep through one seam rather than each owing a post-auth spawn it can
    // forget (`ConversationsSession::set_peer_anchor_sweep_launcher`).
    session.set_peer_anchor_sweep_launcher(Arc::new(
        fauna_client_recovery::harvest::PeerAnchorSweep::new(
            std::sync::Arc::downgrade(&session),
            Arc::clone(&nest),
            Arc::clone(&state.peer_anchor_harvest),
            fauna_client_recovery::harvest::PEER_ANCHOR_SWEEP_INTERVAL,
        ),
    ));

    // Publish the session before starting the loop so its liveness `Weak` stays
    // upgradeable; a re-login builds a fresh state, dropping this Arc and
    // ending the loop.
    state.real_session = Some(Arc::clone(&session));
    tokio::spawn(async move {
        session.start_receive_loop().await;
    });
}

/// tui's concrete in-group succession witness — the shared policy over the
/// shared anchors and the shared native dialer. Named because
/// [`crate::conversations::ConversationsState::succession_witness`] holds one
/// and a `dyn` handle could not answer [`Self::observation`]. linux's twin
/// (`conv_backend::LinuxChainWitness`) is the same three types.
pub type TuiChainWitness = fauna_client_recovery::ChainWitness<
    fauna_client_recovery::ThreadParticipantAnchors,
    fauna_client_recovery::witness::NativeSuccessionChainSource,
>;

/// The member side of a succession as a driver reads it — the field reads that
/// feed the shared renderer (`fauna_client_recovery::witness::state_json`,
/// which owns the shape and the reading order).
///
/// `null` until a real conversations session exists. Every read here is a field
/// read, never a round trip: this runs on the agent's ack path (e2e convention
/// 11's second corollary).
pub fn witness_state_json(state: &crate::conversations::ConversationsState) -> serde_json::Value {
    let Some(witness) = state.succession_witness.as_ref() else {
        return serde_json::Value::Null;
    };
    let counts = state
        .real_session
        .as_ref()
        .map(|s| s.backend().succession_statement_counts())
        .unwrap_or_default();
    fauna_client_recovery::witness::state_json(
        &witness.observation(),
        &state.peer_anchor_harvest,
        &counts,
    )
}

// The pre-cell `ensure_smtp_backend` compose-path re-register (linux's
// `conversations::mail_sink` shape) is RETIRED: the session's live self-address
// cell (`conversations.md` § State & data shape → *Self-address: live, never
// baked*) is seeded at login and pushed by the `SelfAddressRefreshed` handler,
// so both rails — not just SMTP — read the current address at use time with no
// per-send glue.

// ── e2e wire-drivers (`conversations_real_*` agent commands) ─────────────────
//
// The tui twins of linux's `conv_backend.rs` test-only surface. The tier_3
// real-wire suites (`test_fauna_mls_real_roundtrip` and friends) drive the
// manager's async wire-drivers with the API-tier peer's `actor_id` injected.
// Plain async fns — the agent's command dispatch (`automation::apply_command`)
// already awaits, so no runtime-handle `block_on` shim exists here. Errors are
// logged by the callers (the tests' proof is the observable nest effect — key
// package consumed / Welcome delivered — read back via the API-tier peer).

/// Whether the real conversations session is live —
/// `data.conv_real_backend_active`, which the e2e polls before driving a real
/// send. Set synchronously by [`start_conversations_session`], exactly like
/// linux's `ready` flag.
pub fn is_e2e_real_active(state: &ConversationsState) -> bool {
    state.real_session.is_some()
}

/// Register the conversations seams that rest on the **account store** once
/// it exists — the shared `conversation_seams::wire`, every runtime-hosting
/// app's one call: the community class's group-reception keys (the second
/// half of a room's read, `conversation-rooms.md` § The three classes →
/// *Community*) and the native rail's read positions
/// (`conversation-read-state.md` § The read-marker record).
///
/// Called from the account-store-ready edge rather than from
/// [`start_conversations_session`], because the store is assembled **off** the
/// login path: at login there is no handle to hand over. Until then a
/// community room's records stay unopened and native threads keep the launch
/// floor — the honest state, not a failure.
///
/// A no-op when no real conversations session exists (an engine-init refusal,
/// or an account whose store readied after a sign-out): the seams have
/// nothing to attach to, and the next login registers them from scratch.
pub fn wire_account_store_seams(
    state: &ConversationsState,
    store: fauna_sync_engine::account_runtime::AccountStoreHandle,
) {
    let Some(session) = state.real_session.as_ref() else {
        return;
    };
    fauna_client_account_runtime::conversation_seams::wire(
        session,
        store,
        &tokio::runtime::Handle::current(),
    );
}

/// `conversations_disable_real_faunamls` teardown: wipe the manager's threads
/// so a later snapshot conversations test (collection order can put one after
/// the real-wire test, sharing the session-cached app) starts clean. There is
/// no mock to restore — every app drives the real backend after login.
///
/// Compiled out of release artifacts (`docs/goal/architecture/testing.md`
/// convention 15): it drives `clear_for_test`, absent from a release build. Its
/// only caller is the gated-real `automation::apply_command`, so no no-op twin is
/// needed — the same shape as linux's `conv_backend::disable_e2e_real_backend`.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn disable_e2e_real_backend(state: &ConversationsState) {
    if let Some(m) = state.manager.as_ref() {
        m.clear_for_test();
    }
}

/// `conversations_real_resolve_send_new`: start a new FaunaMls conversation by
/// **resolving** the typed `recipient` through the real backend probe (no
/// `actor_id` injection) and send `body` — the same `resolve_recipient` →
/// `accept_current_recipient_chip` → `send_new_thread` chain the UI drives.
/// Returns the new thread id.
pub async fn e2e_resolve_send_new(
    state: &ConversationsState,
    recipient: &str,
    body: &str,
) -> Result<Option<String>, String> {
    let m = manager(state)?;
    m.start_new_conversation();
    m.set_new_thread_recipient_input(recipient.to_string());
    m.resolve_recipient().await;
    if !m.accept_current_recipient_chip() {
        return Err(format!(
            "recipient '{recipient}' did not resolve to a chip (not a reachable Fauna actor?)"
        ));
    }
    m.set_new_thread_body(body.to_string());
    let tid = m.send_new_thread().await.map_err(|e| e.to_string())?;
    Ok(tid.map(|t| t.0))
}

/// `conversations_real_send`: send `body` on an existing thread (a
/// forked-but-unbound group bootstraps its MLS group lazily here).
pub async fn e2e_send(
    state: &ConversationsState,
    thread_id: &str,
    body: &str,
) -> Result<(), String> {
    let m = manager(state)?;
    let tid = ThreadId(thread_id.to_string());
    m.set_compose_body(tid.clone(), body.to_string());
    m.send(tid).await.map_err(|e| e.to_string())
}

/// `conversations_real_send_attachment`: send `body` plus one attachment on an
/// existing thread, driving the **real** seal + upload path (`add_attachment` →
/// `send` → the epoch-key blob seal → `ConversationsRpc::blob_put`).
pub async fn e2e_send_with_attachment(
    state: &ConversationsState,
    thread_id: &str,
    body: &str,
    filename: &str,
    mime_type: &str,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let m = manager(state)?;
    let tid = ThreadId(thread_id.to_string());
    m.set_compose_body(tid.clone(), body.to_string());
    m.add_attachment(
        tid.clone(),
        filename.to_string(),
        mime_type.to_string(),
        bytes,
    );
    m.send(tid).await.map_err(|e| e.to_string())
}

/// `conversations_real_add`: add the peer (`actor_id` injected) to `thread_id`.
/// On a bound FaunaMls group this posts the MLS Commit + Welcome; on a 1:1 it
/// forks a fresh group. Returns the resulting (possibly new) thread id.
pub async fn e2e_add(
    state: &ConversationsState,
    thread_id: &str,
    peer_actor_id_hex: &str,
    peer_handle: &str,
) -> Result<Option<String>, String> {
    let actor_id = parse_actor_hex(peer_actor_id_hex)?;
    let m = manager(state)?;
    m.open_add_participant(ThreadId(thread_id.to_string()));
    m.set_add_participant_recipient_input(peer_handle.to_string());
    m.accept_add_participant_chip(TypedAddress::Fauna {
        handle: peer_handle.to_string(),
        actor_id,
    });
    let new_id = m.confirm_add_participant().await;
    page_error(&m)?;
    Ok(new_id.map(|t| t.0))
}

/// `conversations_real_remove`: remove the peer (`actor_id` injected) from a
/// bound FaunaMls group (posts the MLS Commit; no Welcome). `peer_handle` must
/// match the one used at [`e2e_add`] — the snapshot removal keys on the
/// address display; the wire op finds the MLS leaf by `actor_id` regardless.
pub async fn e2e_remove(
    state: &ConversationsState,
    thread_id: &str,
    peer_actor_id_hex: &str,
    peer_handle: &str,
) -> Result<(), String> {
    let actor_id = parse_actor_hex(peer_actor_id_hex)?;
    let m = manager(state)?;
    let addr = TypedAddress::Fauna {
        handle: peer_handle.to_string(),
        actor_id,
    };
    m.remove_participant(ThreadId(thread_id.to_string()), addr)
        .await;
    page_error(&m)
}

/// `conversations_real_rename`: rename a bound FaunaMls group (posts the
/// encrypted `GroupMeta::NameChanged` Application envelope).
pub async fn e2e_rename(
    state: &ConversationsState,
    thread_id: &str,
    new_label: &str,
) -> Result<(), String> {
    let m = manager(state)?;
    m.rename_thread(ThreadId(thread_id.to_string()), new_label.to_string())
        .await;
    page_error(&m)
}

/// Turn a page error the gesture just stamped into the `Err` its caller can
/// report. The three membership/label gestures above return `()`/`Option` and
/// signal a failed wire op **only** through the page-error slot, so without this
/// read-back the agent acks success for an op the nest refused — convention 11's
/// swallow, and the one that cost a whole triage pass (a `forbidden` Welcome
/// read downstream as "no Welcome delivered").
fn page_error(m: &Arc<fauna_conversations::ConversationsManager>) -> Result<(), String> {
    match m.page_error_diagnostic() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn manager(
    state: &ConversationsState,
) -> Result<Arc<fauna_conversations::ConversationsManager>, String> {
    state
        .manager
        .clone()
        .ok_or_else(|| "no conversations manager (pre-auth)".to_string())
}

fn parse_actor_hex(actor_id_hex: &str) -> Result<ActorId, String> {
    ActorId::from_hex_labeled(actor_id_hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SECRET: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    /// A unique per-test MLS db path under the OS temp dir — no
    /// process-global env mutation (tests run in parallel), no extra dev-dep.
    fn a_db_path(tag: &str) -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "fauna-tui-convtest-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("mls_state.db")
    }

    fn a_state() -> (
        ConversationsState,
        UnboundedSender<crate::app::UiMessage>,
        tokio::sync::mpsc::UnboundedReceiver<crate::app::UiMessage>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (super::super::init(&tx), tx, rx)
    }

    /// An offline client is enough: `NestClient::new` performs no I/O, and the
    /// receive loop's connect failures surface through its own retry path, not
    /// this wiring.
    fn an_offline_nest() -> Arc<NestClient> {
        NestClient::new(
            "http://127.0.0.1:9".to_string(),
            ActorKeypair::from_secret([9u8; 32]),
        )
    }

    /// **The hand-over pin.** A second login for the SAME account, in the same
    /// process, must get a working conversations engine — not the
    /// `ServedElsewhere` refusal its own predecessor's still-registered rail
    /// provokes. `apply_session_patch`'s converge arm cannot cover this shape:
    /// a sign-out drops `app.session` without clearing `ConversationsState`, so
    /// the next login arrives with the predecessor's rail on the manager and
    /// nothing left to converge against.
    ///
    /// Reds without `manager.retire_conversations_engine()` in
    /// [`start_with_db`]: the second `MlsEngine::new` asks `mls_state.db` for a
    /// role lock the first engine is still holding, `served_elsewhere` latches,
    /// and the page carries the standing refusal for the life of the process
    /// (`account-data-plane.md` § Multi-instance concurrency → *The role is
    /// HANDED OVER in-process, never waited out*).
    #[tokio::test]
    async fn a_same_account_relogin_gets_the_conversations_role_back() {
        let (mut state, tx, _rx) = a_state();
        let db_path = a_db_path("relogin");

        start_with_db(
            &mut state,
            an_offline_nest(),
            SECRET,
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            "alice@nest.test".into(),
            &db_path,
            None,
            &tx,
            Vec::new(),
            Vec::new(),
            Default::default(),
        );
        assert!(is_e2e_real_active(&state), "the first login must wire up");

        // Sign out and back in as the same account: the session goes, the
        // page's manager — and the rail it holds — does not.
        state.real_session = None;
        start_with_db(
            &mut state,
            an_offline_nest(),
            SECRET,
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            "alice@nest.test".into(),
            &db_path,
            None,
            &tx,
            Vec::new(),
            Vec::new(),
            Default::default(),
        );

        assert!(
            !state.served_elsewhere,
            "a same-account re-login was told its own conversations are served \
             in another instance — the predecessor's role was never handed over"
        );
        assert!(
            is_e2e_real_active(&state),
            "the re-login must leave the page real-wired, not dark"
        );
    }

    /// The post-auth hook must leave the page REAL-wired: the session is live
    /// (`data.conv_real_backend_active` reads it) and drives the SAME manager
    /// the paint shell + e2e serializer observe — a session over a private
    /// manager would ingest into a surface nothing renders.
    #[tokio::test]
    async fn login_wires_the_real_session_over_the_pages_own_manager() {
        let (mut state, tx, _rx) = a_state();
        start_with_db(
            &mut state,
            an_offline_nest(),
            SECRET,
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            "alice@nest.test".into(),
            &a_db_path("wires"),
            None,
            &tx,
            Vec::new(),
            Vec::new(),
            Default::default(),
        );

        assert!(is_e2e_real_active(&state), "the readiness flag must flip");
        let session = state.real_session.as_ref().expect("session built");
        assert!(
            Arc::ptr_eq(&session.manager(), state.manager.as_ref().unwrap()),
            "the session must drive the page's own manager (one observable surface)"
        );
    }

    /// A malformed secret degrades to the wired-nothing state (mocks in e2e,
    /// empty snapshot in production) instead of panicking login — the same
    /// non-fatal arm linux takes on an engine-init failure.
    #[tokio::test]
    async fn a_malformed_secret_leaves_the_page_unwired_not_broken() {
        let (mut state, tx, _rx) = a_state();
        start_with_db(
            &mut state,
            an_offline_nest(),
            "not-hex",
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            "alice@nest.test".into(),
            &a_db_path("malformed"),
            None,
            &tx,
            Vec::new(),
            Vec::new(),
            Default::default(),
        );

        assert!(state.real_session.is_none());
        assert!(!is_e2e_real_active(&state));
    }

    /// The real-wire drivers refuse pre-auth (no manager) with an error instead
    /// of a silent green — the agent logs it and the test's nest-effect
    /// assertion then fails loudly.
    #[tokio::test]
    async fn the_wire_drivers_error_before_auth() {
        let state = ConversationsState::default();
        assert!(e2e_send(&state, "t1", "hi").await.is_err());
        assert!(e2e_resolve_send_new(&state, "someone", "hi").await.is_err());
    }

    /// Generous ceiling for "the sweep let go" — a green run pays none of it.
    const RELEASE_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

    /// **The harvest sweep's release pin.** The sweep holds the manager only
    /// while a pass runs: once the session it rides is dropped, the manager —
    /// and the retired MLS engine behind it, with its one-engine-per-store
    /// lock — is released at the drop, not at the sweep's next tick
    /// (`account-scoping.md` § Implementation status → the `tui (in-memory)`
    /// ledger row). The cadence is muted to a day, so a manager held across
    /// the sleep can only be released by the session-closed signal.
    ///
    /// Reds under the shape until 2026-08-27: a strong `manager` held across a
    /// plain `sleep`, with nothing waking the task at the drop. (Either half
    /// alone passes — a `Weak` re-upgraded per pass releases the engine even
    /// if the task lingers, and the `closed` arm ends a task that still holds
    /// it; the pin is about the engine, so the mutation to run is both.)
    ///
    /// ⚠ The drops come only AFTER the sweep has provably run a pass and parked:
    /// the first cut of this pin dropped straight after `spawn`, the task's
    /// first `upgrade()` then usually ran after the drops and returned with
    /// nothing held, and the pin passed on the mutated shape too (measured
    /// 2026-08-27). The proof is a logged harvest outcome — a nameless roster
    /// row makes the pass dial, and a torn-down nest makes that dial fail at
    /// once (`NestClient::disconnect`) instead of waiting out a deadline.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_harvest_sweep_releases_the_manager_when_its_session_ends() {
        let manager = fauna_conversations::ConversationsManager::new();
        let engine = Arc::new(
            MlsEngine::new_in_memory(ActorKeypair::from_secret([5u8; 32])).expect("engine"),
        );
        let self_actor = engine.identity_actor_id();
        let nest = an_offline_nest();
        nest.disconnect().await;
        let session = ConversationsSession::from_manager(
            Arc::clone(&manager),
            engine,
            Arc::new(NestConversationsRpc::new(Arc::clone(&nest))),
            "alice@nest.test".to_string(),
            self_actor,
            None,
        );
        // A Welcome-joined roster row (no handle) — exactly what the sweep
        // harvests for, so the first pass dials, fails, and records an outcome.
        manager.materialize_conv_thread(
            "c0ffee".to_string(),
            vec![TypedAddress::Fauna {
                handle: String::new(),
                actor_id: ActorId([7u8; 32]),
            }],
        );
        // The anchor store, lent as the account-store-ready edge lends it: a
        // sweep with no store waits out its grace without dialling or logging
        // (`harvest::UNLENT_STORE_GRACE_PASSES`), which would leave nothing to
        // prove the first pass ran.
        manager.register_peer_anchor_store(Some(Arc::new(
            fauna_conversations::backend::MemoryPeerAnchorStore::default(),
        )));
        let log: Arc<fauna_client_recovery::harvest::HarvestLog> = Default::default();
        fauna_client_recovery::harvest::spawn_peer_anchor_harvest_sweep(
            Arc::downgrade(&session),
            Arc::downgrade(&manager),
            Arc::clone(&nest),
            Arc::clone(&log),
            std::time::Duration::from_secs(86_400),
            session.closed(),
        );
        let ran = tokio::time::Instant::now() + RELEASE_BUDGET;
        while log.entries().is_empty() {
            assert!(
                tokio::time::Instant::now() < ran,
                "the harvest sweep never ran its first pass within {RELEASE_BUDGET:?}"
            );
            // sleep-ok: the poll interval of a deadline wait, not the assertion.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let manager_gone = Arc::downgrade(&manager);
        drop(session);
        drop(manager);

        let deadline = tokio::time::Instant::now() + RELEASE_BUDGET;
        while manager_gone.upgrade().is_some() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the harvest sweep kept the dropped session's manager alive for the \
                 whole {RELEASE_BUDGET:?} budget — it holds the manager across its sleep"
            );
            // sleep-ok: the poll interval of a deadline wait, not the assertion.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}
