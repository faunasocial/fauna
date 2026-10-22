//! The same-account leg's nest-down folder transfer, two engines and no nest
//! (`p2p.md` § Same-account nest-down folder transfer;
//! `p2p-shared-set-build.md` § Build design — the same-account row leg,
//! decisions 4 and 5): a write on device A with no nest answering lands on
//! device B as a PROVISIONAL row with its bodies from A — B's engine pulls it
//! through the sibling registry, over the in-memory transport and A's real
//! peer-sync serve doors — and the nest's sequenced row, when it comes back,
//! retires B's overlay without a fork.
//!
//! The two invariants are asserted, never assumed: a provisional row never
//! enters the nest-sequenced log (B's anchor stays put, and B never records the
//! row as its own — so the nest sequences it once, A's record), and a
//! provisional delete never destroys local bytes. A's own record landing is
//! the recording funnel's (`retain_own_change`, pinned in
//! `fauna-account-store`); no WS-RPC nest runs in this crate, so the nest's
//! return is its sequenced row reaching B.
//!
//! In-crate rather than beside `tests/peer_leg_convergence.rs`: the registry's
//! fill (`SiblingChannels::admitted`) is the dial pass's, crate-private, and
//! the engine fixtures are this crate's own.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fauna_account_store::sqlite::SqliteBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::WriterId;
use fauna_core::crypto::{BackupKey, OwnerSealKey};
use fauna_core::data::{Capability, ContentHash, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
use fauna_core::folder_keys::FolderRef;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_peer_channel::PeerChannel;
use fauna_peer_sync::quota::QuotaConfig;
use fauna_peer_sync::server::{
    NowFn, PeerSyncServer, PeerSyncServerConfig, ServeStoreHandle, start_peer_sync_node,
};
use fauna_peer_sync::{AdmissionViews, admit_over_as};
use fauna_protocol::folders::FolderSummary;
use fauna_protocol::sync_writer_sig::ChangeSigner;
use fauna_transport::testing::{Listeners, MemTransport, await_listening, listeners};
use fauna_transport::{EndpointKey, PathCandidates, PeerTransport};

use crate::adaptive::AdaptiveConcurrency;
use crate::binding_edge::{BindingBasis, BindingEdge};
use crate::db::SyncDb;
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::peer_share_store::{PeerPageOrigin, SyncDbShareStore};
use crate::sibling_chunks::SiblingChannels;
use crate::sibling_source::SiblingChunkSource;
use crate::transfer::TransferPool;

const REL: &str = "notes/cabin.txt";
const BODY: &[u8] = b"written on A while no nest answers\n";
const SET_NONCE: [u8; 32] = [0x4e; 32];
const SET_ID: i64 = 7;
const DEVICE_A: [u8; 32] = [0xa1; 32];
const DEVICE_B: [u8; 32] = [0xb2; 32];

/// The account — and the folder's owner, so both engines sign and read as it.
fn owner() -> ActorKeypair {
    ActorKeypair::from_secret([0x0a; 32])
}

fn folder_ref() -> FolderRef {
    FolderRef::Local(SET_ID)
}

/// Each device's transport identity (its NodeId is its device principal).
fn node_key(device: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[device; 32])
}

fn node_id(device: u8) -> [u8; 32] {
    node_key(device).verifying_key().to_bytes()
}

/// The device's owner-signed admission witness.
fn witness(device: u8) -> EmbedAsBytes {
    let cert = DeviceAuthorization {
        actor_id: owner().actor_id(),
        device_key: node_id(device),
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(&owner(), &cert).expect("sign witness");
    EmbedAsBytes::from_signed(bytes, env)
}

/// One device's replica of the owner-sealed folder: its state DB on disk (the
/// serve doors read it over a second connection, as the agent does), the byte
/// plane at `nest_uri`, a control plane that never connects, and the host's
/// sibling registry.
fn replica(
    watch: &std::path::Path,
    db_path: &std::path::Path,
    device: [u8; 32],
    nest_uri: &str,
    siblings: Arc<dyn SiblingChunkSource>,
) -> SyncEngine {
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        nest_uri.to_string(),
        owner(),
        bearer,
        reqwest::Client::new(),
    ));
    let engine = SyncEngine::new(
        watch.to_path_buf(),
        SyncDb::open(db_path).unwrap(),
        SyncClient::new(auth, &device),
        Some("__test".to_string()),
        device,
        None,
        None,
        Some(OwnerSealKey::Client(BackupKey::from_bytes([11u8; 32]))),
        None,
        None,
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        fauna_client::NestClient::new("http://127.0.0.1:9".to_string(), owner()),
        crate::config::SyncMode::Sync,
    )
    .with_change_signer(Arc::new(ChangeSigner::direct(&owner())), Some(SET_NONCE))
    .with_binding_edge(BindingEdge {
        folder_ref: folder_ref(),
        basis: BindingBasis::of(&FolderSummary {
            id: SET_ID,
            name: "__test".into(),
            ..Default::default()
        }),
        on_rebuild: Arc::new(|| {}),
    })
    .with_sibling_chunks(siblings);
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(owner().actor_id().0),
        ..Default::default()
    });
    engine
}

/// What A's serve doors answer from: its folder DB for rows and manifests,
/// and the bodies its serve core produced for chunks (the agent reaches the
/// serve core through its relay seat; the mint pin covers that it serves).
struct Doors {
    db_path: std::path::PathBuf,
    chunks: HashMap<[u8; 32], Vec<u8>>,
    /// Serve these bytes for every manifest instead — a forging sibling.
    forged_manifest: Option<Vec<u8>>,
}

/// A device's peer-sync node on the shared in-memory network, serving the
/// folder doors from `doors`.
async fn serve(device: u8, net: &Listeners, doors: Doors) -> fauna_peer_channel::PeerNode {
    let dir = tempfile::tempdir().unwrap().keep();
    let store = AccountStore::open(
        SqliteBackend::open(&dir).unwrap(),
        &owner().actor_id_hex(),
        WriterId(node_id(device)),
    )
    .await
    .unwrap();
    let now = Arc::new(AtomicU64::new(2_000));
    let now_fn: NowFn = Arc::new(move || now.load(Ordering::SeqCst));
    let doors = Arc::new(doors);
    let server = Arc::new(PeerSyncServer::new(
        ServeStoreHandle::spawn(store),
        owner().actor_id().0,
        PeerSyncServerConfig {
            display_name: format!("device-{device:02x}"),
            own_witness: witness(device),
            own_witness_kind: fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION.to_string(),
            custody_revoked: None,
            device_removed: None,
            file_chunks: Some({
                let doors = Arc::clone(&doors);
                Arc::new(move |folder: String, key: [u8; 32]| {
                    let body = (folder == folder_ref().to_wire())
                        .then(|| doors.chunks.get(&key).cloned())
                        .flatten();
                    Box::pin(async move { body })
                })
            }),
            folder_changes: Some({
                let doors = Arc::clone(&doors);
                Arc::new(move |folder: String, since: i64, max_rows: u32| {
                    let path = doors.db_path.clone();
                    Box::pin(async move {
                        if folder != folder_ref().to_wire() {
                            return Ok(Vec::new());
                        }
                        SyncDbShareStore::new([0u8; 32], SyncDb::open(&path)?)
                            .rows_since(since, max_rows)
                    })
                })
            }),
            folder_manifests: Some({
                let doors = Arc::clone(&doors);
                Arc::new(move |folder: String, hash: [u8; 32]| {
                    let path = doors.db_path.clone();
                    let forged = doors.forged_manifest.clone();
                    Box::pin(async move {
                        if folder != folder_ref().to_wire() {
                            return None;
                        }
                        if forged.is_some() {
                            return forged;
                        }
                        SyncDbShareStore::new([0u8; 32], SyncDb::open(&path).ok()?)
                            .retained_manifest_bytes(&hash)
                    })
                })
            }),
            quotas: QuotaConfig::default(),
            now: now_fn,
        },
    ));
    let transport = Arc::new(MemTransport {
        me: EndpointKey::from_bytes(node_id(device)),
        listeners: Arc::clone(net),
    });
    let node = start_peer_sync_node(transport, server, &["peer-sync".to_string()])
        .await
        .expect("the capability gate is open in fixtures");
    await_listening(net, &node_id(device)).await;
    node
}

/// `from` dials `to` and runs the mutual admission exchange — the dial pass's
/// `connect_and_admit_sibling`, minus the walk.
async fn admitted_channel(from: u8, to: u8, net: &Listeners) -> Arc<PeerChannel> {
    let transport = MemTransport {
        me: EndpointKey::from_bytes(node_id(from)),
        listeners: Arc::clone(net),
    };
    let conn = transport
        .dial(
            EndpointKey::from_bytes(node_id(to)),
            PathCandidates::default(),
        )
        .await
        .expect("dial");
    let channel = PeerChannel::open(conn).await.expect("channel");
    admit_over_as(
        &channel,
        fauna_protocol::peer_sync::WITNESS_DEVICE_AUTHORIZATION,
        witness(from),
        &owner().actor_id().0,
        2_000,
        AdmissionViews {
            custody_revoked: None,
            device_removed: None,
        },
        None,
    )
    .await
    .expect("mutual admission");
    Arc::new(channel)
}

/// Device A, its write made with the nest down: the engine and the doors its
/// node serves from.
struct WriterA {
    engine: SyncEngine,
    doors: Doors,
    manifest: ContentHash,
    manifest_bytes: Vec<u8>,
    /// The sealed chunks as the nest stores them, once A's record lands.
    nest_chunks: Vec<(ContentHash, Vec<u8>)>,
    _watch: tempfile::TempDir,
    _state: tempfile::TempDir,
}

async fn a_writes_with_the_nest_down() -> WriterA {
    let watch = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let db_path = state.path().join("a.db");
    let engine = replica(
        watch.path(),
        &db_path,
        DEVICE_A,
        "http://127.0.0.1:9",
        SiblingChannels::new(),
    );
    std::fs::create_dir_all(watch.path().join("notes")).unwrap();
    std::fs::write(watch.path().join(REL), BODY).unwrap();
    let outcome = engine.upload_file(REL).await;
    assert!(
        outcome.as_ref().map_or(true, |o| !o.recorded),
        "nothing records with no nest answering"
    );
    let pending = engine.db().own_pending_changes(4).unwrap();
    assert_eq!(pending.len(), 1, "A minted its pending row");
    let manifest_hex = pending[0].manifest_hash.clone().expect("a create");
    let retained = engine
        .db()
        .retained_manifest(&manifest_hex)
        .unwrap()
        .expect("retained manifest");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&retained.bytes).unwrap();
    let mut chunks = HashMap::new();
    let mut nest_chunks = Vec::new();
    for key in manifest.store_keys() {
        let body = engine
            .serve_chunk(&key.digest())
            .await
            .unwrap()
            .expect("A's serve core produces the body");
        chunks.insert(key.digest(), body.clone());
        nest_chunks.push((key, body));
    }
    WriterA {
        engine,
        doors: Doors {
            db_path,
            chunks,
            forged_manifest: None,
        },
        manifest: ContentHash::of_raw(&retained.bytes),
        manifest_bytes: retained.bytes,
        nest_chunks,
        _watch: watch,
        _state: state,
    }
}

/// The real registry, counting the engine's sibling passes (each begins with
/// one `siblings()` read).
struct Counted {
    inner: Arc<SiblingChannels>,
    passes: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl SiblingChunkSource for Counted {
    async fn fetch(
        &self,
        folder: &str,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> HashMap<ContentHash, Vec<u8>> {
        self.inner.fetch(folder, store_keys, relative_path).await
    }
    fn siblings(&self) -> Vec<[u8; 32]> {
        self.passes.fetch_add(1, Ordering::SeqCst);
        self.inner.siblings()
    }
    async fn pull_folder_changes(
        &self,
        sibling: &[u8; 32],
        folder: &str,
        since: i64,
    ) -> anyhow::Result<crate::sibling_source::SiblingRowPage> {
        self.inner.pull_folder_changes(sibling, folder, since).await
    }
    async fn fetch_manifest(&self, folder: &str, hash: &ContentHash) -> Option<Vec<u8>> {
        self.inner.fetch_manifest(folder, hash).await
    }
    fn drop_sibling(&self, sibling: &[u8; 32]) {
        self.inner.drop_sibling(sibling);
    }
    fn admissions(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.inner.admissions()
    }
}

/// Device B: its engine on a byte plane that must never be asked (a mock
/// server with nothing mounted, its requests counted), and its registry.
struct ReaderB {
    engine: SyncEngine,
    siblings: Arc<SiblingChannels>,
    /// How many sibling passes B's engine has begun — the loop test's barrier.
    passes: Arc<std::sync::atomic::AtomicUsize>,
    nest: wiremock::MockServer,
    watch: tempfile::TempDir,
    _state: tempfile::TempDir,
}

async fn reader_b() -> ReaderB {
    let nest = wiremock::MockServer::start().await;
    let watch = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let siblings = SiblingChannels::new();
    let passes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let engine = replica(
        watch.path(),
        &state.path().join("b.db"),
        DEVICE_B,
        &nest.uri(),
        Arc::new(Counted {
            inner: Arc::clone(&siblings),
            passes: Arc::clone(&passes),
        }),
    );
    ReaderB {
        engine,
        siblings,
        passes,
        nest,
        watch,
        _state: state,
    }
}

/// **The convergence pin** — decision 5's sentence: a write on A, no nest
/// anywhere, lands on B as a provisional row with its bodies from A; B's
/// reconcile records nothing of it (the nest will sequence it once — A's
/// record); a provisional delete leaves B's bytes; the nest's sequenced row
/// retires B's overlay with no fork.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_with_no_nest_lands_on_the_sibling_provisionally_and_the_nest_retires_it() {
    let net = listeners();
    let a = a_writes_with_the_nest_down().await;
    let manifest = a.manifest;
    let _a_node = serve(0xa1, &net, a.doors).await;
    let a = AtNest {
        _engine: a.engine,
        manifest_bytes: a.manifest_bytes,
        nest_chunks: a.nest_chunks,
    };
    let b = reader_b().await;

    // Before admission: no sibling, nothing pulled.
    let report = b.engine.pull_sibling_changes().await.unwrap();
    assert_eq!(report.siblings, 0);

    b.siblings
        .admitted(node_id(0xa1), admitted_channel(0xb2, 0xa1, &net).await);
    let report = b.engine.pull_sibling_changes().await.unwrap();
    assert_eq!((report.siblings, report.dropped), (1, 0), "{report:?}");
    assert_eq!(report.materialized, 1, "{report:?}");

    // The row and its bytes arrived — from A, the nest never asked.
    assert_eq!(std::fs::read(b.watch.path().join(REL)).unwrap(), BODY);
    assert!(
        b.engine.db().get_share_overlay(REL).unwrap().is_some(),
        "the row lands PROVISIONAL"
    );
    assert!(b.siblings.tally().chunks > 0, "{:?}", b.siblings.tally());
    assert!(
        b.nest.received_requests().await.unwrap().is_empty(),
        "no nest was asked for anything"
    );
    // Invariant one: never the sequenced log. B's anchor is untouched, and its
    // reconcile records nothing — B is not the row's author.
    assert_eq!(b.engine.db().get_anchor().unwrap(), 0);
    let _ = crate::always_resident::LocalWriteHost::converge(&b.engine, "__test").await;
    assert!(
        b.engine.db().own_pending_changes(4).unwrap().is_empty(),
        "B mints no row of its own for a sibling's write"
    );
    assert!(
        b.nest.received_requests().await.unwrap().is_empty(),
        "B's reconcile uploads nothing"
    );
    // Nor does B's upload door, whoever reaches it: the watcher sees the
    // landing's own write as a file event and drives exactly this call, which
    // a nest answering in the meantime would sequence as B's own row.
    let _ = b.engine.upload_file(REL).await;
    assert!(
        b.engine.db().own_pending_changes(4).unwrap().is_empty(),
        "B's upload door mints no row of its own for a sibling's provisional body"
    );
    assert!(
        b.nest.received_requests().await.unwrap().is_empty(),
        "B's upload door uploads nothing of a sibling's provisional body"
    );
    assert!(
        b.engine.db().get_share_overlay(REL).unwrap().is_some(),
        "the declined upload leaves the row provisional"
    );
    // The sibling's cursor is its own; a pending row moves it nowhere.
    let sibling = PeerPageOrigin::Sibling {
        device_hex: hex::encode(node_id(0xa1)),
    };
    assert_eq!(
        b.engine.share_pull_cursor(&sibling.cursor_key()).unwrap(),
        0
    );

    // The row as A serves it — pending, signed as served.
    let channel = admitted_channel(0xb2, 0xa1, &net).await;
    let page = fauna_peer_sync::pull_folder_changes(&channel, &folder_ref().to_wire(), 0)
        .await
        .unwrap();
    let served = page.changes.into_iter().next().expect("A serves its row");
    assert!(!served.sequenced, "served as pending");

    // The nest comes back: A's record, sequenced, reaches B — the overlay
    // retires and the head stands, no fork. As the nest pull hands it to the
    // apply: sequenced, its sealed path opened and the row verified as this
    // account's own (`fetch_change_batch`, noted here since the apply is driven
    // directly); and the nest holds A's manifest and chunks.
    let mut sequenced = served.change.clone();
    sequenced.seq = 5;
    sequenced.path = Some(REL.to_string());
    b.engine.note_owner_signed(manifest);
    b.engine
        .note_manifest_signer(manifest, fauna_core::file_download::RecordSigner::Current);
    let nest = crate::test_support::MockNest::new();
    nest.store()
        .seed_manifest(manifest, a.manifest_bytes.clone());
    for (key, body) in &a.nest_chunks {
        nest.store().seed_chunk(*key, body.clone());
    }
    nest.mount(&b.nest).await;
    let batch = b
        .engine
        .apply_remote_changes(&[sequenced], 0)
        .await
        .expect("apply the nest's sequenced row");
    assert!(!batch.deferred, "{batch:?}");
    assert!(
        b.engine.db().get_share_overlay(REL).unwrap().is_none(),
        "the provisional annotation retired on arrival"
    );
    let entry = b.engine.db().get_entry(REL).unwrap().expect("entry stands");
    assert_eq!(entry.manifest_hash, Some(manifest), "same head — no fork");
    assert_eq!(std::fs::read(b.watch.path().join(REL)).unwrap(), BODY);
    assert!(
        b.engine.db().own_pending_changes(4).unwrap().is_empty(),
        "sequenced once — A's record, never B's"
    );

    // Invariant two: a provisional delete never destroys local bytes — the
    // sibling-served row, turned delete by its writer.
    let mut delete = served;
    delete.change.change_type = "delete".to_string();
    delete.change.manifest_hash = None;
    delete.change.size_bytes = 0;
    ChangeSigner::direct(&owner())
        .sign_row(&mut delete.change, SET_NONCE)
        .unwrap();
    let (report, _) = crate::always_resident::ingest_peer_page_landing(
        &b.engine,
        &sibling,
        vec![delete],
        &b.engine.sibling_blob_fetcher(),
        None,
    )
    .await
    .unwrap();
    assert_eq!((report.refused, report.overlaid), (0, 1), "{report:?}");
    assert_eq!(
        std::fs::read(b.watch.path().join(REL)).unwrap(),
        BODY,
        "a provisional delete leaves the bytes on disk"
    );
}

/// The accepts gate covers the sibling rail (`local-folder-sync` outcome 15):
/// a seat told not to take changes pulls nothing from a sibling either, and a
/// flip back to accepting catches it up from exactly where it held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seat_that_does_not_accept_pulls_nothing_from_a_sibling() {
    let net = listeners();
    let a = a_writes_with_the_nest_down().await;
    let _a_node = serve(0xa1, &net, a.doors).await;
    let b = reader_b().await;
    b.siblings
        .admitted(node_id(0xa1), admitted_channel(0xb2, 0xa1, &net).await);

    b.engine.set_accepts_remote(false);
    let report = b.engine.pull_sibling_changes().await.unwrap();
    assert_eq!(
        (report.siblings, report.rows, report.materialized),
        (0, 0, 0),
        "a held seat asks no sibling: {report:?}"
    );
    assert!(
        !b.watch.path().join(REL).exists(),
        "the hold did not hold: a sibling's row arrived"
    );

    b.engine.set_accepts_remote(true);
    let report = b.engine.pull_sibling_changes().await.unwrap();
    assert_eq!(report.materialized, 1, "{report:?}");
    assert_eq!(std::fs::read(b.watch.path().join(REL)).unwrap(), BODY);
}

/// What the nest holds of A's write once A's record lands.
struct AtNest {
    _engine: SyncEngine,
    manifest_bytes: Vec<u8>,
    nest_chunks: Vec<(ContentHash, Vec<u8>)>,
}

/// The fourth trigger: a running watch loop pulls a freshly admitted sibling
/// at its admission, not a tick later (the tick here is an hour).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_loop_pulls_a_sibling_at_its_admission() {
    let net = listeners();
    let a = a_writes_with_the_nest_down().await;
    let _a_node = serve(0xa1, &net, a.doors).await;
    let b = reader_b().await;
    let watch = b.watch.path().to_path_buf();
    // The loop takes an `Arc` (its caller-handle contract) though the engine is
    // `!Sync`; it never leaves this task.
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = Arc::new(b.engine);
    // The loop future is deliberately `!Send`: raced in-task, dropped at the end.
    let looped = crate::always_resident::run_watch_loop(
        Arc::clone(&engine),
        watch.clone(),
        "__test".to_string(),
        std::time::Duration::from_secs(3600),
        None,
        None,
    );
    let driver = async {
        // The barrier: the loop's entry sibling pass has run (with no sibling
        // held), so only the admission arm can pull A now — the tick is an hour.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while b.passes.load(Ordering::SeqCst) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the loop never reached its entry pass"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!watch.join(REL).exists(), "nothing before the admission");
        b.siblings
            .admitted(node_id(0xa1), admitted_channel(0xb2, 0xa1, &net).await);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::fs::read(watch.join(REL)).ok().as_deref() != Some(BODY) {
            assert!(
                std::time::Instant::now() < deadline,
                "the admission did not trigger a sibling pull"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    };
    tokio::select! {
        () = looped => panic!("the watch loop returned"),
        () = driver => {}
    }
    drop(a.engine);
}

/// Rule 4 on the manifest door: a sibling serving a manifest that does not
/// hash to the one asked for is dropped, nothing lands, and the nest is the
/// one asked next.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forged_manifest_is_refused_and_its_sibling_dropped() {
    let net = listeners();
    let mut a = a_writes_with_the_nest_down().await;
    a.doors.forged_manifest = Some(b"not the manifest".to_vec());
    let _a_node = serve(0xa1, &net, a.doors).await;
    let b = reader_b().await;
    b.siblings
        .admitted(node_id(0xa1), admitted_channel(0xb2, 0xa1, &net).await);

    let report = b.engine.pull_sibling_changes().await.unwrap();
    assert_eq!(report.materialized, 0, "{report:?}");
    assert!(!b.watch.path().join(REL).exists(), "nothing forged lands");
    assert!(b.siblings.is_empty(), "the forging sibling is dropped");
    assert!(
        b.nest
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path().starts_with("/api/v1/manifests/")),
        "the nest path is asked for the manifest"
    );
    drop(a.engine);
}
