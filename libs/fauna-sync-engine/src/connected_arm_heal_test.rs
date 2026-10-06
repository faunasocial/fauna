//! **What `run_watch_loop`'s `Connected` arm DOES** — the body, at tier_1.
//!
//! [`crate::always_resident::connected_arm_tests`] pins the arm's *trigger*
//! (`next_connected`'s edge semantics: fires on the transition into
//! `Connected`, parks on a dropped sender, inert without a receiver). That is
//! the whole of what could be asserted without a nest, and it leaves the three
//! things the arm exists to do unpinned:
//!
//! 1. **re-resolve the seat's sync mode** — the live cause of the 2026-08-28
//!    delete-propagation failure (`behavior/file-sync.md` § 4, *Implementation
//!    status today* (1)): an in-process host reaches loop entry while its own
//!    control plane is still connecting, both authoritative reads fail, and a
//!    seat with nothing cached lands on `Unresolved` — declining every peer
//!    delete and holding its anchor while looking completely healthy;
//! 2. **pull** — "not optional" in that same note, because an unresolved seat
//!    *held* its anchor precisely so the tombstone re-delivers once the role is
//!    readable, and the nudge that would have carried it has already been
//!    consumed and declined;
//! 3. **subscribe `conn_rx` at all** — it used to be `p2p-share`-gated, which
//!    parked the arm entirely on every other build and left the mode with no
//!    healing trigger but the 300 s rescan tick.
//!
//! ⚠ On (3), read the pin for exactly what it proves. **This crate's own test
//! build always has `p2p-share` on** — measured 2026-08-28 via `--unit-graph`:
//! the dev-dependencies unify `p2p-share`, `account-runtime`,
//! `engine-lifecycle` and `preference-store` into the lib under test, whatever
//! `cargo test` is asked for. So re-gating the subscription on
//! `cfg!(feature = "p2p-share")` is a **no-op here** and leaves these tests
//! green — which is not a weak pin but an inexpressible mutation, and worth
//! knowing before anyone reads a green run as evidence about the gate. What the
//! pin does prove is the half that can fail: an arm whose `conn_rx` is not
//! subscribed is dead, verified red by mutating the line to `None` — the exact
//! shape the pre-2026-08-28 code took on every non-`p2p-share` build. That the
//! line now carries no `cfg` is a property of the source, not of any run this
//! crate can host.
//!
//! Until this module the wiring was proven only at tier_3, by
//! `test_filesync_seats.py::test_seats_converge`'s `native`/`tui` cells. Two
//! obstacles kept it there, and both are now gone: the crate carries a
//! `fauna-client/test-util` dev-dependency (the mocked-socket
//! `SupervisedChannel` — the mode reads go through `NestClient`, so the
//! `wiremock` dev-dep cannot serve them), and `run_watch_loop` takes an
//! `Arc<SyncEngine>`, so a handle survives to read the healed mode back.
//!
//! # Tier and timing
//!
//! tier_1 (`architecture/testing.md` § The four-tier taxonomy): in-process, no
//! nest binary, no driver — only the socket is fake, the supervisor and the
//! dispatcher are production. Latency-independent per `e2e-conventions.md`
//! convention 14: every wait is a deadline poll on *state* with a budget far
//! above any non-pathological delay, and the rescan interval is set an hour
//! out so the tick can never stand in for the arm — the whole discriminating
//! power of these pins is that the arm, and nothing else, did the healing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, ConnectionState, NestClient, PushBroker};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::folders::{
    ActorMembersListReply, FolderActorMember, FolderSummary, FoldersListReply, MembersListReply,
};
use fauna_protocol::sync::SyncChangesListReply;
use fauna_protocol::{Frame, Reply, RpcError, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};

use crate::config::{ModeResolution, SyncMode};
use crate::engine::SyncEngine;
use crate::pull_remote_changes_test::test_engine_with_nest_client;

const FOLDER: &str = "holiday";

/// The set's owner in the served ACTOR roster — an owner is a writer by
/// definition (`refresh_writer_roster_via`), so `cached_share_writer` answering
/// `true` for this id is proof the roster read landed.
const PEER_ACTOR: &str = "abababababababababababababababababababababababababababababababab";

/// Far beyond the test's life: the rescan tick must never be able to do the
/// arm's work, or a red arm would still go green on the tick's own refresh.
const NO_TICK: Duration = Duration::from_secs(3600);

/// Generous ceiling for every deadline poll below (convention 14: sized far
/// above any non-pathological delay, never tuned to a machine).
const BUDGET: Duration = Duration::from_secs(30);

/// The nest double: a request log plus one switch.
///
/// While `serving` is false every read is answered with a genuine wire error.
/// That covers the slower half of loop entry's race — a plane that is up but
/// not yet answering for this seat; the faster half never reaches this double
/// at all, because `SyncEngine::resolve_sync_mode` skips both authoritative
/// reads outright while the plane is not `Connected`. Either way entry has no
/// authoritative answer and no cache, so the seat lands `Unresolved` — the
/// state the arm exists to heal. Flipping the switch to true is "the role
/// became readable", the exact condition the transition then finds.
struct NestDouble {
    kinds: Mutex<Vec<String>>,
    serving: AtomicBool,
    /// The rows `fauna.folders.list` serves — [`served_folder`] alone unless a
    /// test rewrites them (the declassification pins below vary its audience
    /// and attestation; the namesake pin serves two same-named rows).
    folders: Mutex<Vec<FolderSummary>>,
    /// This seat's device id, set once the engine exists — the roster row
    /// `fauna.folders.members.list` serves for it (see [`served_roster`]).
    device_id_hex: Mutex<String>,
}

impl NestDouble {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            kinds: Mutex::new(Vec::new()),
            serving: AtomicBool::new(false),
            folders: Mutex::new(vec![served_folder()]),
            device_id_hex: Mutex::new(String::new()),
        })
    }

    /// Replace the served list with one row.
    fn serve_folder(&self, row: FolderSummary) {
        self.serve_folders(vec![row]);
    }

    /// Replace the served list, in the order the nest lists it.
    fn serve_folders(&self, rows: Vec<FolderSummary>) {
        *self.folders.lock().unwrap() = rows;
    }

    fn log(&self) -> Vec<String> {
        self.kinds.lock().unwrap().clone()
    }

    /// How many times `kind` has been requested since the double was built.
    fn count(&self, kind: &str) -> usize {
        self.log().iter().filter(|k| *k == kind).count()
    }
}

/// What a served `fauna.folders.list` says about this seat's set: owned by
/// this actor.
///
/// `role: Some("owner")` keeps the second read live: `config::should_read_member_role`
/// resolves a `member` row locally without a round trip, which would leave the
/// `members.list` half of the resolution unexercised.
fn served_folder() -> FolderSummary {
    FolderSummary {
        id: 1,
        name: FOLDER.to_string(),
        role: Some("owner".to_string()),
        ..FolderSummary::default()
    }
}

/// What a served `fauna.folders.members.list` says about this seat: an
/// **archive** place (`{originates, accepts, !applies_deletes}` — `Backup`).
///
/// `Backup` is deliberately **not** the constructor's starting mode (`Sync`)
/// and not `Unresolved`, so the healed value can be confused with neither the
/// build-time default nor the failure posture — the distinction the 2026-08-28
/// note turns on (`old=Resolved(Sync) new=Unresolved` was exactly a default
/// being mistaken for an answer).
fn served_roster(device_id_hex: &str) -> Vec<fauna_protocol::folders::FolderMember> {
    vec![fauna_protocol::folders::FolderMember {
        device_id: device_id_hex.to_string(),
        flags: fauna_protocol::folders::PlaceFlags::archive_place(),
        ..Default::default()
    }]
}

/// Answer one decoded request frame. Every kind is logged before it is
/// answered, so the log is a causal record of what the loop actually asked
/// for, in order — the observable the pull assertion reads.
fn reply_for(double: &NestDouble, kind: &str) -> (bool, fauna_protocol::Value) {
    double.kinds.lock().unwrap().push(kind.to_string());

    if !double.serving.load(Ordering::SeqCst) {
        return (
            false,
            value_of(&RpcError::new(
                "fauna.test.not_serving",
                "error.test.not_serving",
            )),
        );
    }

    match kind {
        "fauna.folders.list" => (
            true,
            value_of(&FoldersListReply {
                folders: double.folders.lock().unwrap().clone(),
                ..Default::default()
            }),
        ),
        // This seat's archive place ([`served_roster`]): `SeatRead::find`
        // answers its flags, which resolve `Backup`. An archive place also
        // resolves `accepts: Some(true)`, which is what keeps the arm's pull
        // from being skipped upstream.
        "fauna.folders.members.list" => (
            true,
            value_of(&MembersListReply {
                members: served_roster(&double.device_id_hex.lock().unwrap()),
                ..Default::default()
            }),
        ),
        // The ACTOR roster, which the arm's third job reads
        // (`refresh_share_writer_roster` → `peer_share_store::refresh_writer_roster_via`).
        // One owner, who is a writer by definition — so a cache that names
        // `PEER_ACTOR` a writer can only have come from a successful read of
        // THIS reply. Served on every build, not just `p2p-share` ones: the
        // engine's custody layer probes this kind on the pull path too, and a
        // nest answers it either way.
        "fauna.folders.members.list_actors" => (
            true,
            value_of(&ActorMembersListReply {
                members: vec![FolderActorMember {
                    actor_id: PEER_ACTOR.to_string(),
                    role: "owner".to_string(),
                    ..FolderActorMember::default()
                }],
                ..Default::default()
            }),
        ),
        // An empty feed. The pull's *value* is not this test's subject — that it
        // HAPPENED is, and the request frame is the evidence.
        "fauna.sync.changes.list" => (
            true,
            value_of(&SyncChangesListReply {
                changes: vec![],
                ..Default::default()
            }),
        ),
        // Anything else the loop asks for — the custody layer probes the actor
        // roster on this path, for one — is refused rather than left
        // unanswered: an unanswered request parks until its kind deadline,
        // which would turn an unexpected call into a slow test instead of a
        // legible one.
        _ => (
            false,
            value_of(&RpcError::new(
                "fauna.test.unexpected_kind",
                "error.test.unexpected_kind",
            )),
        ),
    }
}

/// A typed reply body as the dispatcher wants it: canonical CBOR bytes decoded
/// back to the generic dag-cbor node a `Frame::Reply` carries.
fn value_of<T: serde::Serialize>(v: &T) -> fauna_protocol::Value {
    let bytes = fauna_core::encoding::canonical_encode(v).unwrap();
    fauna_core::encoding::canonical_decode(&bytes).unwrap()
}

/// Wait until `cond` holds, polling on a generous budget (convention 14: a
/// deadline poll on state, never a settle-sleep sized to a machine).
async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + BUDGET;
    loop {
        if cond() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {BUDGET:?} waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await; // sleep-ok: deadline poll, not a settle (convention 14)
    }
}

/// One seat, stood up the way both pins below need it: a real `NestClient`
/// whose socket is [`NestDouble`], a real supervisor driving it, and an engine
/// bound to `FOLDER` over that control plane.
///
/// Held together in one struct because the pieces have to outlive the test body
/// — drop the supervisor handle or the `TempDir` early and the loop starts
/// failing for reasons that have nothing to do with the arm.
struct Harness {
    double: Arc<NestDouble>,
    engine: Arc<SyncEngine>,
    /// A second handle on the SAME channel the supervisor publishes to, so a
    /// test can publish the transition itself. The supervisor's own connect
    /// edge lands before the loop subscribes (`conn_rx` is taken at entry,
    /// after the first resolution), and re-deriving a real reconnect through
    /// the supervisor would make these tests fail for supervisor reasons as
    /// well as arm reasons — the arm's trigger is already pinned by
    /// `connected_arm_tests`; what is under test here is its body.
    state_tx: tokio::sync::watch::Sender<ConnectionState>,
    watch_dir: tempfile::TempDir,
    server_task: tokio::task::JoinHandle<()>,
    _supervisor: tokio::task::JoinHandle<()>,
}

fn stand_up() -> Harness {
    stand_up_bound(fauna_core::folder_keys::FolderRef::Local(
        served_folder().id,
    ))
}

/// [`stand_up`], with the engine's binding edge naming `folder_ref` — the key
/// its per-tick row read resolves by (`SeatRowKey::Ref`), exactly as the
/// resident agent installs it.
fn stand_up_bound(folder_ref: fauna_core::folder_keys::FolderRef) -> Harness {
    let double = NestDouble::new();
    let watch_dir = tempfile::tempdir().unwrap();

    let auth = Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret([9u8; 32]),
    ));
    let client = NestClient::with_auth(Arc::clone(&auth));
    let (slot, supervisor_state_tx) = client.supervisor_channels_for_test();
    let (_, state_tx) = client.supervisor_channels_for_test();

    let (adapter, mut server) = mpsc_pair();
    let server_double = Arc::clone(&double);
    let server_task = tokio::spawn(async move {
        while let Some(bytes) = server.rx_from_client.recv().await {
            let Frame::Request(req) = decode_frame(&bytes).expect("decodable frame") else {
                continue; // pushes/acks are not this double's business
            };
            let (ok, payload) = reply_for(&server_double, &req.kind);
            let reply = Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id: req.correlation_id,
                payload,
                ok,
            });
            if server
                .tx_to_client
                .send(encode_frame(&reply).unwrap())
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let channel = Arc::new(MockClientChannel::new(
        connect_queue([Ok(adapter)]),
        Arc::clone(&auth),
        PushBroker::new(16),
    ));
    // The supervisor's own exit is not this module's subject — a `SubprotocolMismatch`
    // teardown is `fauna-ws-substrate`'s to pin — so its `Result` is dropped here
    // rather than widening the handle's type through the harness.
    let supervisor = tokio::spawn(async move {
        let _ = run_supervisor(Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: supervisor_state_tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        })
        .await;
    });

    // `SyncEngine` is `!Sync` (its `SyncDb` wraps a raw `rusqlite::Connection`);
    // the harness's loop future is deliberately `!Send` and raced in-task
    // (never spawned), so this `Arc` never crosses a thread boundary.
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = Arc::new(
        test_engine_with_nest_client(watch_dir.path().to_path_buf(), FOLDER, Arc::clone(&client))
            .with_binding_edge(crate::binding_edge::BindingEdge {
                folder_ref,
                basis: crate::binding_edge::BindingBasis::of(&served_folder()),
                on_rebuild: Arc::new(|| {}),
            }),
    );
    *double.device_id_hex.lock().unwrap() = engine.device_id_hex().to_string();

    Harness {
        double,
        engine,
        state_tx,
        watch_dir,
        server_task,
        _supervisor: supervisor,
    }
}

/// **The whole arm, in one pass.** A seat that resolved `Unresolved` at loop
/// entry — because its own control plane was not answering yet — re-resolves
/// **and pulls** when the plane reaches `Connected`, with the rescan tick an
/// hour away.
///
/// Three separate production lines are load-bearing here, and each fails this
/// test on its own (verified by reverting each in turn):
/// `engine.refresh_sync_mode()` in the arm (the mode assertion), the
/// `engine.pull_remote_changes()` under it (the request-count assertion), and
/// the `conn_rx` subscription itself (both, since an unsubscribed `conn_rx`
/// parks the arm for good — see the module doc on what that third mutation can
/// and cannot show).
#[tokio::test]
async fn the_connected_arm_re_resolves_the_seat_and_pulls() {
    let h = stand_up();
    let double = Arc::clone(&h.double);
    let probe = Arc::clone(&h.engine);
    let test_state_tx = h.state_tx.clone();

    // The loop future is deliberately `!Send`, so it is raced in-task rather
    // than spawned; dropping it at test end is exactly the caller contract.
    let loop_fut = crate::always_resident::run_watch_loop(
        Arc::clone(&h.engine),
        h.watch_dir.path().to_path_buf(),
        FOLDER.to_string(),
        NO_TICK,
        None,
        None,
    );
    tokio::pin!(loop_fut);

    // ── 1. Entry, against a plane that answers nothing ──
    //
    // The entry pull is the barrier, not a sleep: it is issued *after* the
    // entry resolution has installed its answer AND after `conn_rx` has been
    // subscribed, so seeing its request frame proves both — which is precisely
    // what makes the transition published below reach a live arm.
    let entry = Arc::clone(&double);
    race(
        &mut loop_fut,
        until("the entry pull to be issued", || {
            entry.count("fauna.sync.changes.list") >= 1
        }),
    )
    .await;

    assert_eq!(
        probe.sync_mode_resolution(),
        ModeResolution::Unresolved,
        "a seat whose first resolution raced its own connect must land \
         Unresolved — this is the state the arm exists to heal, and if the \
         entry read succeeded here the rest of this test proves nothing"
    );
    // Everything logged from here on can only have come from the transition.
    //
    // The index, rather than a total count, is what makes this deterministic:
    // whether entry's two authoritative reads reach the wire at all depends on
    // whether the supervisor has landed by then. `SyncEngine::resolve_sync_mode`
    // skips both reads outright unless the plane is *currently* `Connected`
    // ("a control plane that is not currently connected fails the reads by
    // definition"), so entry issues either two failing reads or none — and
    // resolves `Unresolved` either way, which is the whole of the production
    // race. A total count would make this test's verdict depend on that
    // timing; the tail does not.
    let before_transition = double.log().len();

    // ── 2. The role becomes readable, and the plane reaches Connected ──
    double.serving.store(true, Ordering::SeqCst);
    test_state_tx.send(ConnectionState::Disconnected).unwrap();
    test_state_tx.send(ConnectionState::Connected).unwrap();

    // ── 3. The arm re-resolves… ──
    let probe_for_mode = Arc::clone(&probe);
    race(
        &mut loop_fut,
        until("the seat to be re-resolved", || {
            probe_for_mode.sync_mode_resolution() != ModeResolution::Unresolved
        }),
    )
    .await;

    assert_eq!(
        probe.sync_mode_resolution(),
        ModeResolution::Resolved(SyncMode::Backup),
        "the arm must install what the nest roster says (an archive place), \
         not the constructor's starting position"
    );

    // ── 4. …and pulls ──
    //
    // Without this the held tombstone waits out the rescan tick — an hour away
    // here, 300 s in production, and forever on any deployment whose window
    // matters more than its cadence.
    let pull_double = Arc::clone(&double);
    race(
        &mut loop_fut,
        until("the reconnect pull to be issued", || {
            pull_double.log()[before_transition..]
                .iter()
                .any(|k| k == "fauna.sync.changes.list")
        }),
    )
    .await;

    // And the resolution the arm ran was a real one: BOTH authoritative reads
    // went out on this transition, neither answered off anything cached.
    let log = double.log();
    let after = &log[before_transition..];
    assert!(
        after.iter().any(|k| k == "fauna.folders.list"),
        "the arm's refresh must read the folder row (log after the transition: \
         {after:?})"
    );
    assert!(
        after.iter().any(|k| k == "fauna.folders.members.list"),
        "…and the device roster with it — the two reads are one resolution \
         (log after the transition: {after:?})"
    );

    h.server_task.abort();
}

/// **The arm's third job: the share leg's writer roster.**
///
/// This is the arm's *original* reason for existing — closing the window in
/// which a member's cached writer roster stays empty, and an empty roster
/// "refuses **every** row **every** peer serves, before weighing any of them"
/// (`next_connected`'s own doc; `p2p-shared-set-build.md` § Build design — the row half). It had
/// never had a live red: `refresh_share_writer_roster` silently no-ops unless
/// the plane is `Connected`, which is exactly what makes the transition the
/// only trigger that can be relied on, and exactly what made it unobservable
/// without a control plane to transition.
///
/// The observable is fail-closed by construction: `cached_share_writer` answers
/// `false` for an unread roster and for a roster naming no writer alike, so the
/// `false → true` flip across the transition can only be a successful read.
#[cfg(feature = "p2p-share")]
#[tokio::test]
async fn the_connected_arm_refreshes_the_share_writer_roster() {
    let h = stand_up();
    let loop_fut = crate::always_resident::run_watch_loop(
        Arc::clone(&h.engine),
        h.watch_dir.path().to_path_buf(),
        FOLDER.to_string(),
        NO_TICK,
        None,
        None,
    );
    tokio::pin!(loop_fut);

    let entry = Arc::clone(&h.double);
    race(
        &mut loop_fut,
        until("the entry pull to be issued", || {
            entry.count("fauna.sync.changes.list") >= 1
        }),
    )
    .await;

    assert!(
        !h.engine.db().cached_share_writer(PEER_ACTOR).unwrap(),
        "the roster must be empty at entry — otherwise the flip below proves \
         nothing about the transition"
    );

    h.double.serving.store(true, Ordering::SeqCst);
    h.state_tx.send(ConnectionState::Disconnected).unwrap();
    h.state_tx.send(ConnectionState::Connected).unwrap();

    let db_probe = Arc::clone(&h.engine);
    race(
        &mut loop_fut,
        until("the writer roster to be cached", || {
            db_probe.db().cached_share_writer(PEER_ACTOR).unwrap()
        }),
    )
    .await;

    h.server_task.abort();
}

// ── the live seat resolver arms the plaintext arm only on the owner's
//    attestation ──

/// The seat's own identity — `stand_up`'s `AuthClient` keypair, and so the
/// trusted owner of the folder the harness binds.
fn seat_keypair() -> ActorKeypair {
    ActorKeypair::from_secret([9u8; 32])
}

/// [`served_folder`] as the nest reports it `public`, attested (or not) as the
/// test says.
fn public_served_folder(attest_by: Option<&ActorKeypair>, counter: u64) -> FolderSummary {
    let mut row = served_folder();
    row.audience = fauna_protocol::folders::AUDIENCE_PUBLIC.to_string();
    row.audience_attestation = attest_by.map(|kp| {
        fauna_protocol::folders::AudienceAttestation::mint(kp, row.id, &row.name, counter, None)
    });
    row
}

/// Wait for the harness's control plane to reach `Connected`, so
/// `SyncEngine::resolve_sync_mode` performs its reads instead of skipping them.
async fn until_connected(engine: &SyncEngine) {
    let control = engine.control_plane();
    until("the control plane to connect", || {
        matches!(
            *control.connection_state().borrow(),
            ConnectionState::Connected
        )
    })
    .await;
}

/// **The reader half of the owner-attested declassification, at the live seat
/// resolver** (`encryption-at-rest.md` § Readable classes → *The
/// declassification is owner-ATTESTED*): a nest reporting `public` with no
/// attestation, or with one somebody else signed, leaves this seat SEALED and
/// its corpus untouched; only the owner's genuine attestation over this row
/// arms it; a flip-back burns the counter, so the nest replaying the withdrawn
/// attestation never re-arms this seat, while the owner's honest re-flip does;
/// an unreadable list keeps the last posture, exactly the mode's discipline.
#[tokio::test]
async fn the_seat_arms_the_plaintext_arm_only_on_the_owners_genuine_attestation() {
    let h = stand_up();
    until_connected(&h.engine).await;
    h.double.serving.store(true, Ordering::SeqCst);
    let stranger = ActorKeypair::from_secret([1u8; 32]);

    // A bare `public` claim: the defect, restated by the nest.
    h.double.serve_folder(public_served_folder(None, 1_000));
    h.engine.refresh_sync_mode().await;
    assert!(!h.engine.is_public_audience(), "a bare claim arms nothing");
    assert_eq!(
        h.engine.converge_corpus_to_audience().await.unwrap(),
        0,
        "and the corpus does not move"
    );

    // A stranger's signature.
    h.double
        .serve_folder(public_served_folder(Some(&stranger), 1_000));
    h.engine.refresh_sync_mode().await;
    assert!(
        !h.engine.is_public_audience(),
        "a forged attestation arms nothing"
    );

    // The owner's own.
    h.double
        .serve_folder(public_served_folder(Some(&seat_keypair()), 1_000));
    h.engine.refresh_sync_mode().await;
    assert!(
        h.engine.is_public_audience(),
        "the owner's attestation arms the seat"
    );
    assert_eq!(
        h.engine
            .db()
            .audience_attestation_memory()
            .unwrap()
            .as_deref(),
        Some("1000:1000"),
        "the seat remembers what it armed under"
    );

    // The flip-back: sealed, and the armed counter burns.
    let mut private = public_served_folder(Some(&seat_keypair()), 1_000);
    private.audience = fauna_protocol::folders::AUDIENCE_PRIVATE.to_string();
    h.double.serve_folder(private);
    h.engine.refresh_sync_mode().await;
    assert!(!h.engine.is_public_audience());
    assert_eq!(
        h.engine
            .db()
            .audience_attestation_memory()
            .unwrap()
            .as_deref(),
        Some("1001:-"),
        "the burn lifts the floor past the withdrawn counter"
    );

    // Replay: the nest re-serves the pre-flip row verbatim.
    h.double
        .serve_folder(public_served_folder(Some(&seat_keypair()), 1_000));
    h.engine.refresh_sync_mode().await;
    assert!(
        !h.engine.is_public_audience(),
        "the withdrawn attestation never re-arms this seat"
    );

    // The honest re-flip mints above what the nest last served.
    h.double
        .serve_folder(public_served_folder(Some(&seat_keypair()), 5_000));
    h.engine.refresh_sync_mode().await;
    assert!(h.engine.is_public_audience());

    // An unreadable list keeps the armed posture and touches the memory not
    // at all — the mode's own failure discipline.
    h.double.serving.store(false, Ordering::SeqCst);
    h.engine.refresh_sync_mode().await;
    assert!(
        h.engine.is_public_audience(),
        "an unreadable list keeps the posture"
    );
    assert_eq!(
        h.engine
            .db()
            .audience_attestation_memory()
            .unwrap()
            .as_deref(),
        Some("5000:5000")
    );

    h.server_task.abort();
}

/// A **member** row on a seat that holds no MLS state (this harness's engine,
/// like the sync agent's bearer-only engines) has no trusted owner: the
/// nest-filled owner field is not one, so even an attestation genuinely signed
/// by the identity the nest names arms nothing.
#[tokio::test]
async fn a_member_seat_without_an_mls_anchor_stays_sealed_whatever_the_nest_names() {
    let h = stand_up();
    until_connected(&h.engine).await;
    h.double.serving.store(true, Ordering::SeqCst);
    let sharer = ActorKeypair::from_secret([3u8; 32]);

    let mut member = public_served_folder(Some(&sharer), 1_000);
    member.role = Some("member".to_string());
    member.access = Some("writer".to_string());
    member.mls_group_id = Some(hex::encode(b"member-row-group"));
    member.owner_actor_id = Some(sharer.actor_id().to_hex());
    h.double.serve_folder(member);
    h.engine.refresh_sync_mode().await;
    assert!(
        !h.engine.is_public_audience(),
        "no MLS-recorded owner ⇒ no anchor ⇒ sealed, whatever the nest names"
    );

    h.server_task.abort();
}

/// Every event this thread emits, as `message field=value …` — a thread-local
/// capture (`set_default`), so no sibling test's lines land here and none of
/// this test's reach the process-wide `fauna_log` ring another test clears.
#[derive(Clone, Default)]
struct EventCapture(Arc<Mutex<Vec<String>>>);

impl EventCapture {
    fn count(&self, needle: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    fn lines(&self, needle: &str) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains(needle))
            .cloned()
            .collect()
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EventCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Line(String);
        impl tracing::field::Visit for Line {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write as _;
                if field.name() == "message" {
                    let _ = write!(self.0, "{value:?}");
                } else {
                    let _ = write!(self.0, " {}={value:?}", field.name());
                }
            }
        }
        let mut line = Line(String::new());
        event.record(&mut line);
        self.0.lock().unwrap().push(line.0);
    }
}

/// The two lines a real-seat test waits on (`test_folder_bound_flip_back.py`'s
/// member arm): they are its only positive evidence that the member's engine
/// READ the public window and judged it, so "the member's paths stayed sealed"
/// is asserted after the verdict rather than after a delay.
const UNANCHORED_CLAIM: &str = "claims public but this seat holds no trusted owner";
const UNANCHORED_CLAIM_WITHDRAWN: &str = "unanchored public claim was withdrawn";

/// **A member seat with no anchor says so, once per edge**
/// (`encryption-at-rest.md` § Implementation status today, the 2026-10-04
/// ruling: a member seat on an MLS-less host never follows the owner's
/// declassification). The verdict itself is the test above; this pins its
/// LOG: one line when the claim appears, one when it is withdrawn, none on the
/// ticks between, none for an unreadable list (which reads nothing), and none
/// for a row this seat has a trusted owner for — an own row's bare claim is
/// the re-confirm surface's subject, not this line's.
#[tokio::test]
async fn a_member_seat_logs_an_unanchored_public_claim_on_each_edge_only() {
    use tracing_subscriber::prelude::*;

    let h = stand_up();
    until_connected(&h.engine).await;
    h.double.serving.store(true, Ordering::SeqCst);
    let sharer = ActorKeypair::from_secret([3u8; 32]);
    let member_row = |audience: &str| {
        let mut row = public_served_folder(Some(&sharer), 1_000);
        row.audience = audience.to_string();
        row.role = Some("member".to_string());
        row.access = Some("writer".to_string());
        row.mls_group_id = Some(hex::encode(b"member-row-group"));
        row.owner_actor_id = Some(sharer.actor_id().to_hex());
        row
    };

    let capture = EventCapture::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));

    // An OWN row's bare claim: sealed, but anchored — not this line.
    h.double.serve_folder(public_served_folder(None, 1_000));
    h.engine.refresh_sync_mode().await;
    // The member row before the public window.
    h.double
        .serve_folder(member_row(fauna_protocol::folders::AUDIENCE_SHARED));
    h.engine.refresh_sync_mode().await;
    assert_eq!(capture.count(UNANCHORED_CLAIM), 0, "no claim, no line");
    assert_eq!(capture.count(UNANCHORED_CLAIM_WITHDRAWN), 0);

    // The owner publishes: the claim appears — one line, however many ticks.
    h.double
        .serve_folder(member_row(fauna_protocol::folders::AUDIENCE_PUBLIC));
    h.engine.refresh_sync_mode().await;
    h.engine.refresh_sync_mode().await;
    assert!(!h.engine.is_public_audience(), "and the seat stays sealed");
    let claimed = capture.lines(UNANCHORED_CLAIM);
    assert_eq!(claimed.len(), 1, "logged on the transition: {claimed:?}");
    assert!(
        claimed[0].contains("folder=name~") && !claimed[0].contains(FOLDER),
        "the folder name rides redacted: {claimed:?}"
    );

    // An unreadable list reads nothing, so it withdraws nothing.
    h.double.serving.store(false, Ordering::SeqCst);
    h.engine.refresh_sync_mode().await;
    assert_eq!(capture.count(UNANCHORED_CLAIM_WITHDRAWN), 0);
    h.double.serving.store(true, Ordering::SeqCst);

    // The flip-back: the claim is withdrawn — again one line.
    h.double
        .serve_folder(member_row(fauna_protocol::folders::AUDIENCE_SHARED));
    h.engine.refresh_sync_mode().await;
    h.engine.refresh_sync_mode().await;
    let withdrawn = capture.lines(UNANCHORED_CLAIM_WITHDRAWN);
    assert_eq!(
        withdrawn.len(),
        1,
        "logged on the transition: {withdrawn:?}"
    );
    assert!(
        withdrawn[0].contains("folder=name~") && !withdrawn[0].contains(FOLDER),
        "the folder name rides redacted: {withdrawn:?}"
    );
    assert_eq!(
        capture.count(UNANCHORED_CLAIM),
        1,
        "and no second claim line"
    );

    h.server_task.abort();
}

/// **A member seat never adopts its namesake's audience**
/// (`on-demand-files.md` § Hosting multiple on-demand folders: names are
/// unique only per owner; the ref is the binding's only key). The seat's user
/// owns a `public`, genuinely owner-attested `holiday` AND is a writer member
/// of someone else's sealed `holiday`; the nest lists the OWN row first. The
/// member engine — bound by ref to the shared row — must judge the shared row
/// through `judge_seat_declassification` and stay sealed. A name-keyed read
/// adopts the own row, whose attestation verifies under this seat's own
/// anchor, and arms the plaintext upload arm on the shared set: its chunks and
/// manifest would land unsealed before the nest refused the record.
#[tokio::test]
async fn a_member_seat_never_arms_on_its_users_same_named_public_set() {
    use fauna_core::folder_keys::FolderRef;

    let own_public = public_served_folder(Some(&seat_keypair()), 1_000);
    let sharer = ActorKeypair::from_secret([3u8; 32]);
    let shared = FolderSummary {
        id: 2,
        name: FOLDER.to_string(),
        role: Some("member".to_string()),
        access: Some("writer".to_string()),
        mls_group_id: Some(hex::encode(b"shared-set-group")),
        owner_actor_id: Some(sharer.actor_id().to_hex()),
        ..FolderSummary::default()
    };
    assert_eq!(own_public.name, shared.name, "the namesakes share a name");
    let listed = vec![own_public.clone(), shared.clone()];

    // The member seat, bound to the SHARED row.
    let member = stand_up_bound(FolderRef::Local(shared.id));
    until_connected(&member.engine).await;
    member.double.serving.store(true, Ordering::SeqCst);
    member.double.serve_folders(listed.clone());
    assert_eq!(
        member.engine.resolve_sync_mode().await.public_audience,
        Some(false),
        "the member seat judges the shared, sealed row — never its namesake"
    );
    member.engine.refresh_sync_mode().await;
    assert!(
        !member.engine.is_public_audience(),
        "so its uploads keep sealing"
    );

    // Control: over the very same list, the seat bound to the OWN row arms —
    // the own row's attestation genuinely verifies here, so the member's
    // sealed verdict above is the key's doing, not a dud row's.
    let owner = stand_up_bound(FolderRef::Local(own_public.id));
    until_connected(&owner.engine).await;
    owner.double.serving.store(true, Ordering::SeqCst);
    owner.double.serve_folders(listed);
    owner.engine.refresh_sync_mode().await;
    assert!(
        owner.engine.is_public_audience(),
        "the own public row arms the seat bound to it"
    );

    member.server_task.abort();
    owner.server_task.abort();
}

/// **A ref the list does not carry reads sealed — and disarms**: a successful list with no row for the bound
/// ref is an absent row, `public_audience == Some(false)`, never the last
/// posture (that is `None`, an unreadable list's answer only). An ARMED seat
/// whose own row leaves the list therefore disarms, even while its namesake —
/// a public, owner-attested row of the same name — is still listed first.
#[tokio::test]
async fn a_seat_whose_ref_the_list_does_not_carry_reads_sealed_and_disarms() {
    use fauna_core::folder_keys::FolderRef;

    let own_public = public_served_folder(Some(&seat_keypair()), 1_000);
    let h = stand_up_bound(FolderRef::Local(own_public.id));
    until_connected(&h.engine).await;
    h.double.serving.store(true, Ordering::SeqCst);
    h.double.serve_folders(vec![own_public.clone()]);
    h.engine.refresh_sync_mode().await;
    assert!(h.engine.is_public_audience(), "armed on its own row");

    // Its row is gone; a same-named public row of another id is listed.
    let mut namesake = public_served_folder(Some(&seat_keypair()), 2_000);
    namesake.id = own_public.id + 100;
    namesake.audience_attestation = Some(fauna_protocol::folders::AudienceAttestation::mint(
        &seat_keypair(),
        namesake.id,
        &namesake.name,
        2_000,
        None,
    ));
    h.double.serve_folders(vec![namesake]);
    assert_eq!(
        h.engine.resolve_sync_mode().await.public_audience,
        Some(false),
        "a successful list without the bound ref is an absent row — sealed"
    );
    h.engine.refresh_sync_mode().await;
    assert!(
        !h.engine.is_public_audience(),
        "so the armed seat disarms rather than keeping its posture"
    );

    h.server_task.abort();
}

/// Drive `fut` while the loop future runs, so the loop keeps making progress.
///
/// `run_watch_loop`'s future is `!Send` and owns the folder watcher, so it
/// cannot be spawned; every wait in this module therefore races it, which is
/// also the caller contract (`mass_delete_floor_test` does the same).
async fn race<L: std::future::Future<Output = ()>>(
    loop_fut: &mut std::pin::Pin<&mut L>,
    waiter: impl std::future::Future<Output = ()>,
) {
    tokio::select! {
        _ = loop_fut.as_mut() => panic!("the resident loop exited on its own"),
        () = waiter => {}
    }
}
