//! A group set end to end — two engines, one ceremony-born scope, a REAL
//! peer channel, NO nest (`docs/goal/behavior/p2p-shared-set-build.md`
//! § Build design — the group set, decision 7).
//!
//! Two real `fauna-account-store` replicas of two different accounts go
//! through the offline share ceremony (the shared `group_seats` fixtures);
//! each binds a location under the scope through the production engine build
//! (`engine_lifecycle::build_engine`'s group arm, its ring read from its own
//! store) and its production share seat on an in-memory transport, serving
//! the set under the scope id. One share-plane pass is the group leg's own
//! steps: the lift of the writer's pending rows onto the plane
//! (`group_pump::lift_group_set`), then the reader's walk of the writer's
//! scope and the fold of the merged rows into its engine with the bodies
//! pulled over the share doors (`group_pump::pull_group_scope`). Every nest a
//! seat holds a client for is a tripwire that fails the test if dialled.
//!
//! Tier 1 (`architecture/testing.md` § The four-tier taxonomy): in-process,
//! no nest, no driver.

#![cfg(all(
    feature = "p2p-share",
    feature = "account-runtime",
    feature = "engine-lifecycle"
))]

mod group_seats;
use group_seats::wire::{SeatStore, bind_seat};
use group_seats::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fauna_client_capabilities::group_ceremony_node::CeremonyNode;
use fauna_core::device_endpoints::DeviceEndpointsEntry;
use fauna_core::feature_gate::{GatedFeature, effective_policy};
use fauna_core::folder_keys::{FolderRef, SetContentKeys};
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_content::GroupContentKeys;
use fauna_core::group_generation::GroupHeldRootRecord;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::scope::GroupScope;
use fauna_sync_engine::db::RetainedLoser;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::engine_lifecycle::{EngineCredential, EngineParams, build_engine};
use fauna_sync_engine::group_pump::{
    GroupFoldLeg, GroupFoldOutcome, group_dial_targets, lift_group_set, pull_group_scope,
};
use fauna_sync_engine::group_set::group_set_keys;
use fauna_sync_engine::provider_face::owned_tree::OwnedTree;
use fauna_sync_engine::share_body::{OnDemandBodySource, TreeBodySource};
use fauna_sync_engine::share_landing::Landing;
use fauna_sync_engine::share_pump::{
    ShareIngestDoor, ShareIngestSummary, SharedSetSpec, TransferUsageLedger, refresh_serve_sources,
};
use fauna_transport::testing::Listeners;

/// A "nest" that only notices being dialled: every accept flips the flag.
async fn nest_tripwire() -> (String, Arc<AtomicBool>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dialled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&dialled);
    tokio::spawn(async move {
        while listener.accept().await.is_ok() {
            flag.store(true, Ordering::SeqCst);
        }
    });
    (format!("http://{addr}"), dialled)
}

/// The engine build's key reader: the seat's own ring for the scope, read
/// from its store as the agent's `PlaneFolderKeys` reads it, and no folder
/// custody at all.
struct SeatRing(GroupContentKeys);

#[async_trait::async_trait]
impl fauna_client_folders::FolderKeyReader for SeatRing {
    async fn load(&self) -> anyhow::Result<fauna_core::data::FoldersConfig> {
        anyhow::bail!("a group seat's build reads no folder custody")
    }
    async fn group_set_keys(&self, _scope: &[u8; 32]) -> anyhow::Result<Option<GroupContentKeys>> {
        Ok(Some(self.0.clone()))
    }
}

/// One engine on a thread of its own — a `SyncEngine` is not `Sync`, so,
/// like the agent's command arm, every call reaches it as a command.
enum EngineCmd {
    Ingest {
        actor: String,
        rows: Vec<Vec<u8>>,
        spool: PathBuf,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<ShareIngestSummary>>,
    },
    Upload {
        path: String,
        reply: tokio::sync::oneshot::Sender<()>,
    },
    Delete {
        path: String,
        reply: tokio::sync::oneshot::Sender<()>,
    },
    Pending {
        reply: tokio::sync::oneshot::Sender<usize>,
    },
    /// A document created through the owned tree, as Files or the SAF
    /// browser creates one: create, open for write, write, close → ingest.
    TreeWrite {
        path: String,
        bytes: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<()>,
    },
    /// The host's start sweep over its owned tree.
    Sweep {
        reply:
            tokio::sync::oneshot::Sender<fauna_sync_engine::provider_face::owned_tree::SweepReport>,
    },
    /// The set's unresolved retained-loser rows — the review list's, or with
    /// `superseded` the base-matched retentions it never lists (decision 10)
    /// — and, for each, the bytes its retained version holds (decision 9).
    RetainedLosers {
        superseded: bool,
        reply: tokio::sync::oneshot::Sender<Vec<(RetainedLoser, Option<Vec<u8>>)>>,
    },
    /// The review row's "use the other version", on the row at `path`.
    UseOtherVersion {
        path: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Clone)]
struct EngineHandle(tokio::sync::mpsc::UnboundedSender<EngineCmd>);

impl EngineHandle {
    /// `tree` set: the engine is an on-demand replica's, and the tree its one
    /// writer — every page lands through the host's door
    /// (`OwnedTree::share_ingest`), as the FFI host's worker runs it.
    fn spawn(
        engine: SyncEngine,
        tree: Option<OwnedTree>,
    ) -> (EngineHandle, std::thread::JoinHandle<()>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        EngineCmd::Ingest {
                            actor,
                            rows,
                            spool,
                            reply,
                        } => {
                            let r = match &tree {
                                Some(tree) => {
                                    tree.share_ingest(&engine, &actor, &rows, spool).await
                                }
                                None => {
                                    fauna_sync_engine::always_resident::ingest_share_page(
                                        &engine, &actor, &rows, spool,
                                    )
                                    .await
                                }
                            }
                            .map(|(report, cursor)| {
                                ShareIngestSummary {
                                    refused: report.refused as u32,
                                    overlaid: report.overlaid as u32,
                                    materialized: report.materialized as u32,
                                    already_current: report.already_current as u32,
                                    cursor,
                                    storage_limited: report.storage_limited,
                                }
                            });
                            let _ = reply.send(r);
                        }
                        EngineCmd::Upload { path, reply } => {
                            engine.upload_file(&path).await.unwrap();
                            let _ = reply.send(());
                        }
                        EngineCmd::Delete { path, reply } => {
                            engine.handle_delete(&path).await.unwrap();
                            let _ = reply.send(());
                        }
                        EngineCmd::Pending { reply } => {
                            let _ = reply.send(engine.db().own_pending_changes(100).unwrap().len());
                        }
                        EngineCmd::TreeWrite { path, bytes, reply } => {
                            let tree = tree.as_ref().expect("an on-demand member");
                            tree.create(&engine, &path).await.unwrap();
                            let open = tree.open_for_write(&engine, &path).await.unwrap();
                            std::fs::write(&open.path, &bytes).unwrap();
                            tree.closed_write(&engine, &path, &open.base_version)
                                .await
                                .unwrap();
                            let _ = reply.send(());
                        }
                        EngineCmd::Sweep { reply } => {
                            let tree = tree.as_ref().expect("an on-demand member");
                            let _ = reply.send(tree.start_sweep(&engine).await.unwrap());
                        }
                        EngineCmd::UseOtherVersion { path, reply } => {
                            let (id, _) = engine
                                .db()
                                .retained_losers()
                                .unwrap()
                                .into_iter()
                                .find(|(_, loser)| loser.path == path)
                                .expect("a review row at the path");
                            let _ = reply.send(
                                engine
                                    .restore_retained_version(id)
                                    .await
                                    .map_err(|e| e.to_string()),
                            );
                        }
                        EngineCmd::RetainedLosers { superseded, reply } => {
                            let losers = if superseded {
                                engine.db().superseded_retentions().unwrap()
                            } else {
                                engine.db().retained_losers().unwrap()
                            };
                            let _ = reply.send(
                                losers
                                    .into_iter()
                                    .map(|(_, loser)| {
                                        let bytes =
                                            engine.read_retained_version(&loser.retained_manifest);
                                        (loser, bytes)
                                    })
                                    .collect(),
                            );
                        }
                    }
                }
            });
        });
        (EngineHandle(tx), thread)
    }

    async fn ask<T>(&self, cmd: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> EngineCmd) -> T {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.0
            .send(cmd(reply))
            .unwrap_or_else(|_| panic!("engine thread gone"));
        rx.await.unwrap()
    }
}

/// One member's bound location: its engine, its set spec and its share seat.
struct Member<'a> {
    seat: &'a Seat,
    root_row: &'a GroupHeldRootRecord,
    engine: EngineHandle,
    engine_thread: Option<std::thread::JoinHandle<()>>,
    spec: SharedSetSpec,
    node: CeremonyNode,
    watch: PathBuf,
    spool: PathBuf,
}

/// Bind `seat`'s location under the scope: the production build's group arm,
/// keyed by the ring the seat's own store resolves, its state DB and tree
/// under `dir` (so a rebuild over the same `dir` is a restart).
async fn bind_location(
    seat: &Seat,
    scope_id: [u8; 32],
    nest: &str,
    dir: &Path,
    replica: Replica,
) -> (SyncEngine, SharedSetSpec) {
    let ring = group_set_keys(&seat.store, &scope_id).await.unwrap();
    let keypair = ActorKeypair::from_secret(*seat.actor.secret_bytes());
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        nest.to_string(),
        ActorKeypair::from_secret(*seat.actor.secret_bytes()),
        Arc::new(fauna_nest_http::StaticBearer("test.bearer".to_string())),
        reqwest::Client::new(),
    ));
    // An on-demand replica's engine runs over its kept root.
    let watch_dir = dir.join(replica.root());
    std::fs::create_dir_all(&watch_dir).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    let folder_ref = FolderRef::Group(scope_id);
    let built = build_engine(EngineParams {
        state_dir: dir.to_path_buf(),
        watch_dir: watch_dir.clone(),
        folder_ref,
        device_id: [seat.device_seed; 32],
        device_label: None,
        auth,
        nest_rpc: fauna_client::NestClient::new(nest.to_string(), keypair),
        mls: None,
        credential: EngineCredential::BackupKey(fauna_core::crypto::BackupKey::from_bytes(
            [seat.device_seed; 32],
        )),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: Arc::new(SeatRing(ring.clone())),
        reader_hosting: Default::default(),
    })
    .await
    .expect("a member with a tipped ring binds a location under the scope");
    assert!(built.engine.nest_leg_absent());
    let db_path = folder_ref.state_db_path(dir);
    // The serve info a phone's replica access answers
    // (`share_glue::OnDemandShareInfo`): its two roots and the by-policy
    // landing; a desktop location's is the tree and the resident landing.
    let (body, landing): (Arc<dyn fauna_sync_engine::share_body::BodySource>, _) = match replica {
        Replica::Resident => (TreeBodySource::shared(watch_dir), Landing::Resident),
        Replica::OnDemand => (
            Arc::new(OnDemandBodySource::new(
                watch_dir.clone(),
                Some(dir.join("cache")),
                fauna_sync_engine::db::SyncDb::open(&db_path).unwrap(),
            )),
            Landing::OnDemand {
                kept_root: watch_dir,
            },
        ),
    };
    let spec = SharedSetSpec {
        folder: "holiday".into(),
        folder_id: folder_ref.to_wire(),
        set_id: scope_id,
        db_path,
        body,
        landing,
        content_keys: SetContentKeys::Group(ring),
    };
    (built.engine, spec)
}

/// How a member holds the set: a desktop location (the agent's resident
/// tree) or a phone's on-demand replica (an owned tree — `group-set.md`
/// § Build design — the group set, decision 11).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Replica {
    Resident,
    OnDemand,
}

impl Replica {
    /// The engine's root under the member's dir.
    fn root(self) -> &'static str {
        match self {
            Replica::Resident => "watch",
            Replica::OnDemand => "kept",
        }
    }
}

async fn member<'a>(
    seat: &'a Seat,
    root_row: &'a GroupHeldRootRecord,
    scope_id: [u8; 32],
    nest: &str,
    dir: &Path,
    listeners: &Listeners,
) -> Member<'a> {
    member_as(
        seat,
        root_row,
        scope_id,
        nest,
        dir,
        listeners,
        Replica::Resident,
    )
    .await
}

async fn member_as<'a>(
    seat: &'a Seat,
    root_row: &'a GroupHeldRootRecord,
    scope_id: [u8; 32],
    nest: &str,
    dir: &Path,
    listeners: &Listeners,
    replica: Replica,
) -> Member<'a> {
    let (engine, spec) = bind_location(seat, scope_id, nest, dir, replica).await;
    let tree =
        (replica == Replica::OnDemand).then(|| OwnedTree::new(dir.join("kept"), dir.join("cache")));
    let (engine, engine_thread) = EngineHandle::spawn(engine, tree);
    let node = bind_seat(seat, listeners).await;
    // The serve half: this member's set routed under the scope id.
    assert_eq!(refresh_serve_sources(&node, std::slice::from_ref(&spec)), 1);
    Member {
        seat,
        root_row,
        engine,
        engine_thread: Some(engine_thread),
        watch: dir.join(replica.root()),
        spool: dir.join("spool"),
        spec,
        node,
    }
}

/// The ingest door over the member's engine — the agent's `ShareIngest`
/// route's own body (`always_resident::ingest_share_page`).
struct EngineDoor(EngineHandle);

#[async_trait::async_trait]
impl ShareIngestDoor for EngineDoor {
    async fn ingest(
        &self,
        _folder: &str,
        _folder_id: &str,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &Path,
    ) -> anyhow::Result<ShareIngestSummary> {
        let actor = proven_actor_hex.to_string();
        let spool = spool_dir.to_path_buf();
        self.0
            .ask(|reply| EngineCmd::Ingest {
                actor,
                rows,
                spool,
                reply,
            })
            .await
    }
}

impl Drop for Member<'_> {
    /// The engine goes down with its member — a restart's first half.
    fn drop(&mut self) {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        self.engine = EngineHandle(tx);
        if let Some(thread) = self.engine_thread.take() {
            thread.join().unwrap();
        }
    }
}

impl Member<'_> {
    fn store(&self) -> SeatStore<'_> {
        SeatStore {
            seat: self.seat,
            root_row: self.root_row,
        }
    }

    /// Write `path` in this member's tree, as a user would, and let the
    /// engine see it.
    async fn write(&self, path: &str, bytes: &[u8]) {
        std::fs::write(self.watch.join(path), bytes).unwrap();
        let path = path.to_string();
        self.engine
            .ask(|reply| EngineCmd::Upload { path, reply })
            .await;
    }

    async fn delete(&self, path: &str) {
        std::fs::remove_file(self.watch.join(path)).unwrap();
        let path = path.to_string();
        self.engine
            .ask(|reply| EngineCmd::Delete { path, reply })
            .await;
    }

    /// The pass's lift: every pending own row put on the plane, its seq
    /// handed back to the engine.
    async fn lift(&self) -> usize {
        lift_group_set(
            &self.store(),
            &EngineDoor(self.engine.clone()),
            &self.spec,
            &self.spool,
        )
        .await
        .expect("the lift")
    }

    /// The pass's pull of `from`: dial, walk the scope, fold into this
    /// member's engine with the bodies pulled from `from`'s serve.
    async fn pull(
        &self,
        from: &Member<'_>,
    ) -> (
        fauna_sync_engine::account_state_plane::WalkReport,
        GroupFoldOutcome,
    ) {
        let scope_id = self.spec.set_id;
        let rows = self
            .seat
            .store
            .group_scope_states(&GroupScope::new(scope_id).to_string())
            .await
            .unwrap();
        let scope = fauna_account_plane::group_pump::pump_scope(
            scope_id,
            &rows,
            &self.seat.actor.actor_id(),
            &GroupShareConfig::default(),
        )
        .expect("a live member");
        let target = fauna_account_plane::group_pump::GroupPumpScope {
            peers: vec![DeviceEndpointsEntry {
                node_id: from.seat.actor.actor_id().0,
                ..Default::default()
            }],
            ..scope.clone()
        };
        let targets = group_dial_targets(&target, &[], None);
        let policy = effective_policy(GatedFeature::P2pShare, &[], &[]);
        let mut ledger = TransferUsageLedger::default();
        let door = EngineDoor(self.engine.clone());
        let mut leg = GroupFoldLeg {
            spec: &self.spec,
            door: &door,
            spool_root: &self.spool,
            policy: &policy,
            ledger: &mut ledger,
            today: 0,
            own_actor: self.seat.actor.actor_id().0,
        };
        let mut outcomes = pull_group_scope(
            &self.node,
            &self.store(),
            scope_id,
            &scope.witness,
            &targets,
            Some(&mut leg),
        )
        .await;
        assert_eq!(outcomes.len(), 1);
        let outcome = outcomes.remove(0);
        let walked = outcome.result.expect("the walk");
        let folded = outcome
            .fold
            .expect("a bound set folds after its walk")
            .expect("the fold");
        (walked, folded)
    }

    /// Create `path` through this on-demand member's owned tree — the
    /// provider's create, write and close; the close is the ingest.
    async fn tree_write(&self, path: &str, bytes: &[u8]) {
        let (path, bytes) = (path.to_string(), bytes.to_vec());
        self.engine
            .ask(|reply| EngineCmd::TreeWrite { path, bytes, reply })
            .await;
    }

    /// The on-demand host's start sweep.
    async fn sweep(&self) -> fauna_sync_engine::provider_face::owned_tree::SweepReport {
        self.engine.ask(|reply| EngineCmd::Sweep { reply }).await
    }

    async fn pending(&self) -> usize {
        self.engine.ask(|reply| EngineCmd::Pending { reply }).await
    }

    /// The review list's rows: concurrent losses only.
    async fn retained_losers(&self) -> Vec<(RetainedLoser, Option<Vec<u8>>)> {
        self.engine
            .ask(|reply| EngineCmd::RetainedLosers {
                superseded: false,
                reply,
            })
            .await
    }

    /// The review row's one gesture on the row at `path`.
    async fn use_other_version(&self, path: &str) -> Result<(), String> {
        let path = path.to_string();
        self.engine
            .ask(|reply| EngineCmd::UseOtherVersion { path, reply })
            .await
    }

    /// The base-matched retentions the review list never shows (decision 10).
    async fn superseded(&self) -> Vec<(RetainedLoser, Option<Vec<u8>>)> {
        self.engine
            .ask(|reply| EngineCmd::RetainedLosers {
                superseded: true,
                reply,
            })
            .await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_written_at_one_member_materializes_at_the_other_with_no_nest() {
    let (nest, dialled) = nest_tripwire().await;
    let (alice, bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;

    // ── Alice (the authority) writes; the lift puts it on the plane and the
    // engine's pending row is upgraded in place by the writer seq.
    a.write("trip.txt", b"written at alice").await;
    assert_eq!(a.pending().await, 1);
    assert_eq!(a.lift().await, 1);
    assert_eq!(
        a.pending().await,
        0,
        "the lift's seq upgraded the pending row"
    );
    assert_eq!(a.lift().await, 0, "nothing is lifted twice");

    // ── Bob walks Alice's scope and folds: the manifest and chunks are
    // pulled over the share doors with the scope as the set id.
    let (walked, folded) = b.pull(&a).await;
    assert_eq!(walked.refused, 0, "{walked:?}");
    assert_eq!(
        (folded.rows, folded.materialized, folded.refused),
        (1, 1, 0),
        "{folded:?}"
    );
    assert_eq!(
        std::fs::read(b.watch.join("trip.txt")).unwrap(),
        b"written at alice"
    );
    assert_eq!(
        b.pending().await,
        0,
        "a landed row is no local edit to lift"
    );
    b.engine
        .ask(|reply| EngineCmd::Upload {
            path: "trip.txt".into(),
            reply,
        })
        .await;
    assert_eq!(
        b.pending().await,
        0,
        "the watcher's echo of the landing mints nothing"
    );

    // A second pass folds nothing: the projection already names the head.
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.rows, 0, "{folded:?}");

    // ── The other direction: Bob, a member (not the authority), writes;
    // his row carries his roster entry core and Alice's fold honours it.
    b.write("notes.txt", b"bob's notes").await;
    assert_eq!(b.lift().await, 1);
    let (_, folded) = a.pull(&b).await;
    assert_eq!(
        (folded.rows, folded.materialized, folded.refused),
        (1, 1, 0),
        "{folded:?}"
    );
    assert_eq!(
        std::fs::read(a.watch.join("notes.txt")).unwrap(),
        b"bob's notes"
    );

    // ── A delete is a value phase: it propagates and leaves Bob's disk.
    a.delete("trip.txt").await;
    assert_eq!(a.lift().await, 1);
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.rows, 1, "{folded:?}");
    assert!(
        !b.watch.join("trip.txt").exists(),
        "the delete left bob's disk"
    );
    let (_, folded) = b.pull(&a).await;
    assert_eq!(
        folded.rows, 0,
        "a delete the projection holds is not news again"
    );

    tokio::task::yield_now().await;
    assert!(
        !dialled.load(Ordering::SeqCst),
        "a group seat dialled a nest"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_the_projection_and_resumes_the_walk_from_its_frontier() {
    let (nest, dialled) = nest_tripwire().await;
    let (alice, mut bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let first_walk_rows = {
        let b = member(
            &bob,
            &run.bob_root_row,
            run.scope_id,
            &nest,
            bob_dir.path(),
            &listeners,
        )
        .await;
        a.write("one.txt", b"the first").await;
        a.lift().await;
        let (walked, folded) = b.pull(&a).await;
        assert_eq!(folded.materialized, 1, "{folded:?}");
        walked.rows
    };

    // Bob's store and engine come back from disk over the same dirs.
    bob.restart().await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;
    assert_eq!(
        std::fs::read(b.watch.join("one.txt")).unwrap(),
        b"the first",
        "the landed file survived"
    );

    a.write("two.txt", b"the second").await;
    a.lift().await;
    let (walked, folded) = b.pull(&a).await;
    assert!(
        walked.rows < first_walk_rows,
        "the walk resumed from the stored frontier: {} rows now, {first_walk_rows} at first",
        walked.rows
    );
    assert_eq!(
        (folded.rows, folded.materialized),
        (1, 1),
        "only the new file is news to the kept projection: {folded:?}"
    );
    assert_eq!(
        std::fs::read(b.watch.join("two.txt")).unwrap(),
        b"the second"
    );

    tokio::task::yield_now().await;
    assert!(
        !dialled.load(Ordering::SeqCst),
        "a group seat dialled a nest"
    );
}

/// The hex actor id a winning row names its author by.
fn actor_hex(member: &Member<'_>) -> String {
    hex::encode(member.seat.actor.actor_id().0)
}

/// Decision 9 (`p2p-shared-set-build.md` § Build design — the group set):
/// two members put one path concurrently. The plane's `LatestWins` keeps the
/// later-stamped row on both replicas; on the LOSING replica its own write
/// was lifted (no longer unpublished local work) and its projection is clean
/// at its own bytes, so the fold lands the winner's bytes — but only after
/// the loser's author has retained its superseded edit under the state root
/// and recorded the resolved conflict, the row the folder family's nest
/// would hold. No sibling copy appears in the tree. A later edit made over
/// the winner names it as its base (decision 10), so the replica it
/// supersedes retains its bytes without listing a review row.
#[tokio::test(flavor = "multi_thread")]
async fn a_concurrent_put_to_one_path_converges_on_the_later_stamp() {
    let (nest, _dialled) = nest_tripwire().await;
    let (alice, bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;

    a.write("same.txt", b"alice's version").await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    b.write("same.txt", b"bob's later version").await;
    a.lift().await;
    b.lift().await;

    // Alice pulls Bob: his later row wins her cell. Her own bytes — lifted,
    // clean in her projection — are retained and the conflict recorded
    // before his are written.
    let (_, folded) = a.pull(&b).await;
    assert_eq!((folded.rows, folded.materialized), (1, 1), "{folded:?}");
    assert_eq!(
        std::fs::read(a.watch.join("same.txt")).unwrap(),
        b"bob's later version"
    );
    let kept = a.retained_losers().await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    let (loser, bytes) = &kept[0];
    assert_eq!(loser.path, "same.txt");
    assert_eq!(loser.winner_author.as_deref(), Some(actor_hex(&b).as_str()));
    assert!(loser.winner_manifest.is_some(), "the winner is a put");
    assert_ne!(
        loser.winner_manifest.as_deref(),
        Some(loser.retained_manifest.as_str())
    );
    assert_eq!(bytes.as_deref(), Some(&b"alice's version"[..]));
    let visible: Vec<_> = std::fs::read_dir(&a.watch)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    assert_eq!(visible, ["same.txt"], "no conflict copy in the tree");
    // Bob pulls Alice: her earlier row loses his cell; nothing is news, and
    // the winner retains nothing.
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.rows, 0, "{folded:?}");
    assert_eq!(
        std::fs::read(b.watch.join("same.txt")).unwrap(),
        b"bob's later version"
    );
    assert!(b.retained_losers().await.is_empty());

    // An edit not yet lifted is never overwritten: Alice edits again, Bob's
    // row is folded before her lift, and her bytes stand until the plane's
    // merge decides between them. A re-fold records nothing twice.
    a.write("same.txt", b"alice edits again").await;
    let (_, folded) = a.pull(&b).await;
    assert_eq!(folded.materialized, 0, "{folded:?}");
    assert_eq!(
        std::fs::read(a.watch.join("same.txt")).unwrap(),
        b"alice edits again"
    );
    assert_eq!(a.retained_losers().await.len(), 1);
    assert_eq!(a.lift().await, 1);
    // Her later row supersedes Bob's lifted head, and names it as its base:
    // she edited the version she materialized from him. His replica retains
    // his bytes all the same, but as an ordinary supersede — no review row.
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.materialized, 1, "{folded:?}");
    assert_eq!(
        std::fs::read(b.watch.join("same.txt")).unwrap(),
        b"alice edits again"
    );
    assert!(b.retained_losers().await.is_empty());
    let kept = b.superseded().await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        kept[0].0.winner_author.as_deref(),
        Some(actor_hex(&a).as_str())
    );
    assert_eq!(kept[0].1.as_deref(), Some(&b"bob's later version"[..]));
    let visible: Vec<_> = std::fs::read_dir(&b.watch)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    assert_eq!(visible, ["same.txt"], "no conflict copy in the tree");
}

/// Decision 10's case (c), the offline edit over an older head: Bob puts X
/// and Alice materializes it; then each edits over what they hold — Alice
/// over X, Bob over his X with Y — and both lift. Neither edit saw the
/// other, so whichever stamp wins the cell, the loser's replica — whose own
/// lifted head is not the winner's base — lists a concurrent loss, and the
/// winner's lists nothing.
#[tokio::test(flavor = "multi_thread")]
async fn an_offline_edit_over_an_older_head_lists_only_at_the_replica_it_did_not_see() {
    for alice_wins in [true, false] {
        let (nest, _dialled) = nest_tripwire().await;
        let (alice, bob, run) = admitted_pair().await;
        let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let listeners = fauna_transport::testing::listeners();
        let a = member(
            &alice,
            &run.alice_root_row,
            run.scope_id,
            &nest,
            alice_dir.path(),
            &listeners,
        )
        .await;
        let b = member(
            &bob,
            &run.bob_root_row,
            run.scope_id,
            &nest,
            bob_dir.path(),
            &listeners,
        )
        .await;

        b.write("doc.txt", b"X").await;
        assert_eq!(b.lift().await, 1);
        let (_, folded) = a.pull(&b).await;
        assert_eq!(folded.materialized, 1, "{folded:?}");

        // The earlier edit loses the cell.
        let pause = || tokio::time::sleep(std::time::Duration::from_millis(5));
        if alice_wins {
            b.write("doc.txt", b"Y").await;
            pause().await;
            a.write("doc.txt", b"Z").await;
        } else {
            a.write("doc.txt", b"Z").await;
            pause().await;
            b.write("doc.txt", b"Y").await;
        }
        assert_eq!(b.lift().await, 1);
        assert_eq!(a.lift().await, 1);

        let (winner, loser, loser_bytes, winner_bytes) = if alice_wins {
            (&a, &b, &b"Y"[..], &b"Z"[..])
        } else {
            (&b, &a, &b"Z"[..], &b"Y"[..])
        };
        let (_, folded) = loser.pull(winner).await;
        assert_eq!(
            folded.materialized, 1,
            "alice_wins={alice_wins}: {folded:?}"
        );
        assert_eq!(
            std::fs::read(loser.watch.join("doc.txt")).unwrap(),
            winner_bytes
        );
        let kept = loser.retained_losers().await;
        assert_eq!(kept.len(), 1, "alice_wins={alice_wins}: {kept:?}");
        assert_eq!(
            kept[0].0.winner_author.as_deref(),
            Some(actor_hex(winner).as_str())
        );
        assert_eq!(kept[0].1.as_deref(), Some(loser_bytes));
        assert!(loser.superseded().await.is_empty());

        let (_, folded) = winner.pull(loser).await;
        assert_eq!(folded.rows, 0, "alice_wins={alice_wins}: {folded:?}");
        assert!(winner.retained_losers().await.is_empty());
        assert!(winner.superseded().await.is_empty());
    }
}

/// Decision 9's delete arm, with decision 10's base: a winning delete over
/// the replica's own lifted put is applied, never declined — after the put
/// is retained and recorded, the winner named as a delete. A delete made
/// over that very put (its base) is an ordinary supersede: retained, never
/// listed. One made over an older head while the put was written is a
/// concurrent loss: retained and listed.
#[tokio::test(flavor = "multi_thread")]
async fn a_winning_delete_over_a_lifted_own_put_retains_it_then_applies() {
    let (nest, _dialled) = nest_tripwire().await;
    let (alice, bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;

    a.write("plan.txt", b"alice's plan").await;
    assert_eq!(a.lift().await, 1);
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.materialized, 1, "{folded:?}");
    // Bob's head is Alice's row: his delete supersedes nothing of his own.
    b.delete("plan.txt").await;
    assert_eq!(b.lift().await, 1);
    assert!(b.retained_losers().await.is_empty());

    // Bob deleted the version he materialized from Alice — its base is her
    // head: retained as a supersede, listed nowhere, and applied.
    let (_, folded) = a.pull(&b).await;
    assert_eq!(folded.rows, 1, "{folded:?}");
    assert!(
        !a.watch.join("plan.txt").exists(),
        "the winning delete left alice's disk"
    );
    assert!(a.retained_losers().await.is_empty());
    let kept = a.superseded().await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    let (loser, bytes) = &kept[0];
    assert_eq!(loser.path, "plan.txt");
    assert_eq!(loser.winner_manifest, None, "the winner is a delete");
    assert_eq!(loser.winner_author.as_deref(), Some(actor_hex(&b).as_str()));
    assert_eq!(bytes.as_deref(), Some(&b"alice's plan"[..]));
    let (_, folded) = a.pull(&b).await;
    assert_eq!(
        folded.rows, 0,
        "a delete the projection holds is not news again"
    );
    assert_eq!(a.superseded().await.len(), 1);

    // The concurrent twin: Bob materializes Alice's notes and deletes them
    // while Alice — not having seen his delete — edits the same version and
    // lifts first. His later delete's base is the version both held, not her
    // edit: a concurrent loss, retained, listed and applied.
    a.write("notes.txt", b"alice's notes").await;
    assert_eq!(a.lift().await, 1);
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.materialized, 1, "{folded:?}");
    a.write("notes.txt", b"alice's later notes").await;
    assert_eq!(a.lift().await, 1);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    b.delete("notes.txt").await;
    assert_eq!(b.lift().await, 1);
    let (_, folded) = a.pull(&b).await;
    assert_eq!(folded.rows, 1, "{folded:?}");
    assert!(!a.watch.join("notes.txt").exists());
    let kept = a.retained_losers().await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].0.path, "notes.txt");
    assert_eq!(kept[0].0.winner_manifest, None, "the winner is a delete");
    assert_eq!(kept[0].1.as_deref(), Some(&b"alice's later notes"[..]));
    assert_eq!(a.superseded().await.len(), 1, "the first supersede stands");
}

/// The leaving delete (`group-set.md` decision 5, the 2026-11-02 ruling): a
/// path a member's row materialized leaves this replica's disk once that row
/// folds as no row — here, Bob's file stamped after the removal Alice later
/// signs (rule (i), a removed member is honoured as of its removal) — while
/// an own write the lift has not put yet, which has no cell, stays.
///
/// The removal instant is the authority's to sign; signing it before Bob's
/// row's stamp is the stale-replica case in two seats: Alice merged and
/// materialized the row before she knew of a removal that bounds it.
#[tokio::test(flavor = "multi_thread")]
async fn a_path_whose_cell_folds_as_no_row_leaves_the_engine_and_an_unlifted_own_file_stays() {
    use fauna_core::encoding::canonical_encode;
    use fauna_protocol::group_state::KIND_GROUP_ROSTER;
    use fauna_sync_engine::account_state_plane::ItemId;
    use fauna_sync_engine::group_state_plane::{GroupStatePlane, NoFeed};

    let (nest, _dialled) = nest_tripwire().await;
    let (alice, bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;

    // Bob writes; Alice folds it onto her disk.
    b.write("planted.txt", b"bob's row").await;
    assert_eq!(b.lift().await, 1);
    let (_, folded) = a.pull(&b).await;
    assert_eq!(folded.materialized, 1, "{folded:?}");
    assert!(a.watch.join("planted.txt").exists());

    // Alice's own write, never lifted: no cell names it.
    a.write("draft.txt", b"alice's draft").await;
    assert_eq!(a.pending().await, 1);

    // Alice removes Bob as of an instant before his row's stamp.
    let (removal_cell, removal) = fauna_core::group_scope::sign_roster_removal(
        &alice.device(),
        run.bob_entry_id,
        alice.device_cert(),
        1_700_000_200_000,
    );
    {
        let sk = alice.device();
        let root = run.alice_root_row.machinery_root().unwrap();
        GroupStatePlane::new(&alice.store, &NoFeed, &root, &sk, &run.scope_id)
            .unwrap()
            .put(
                &ItemId {
                    kind: KIND_GROUP_ROSTER.into(),
                    key: removal_cell,
                },
                canonical_encode(&removal).unwrap().to_vec(),
                None,
            )
            .await
            .expect("the removal");
    }

    // Alice's next pass: Bob's path folds as no row and leaves her engine.
    let (_, folded) = a.pull(&b).await;
    assert_eq!(folded.rows, 1, "one leaving delete: {folded:?}");
    assert!(
        !a.watch.join("planted.txt").exists(),
        "the path whose cell folds as no row left alice's disk"
    );
    assert!(
        a.retained_losers().await.is_empty(),
        "no foreign row retained"
    );
    assert!(a.superseded().await.is_empty(), "no foreign row retained");
    assert_eq!(
        std::fs::read(a.watch.join("draft.txt")).unwrap(),
        b"alice's draft",
        "an unlifted own file has no cell and stays"
    );
    assert_eq!(
        a.pending().await,
        1,
        "its pending row still waits for the lift"
    );
    let (_, folded) = a.pull(&b).await;
    assert_eq!(
        folded.rows, 0,
        "a leaving delete the projection holds is not news again"
    );
}

/// Decision 9's restore: the losing member's review row re-publishes its
/// retained edit as a new local edit. The fresh stamp makes it the head on
/// both members, the row resolves, and the member it overwrites retains its
/// own version as an ordinary supersede — the new row names that version as
/// its base, so nothing is listed there (decision 10). The retained bytes,
/// named by nothing any more, are gone from the state root.
#[tokio::test(flavor = "multi_thread")]
async fn use_the_other_version_republishes_the_retained_edit_on_both_members() {
    let (nest, _dialled) = nest_tripwire().await;
    let (alice, bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;

    a.write("same.txt", b"alice's version").await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    b.write("same.txt", b"bob's later version").await;
    a.lift().await;
    b.lift().await;
    a.pull(&b).await;
    assert_eq!(a.retained_losers().await.len(), 1);

    a.use_other_version("same.txt").await.expect("the restore");
    assert_eq!(
        std::fs::read(a.watch.join("same.txt")).unwrap(),
        b"alice's version"
    );
    assert!(a.retained_losers().await.is_empty(), "the row resolved");
    assert_eq!(a.lift().await, 1, "one new own row, the restore");

    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.materialized, 1, "{folded:?}");
    assert_eq!(
        std::fs::read(b.watch.join("same.txt")).unwrap(),
        b"alice's version"
    );
    assert!(b.retained_losers().await.is_empty());
    let kept = b.superseded().await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].1.as_deref(), Some(&b"bob's later version"[..]));
    let retained = walk_retained(&a.watch);
    assert!(retained.is_empty(), "{retained:?}");
}

/// Every file under a location's retained-version directory.
fn walk_retained(watch: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, under_retained: bool, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                let retained = under_retained || entry.file_name() == ".fauna-retained";
                walk(&path, retained, out);
            } else if under_retained {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(watch, false, &mut out);
    out
}

/// A restore never overwrites an edit that was not lifted (decision 9 and
/// refinement (3)) and never publishes bytes it does not hold: both refuse,
/// write nothing, and leave the row on the list.
#[tokio::test(flavor = "multi_thread")]
async fn a_restore_keeps_the_row_and_writes_nothing_when_it_cannot_be_safe() {
    let (nest, _dialled) = nest_tripwire().await;
    let (alice, bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;

    a.write("same.txt", b"alice's version").await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    b.write("same.txt", b"bob's later version").await;
    a.lift().await;
    b.lift().await;
    a.pull(&b).await;

    // An edit on the disk no lift has seen.
    std::fs::write(a.watch.join("same.txt"), b"typed just now").unwrap();
    a.use_other_version("same.txt")
        .await
        .expect_err("an unlifted edit stands");
    assert_eq!(
        std::fs::read(a.watch.join("same.txt")).unwrap(),
        b"typed just now"
    );
    assert_eq!(a.retained_losers().await.len(), 1, "the row stays");

    // The retained bytes went missing.
    std::fs::write(a.watch.join("same.txt"), b"bob's later version").unwrap();
    for retained in walk_retained(&a.watch) {
        std::fs::remove_file(retained).unwrap();
    }
    a.use_other_version("same.txt")
        .await
        .expect_err("no bytes to restore");
    assert_eq!(
        std::fs::read(a.watch.join("same.txt")).unwrap(),
        b"bob's later version"
    );
    assert_eq!(a.retained_losers().await.len(), 1, "the row stays");
    assert_eq!(a.lift().await, 0, "nothing was published");
}

/// A row whose winner is a delete has the gesture too: the retained edit
/// comes back over the path the delete emptied, and reaches the deleter.
#[tokio::test(flavor = "multi_thread")]
async fn a_restore_over_a_winning_delete_brings_the_file_back_on_both_members() {
    let (nest, _dialled) = nest_tripwire().await;
    let (alice, bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;

    a.write("notes.txt", b"alice's notes").await;
    assert_eq!(a.lift().await, 1);
    b.pull(&a).await;
    a.write("notes.txt", b"alice's later notes").await;
    assert_eq!(a.lift().await, 1);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    b.delete("notes.txt").await;
    assert_eq!(b.lift().await, 1);
    a.pull(&b).await;
    assert!(!a.watch.join("notes.txt").exists());

    a.use_other_version("notes.txt").await.expect("the restore");
    assert_eq!(
        std::fs::read(a.watch.join("notes.txt")).unwrap(),
        b"alice's later notes"
    );
    assert!(a.retained_losers().await.is_empty());
    assert_eq!(a.lift().await, 1);
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.materialized, 1, "{folded:?}");
    assert_eq!(
        std::fs::read(b.watch.join("notes.txt")).unwrap(),
        b"alice's later notes"
    );
}

/// A member's recreate of a path another member deleted lands there: Bob
/// writes and deletes a path, Alice — pulling after both — writes it again,
/// and Bob's pull materializes her bytes over his tracked delete. The disk
/// is empty under that tombstone, so the landing loses nothing local, and a
/// group set has no nest to converge the path later. Bob's own head is a
/// delete, so nothing of his is retained, and nothing is listed anywhere.
#[tokio::test(flavor = "multi_thread")]
async fn a_recreate_over_a_tracked_delete_materializes_at_the_deleting_member() {
    let (nest, _dialled) = nest_tripwire().await;
    let (alice, bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let b = member(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
    )
    .await;

    b.write("plan.txt", b"bob's plan").await;
    assert_eq!(b.lift().await, 1);
    b.delete("plan.txt").await;
    assert_eq!(b.lift().await, 1);
    a.pull(&b).await;
    assert!(!a.watch.join("plan.txt").exists(), "the delete is the head");

    a.write("plan.txt", b"alice's plan").await;
    assert_eq!(a.lift().await, 1);
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.rows, 1, "{folded:?}");
    assert_eq!(folded.materialized, 1, "{folded:?}");
    assert_eq!(
        std::fs::read(b.watch.join("plan.txt")).unwrap(),
        b"alice's plan",
        "the recreate landed at the member that deleted the path"
    );
    for m in [&a, &b] {
        assert!(m.retained_losers().await.is_empty());
        assert!(m.superseded().await.is_empty());
    }
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.rows, 0, "a landed recreate is not news again");
}

/// A phone's seat is its on-demand replica (`group-set.md` § Build design —
/// the group set, decision 11): the member's set is an owned tree whose
/// engine runs over the kept root, every page landing through the host's
/// door (`OwnedTree::share_ingest`) and every lift reading the replica's
/// state DB. A file written at the resident sharer lands in the phone's kept
/// root and stays there; a file created through the phone's tree is minted
/// at its close, lifted and lands at the sharer; a restart keeps the
/// projection and the landed bodies. (The storage floor, which reads the
/// device's real free space here, is pinned at tier 1 beside the engine's
/// landing.)
#[tokio::test(flavor = "multi_thread")]
async fn an_on_demand_member_lands_lifts_and_keeps_its_projection_across_a_restart() {
    let (nest, dialled) = nest_tripwire().await;
    let (alice, mut bob, run) = admitted_pair().await;
    let (alice_dir, bob_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let listeners = fauna_transport::testing::listeners();
    let a = member(
        &alice,
        &run.alice_root_row,
        run.scope_id,
        &nest,
        alice_dir.path(),
        &listeners,
    )
    .await;
    let cache = bob_dir.path().join("cache");
    {
        let b = member_as(
            &bob,
            &run.bob_root_row,
            run.scope_id,
            &nest,
            bob_dir.path(),
            &listeners,
            Replica::OnDemand,
        )
        .await;

        // ── The sharer writes; the phone's fold lands it in the kept root.
        a.write("trip.txt", b"written at alice").await;
        assert_eq!(a.lift().await, 1);
        let (walked, folded) = b.pull(&a).await;
        assert_eq!(walked.refused, 0, "{walked:?}");
        assert_eq!(
            (folded.rows, folded.materialized, folded.refused),
            (1, 1, 0),
            "{folded:?}"
        );
        assert_eq!(
            std::fs::read(b.watch.join("trip.txt")).unwrap(),
            b"written at alice"
        );
        assert_eq!(b.pending().await, 0, "a landed row is no local edit");
        // The host's start sweep reads the landed head as recorded: nothing
        // minted, nothing demoted to the cache root the OS may reclaim.
        let sweep = b.sweep().await;
        assert!(sweep.pending.is_empty(), "{sweep:?}");
        assert_eq!(b.pending().await, 0, "the sweep minted nothing");
        assert!(b.watch.join("trip.txt").is_file());
        assert!(
            !cache.join("trip.txt").exists(),
            "a group body is never demoted"
        );

        // ── The phone creates a file in Files: the close is the ingest, the
        // lift reads the replica's pending row, and it lands at the sharer.
        b.tree_write("phone.txt", b"from the phone").await;
        assert_eq!(b.pending().await, 1, "the close minted the phone's row");
        assert_eq!(b.lift().await, 1);
        assert_eq!(b.pending().await, 0, "the lift's seq upgraded it in place");
        let (_, folded) = a.pull(&b).await;
        assert_eq!(
            (folded.rows, folded.materialized, folded.refused),
            (1, 1, 0),
            "{folded:?}"
        );
        assert_eq!(
            std::fs::read(a.watch.join("phone.txt")).unwrap(),
            b"from the phone"
        );
        assert!(
            b.watch.join("phone.txt").is_file() && !cache.join("phone.txt").exists(),
            "the phone's own body stays kept: no nest will ever serve it back"
        );
    }

    // ── The phone restarts: store and engine back from disk, the host's
    // sweep re-ingests nothing, and only the sharer's new file is news.
    bob.restart().await;
    let b = member_as(
        &bob,
        &run.bob_root_row,
        run.scope_id,
        &nest,
        bob_dir.path(),
        &listeners,
        Replica::OnDemand,
    )
    .await;
    let sweep = b.sweep().await;
    assert!(sweep.pending.is_empty(), "{sweep:?}");
    assert_eq!(b.pending().await, 0, "the restart re-minted nothing");
    assert_eq!(
        std::fs::read(b.watch.join("trip.txt")).unwrap(),
        b"written at alice",
        "the landed file survived"
    );

    // ── The sharer edits the phone's file over the head it landed: the
    // phone's kept body is its own recorded head (the lift's seq stamped it),
    // not a write intent, so the edit lands over it.
    a.write("phone.txt", b"alice's edit").await;
    assert_eq!(a.lift().await, 1);
    let (_, folded) = b.pull(&a).await;
    assert_eq!(folded.materialized, 1, "{folded:?}");
    assert_eq!(
        std::fs::read(b.watch.join("phone.txt")).unwrap(),
        b"alice's edit"
    );

    a.write("two.txt", b"the second").await;
    a.lift().await;
    let (_, folded) = b.pull(&a).await;
    assert_eq!(
        (folded.rows, folded.materialized),
        (1, 1),
        "only the new file is news to the kept projection: {folded:?}"
    );
    assert_eq!(
        std::fs::read(b.watch.join("two.txt")).unwrap(),
        b"the second"
    );

    tokio::task::yield_now().await;
    assert!(
        !dialled.load(Ordering::SeqCst),
        "a group seat dialled a nest"
    );
}
