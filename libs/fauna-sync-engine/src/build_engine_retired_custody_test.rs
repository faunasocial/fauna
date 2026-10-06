//! `engine_lifecycle::build_engine`'s retired-serve-custody wiring
//! (`webdav-server.md` § Key model, Revocation) had no witness on the
//! iOS/Android path: a change
//! threads the custody load, the `retired_serve_custody`
//! producer, and `SyncEngine::set_retired_content_keys` into
//! [`build_engine`](crate::engine_lifecycle::build_engine) at three places
//! (`engine_lifecycle.rs:550`, `:580-583`, `:687`), and every piece was pinned
//! in isolation except the wiring itself — `engine_lifecycle.rs`'s own tests
//! never call `build_engine` (its callers are the `fauna-ffi` hosts and the
//! desktop sync agent).
//!
//! Both `retired_content_keys` (the engine field) and `download_keys` (the
//! reader assembly) are private (`engine.rs:744`, `:7549`), so these tests
//! assert BEHAVIORALLY — and since the desktop sync agent builds every engine
//! through `build_engine` too, the seed-less test below is the agent's proof as
//! well (it once carried a twin over its own builder, retired with it): seal
//! a served-era file under [`MockNest`]'s HTTP byte plane, run
//! `reseal_predecessor_sealed`, and check a *new* manifest was posted — proof
//! the wiring actually reaches the shared re-seal walk, not just that a field
//! exists.
//!
//! `build_engine` also needs a live WS-RPC control plane (register,
//! `fauna.folders.list`) that `MockNest` (the HTTP byte plane only) cannot
//! answer, so the harness below stands up a minimal `NestClient` over an
//! in-memory mocked socket — the same technique `connected_arm_heal_test.rs`
//! uses for `run_watch_loop`'s Connected arm, trimmed to the one-shot calls
//! `build_engine` makes (no reconnect/state simulation needed, since nothing
//! here ever fails a request).
//!
//! The same harness pins the seed-less build's custody load
//! (`on-demand-files.md` § Shared sets on a capability host, decisions 1 and
//! 2): a capability host holding only the `BackupKey` reads its account's
//! folder-keys custody itself, for a bound set's generations and for retired serve
//! custody alike, and records the row facts its re-resolve edges compare — and
//! the cross-nest arm (*One mechanism*, question 2): a foreign set built from its
//! holder's custody record alone, custody standing in for the row at the edges.
//!
//! Tier 1 (`architecture/testing.md` § The four-tier taxonomy): in-process,
//! no nest binary, no driver — only the two sockets (WS + HTTP) are fake.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, NestClient, PushBroker};
use fauna_core::crypto::BackupKey;
use fauna_core::data::{ContentHash, FoldersConfig};
use fauna_core::folder_keys::{FolderContentKeys, FolderRef};
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::folders::{FolderSummary, FoldersListReply, KIND_FOLDERS_LIST};
use fauna_protocol::sync::SyncRegisterReply;
use fauna_protocol::{Frame, Reply, RpcError, Value, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};

use crate::db::{SyncDb, SyncState};
use crate::engine_lifecycle::{EngineCredential, EngineParams, build_engine};
use crate::test_support::MockNest;

/// The retired serve-window generation both fixtures below key off: the exact
/// key + timestamp is recreated on the custody side via
/// [`fauna_client_folders::custody::record_new_set`] (deterministic —
/// `FolderContentKeys::genesis` takes no RNG), so the two independently-built
/// values compare equal.
const RETIRED_KEY: [u8; 32] = [0x88u8; 32];
const RETIRED_KEY_MICROS: u64 = 1_000;

/// The account's folder-key custody a build reads (`EngineParams::folder_keys`)
/// — what the host's reader answers, counting its loads so a test can pin how
/// often a build reads custody. `unreadable` models a host that cannot open
/// the account's custody at all (a reader that always answers `Err`).
struct TestCustody {
    held: FoldersConfig,
    unreadable: bool,
    loads: std::sync::atomic::AtomicUsize,
}

impl TestCustody {
    fn holding(held: FoldersConfig) -> Arc<Self> {
        Arc::new(Self {
            held,
            unreadable: false,
            loads: Default::default(),
        })
    }

    fn unreadable() -> Arc<Self> {
        Arc::new(Self {
            held: FoldersConfig::default(),
            unreadable: true,
            loads: Default::default(),
        })
    }

    fn loads(&self) -> usize {
        self.loads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl fauna_client_folders::FolderKeyReader for TestCustody {
    async fn load(&self) -> anyhow::Result<FoldersConfig> {
        self.loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.unreadable {
            anyhow::bail!("this host cannot open the account's custody");
        }
        Ok(self.held.clone())
    }
}

/// Stock the store with `original` sealed under `root` the way the upload
/// path seals it — this crate's own `download_file_bytes_test::
/// store_sealed_fixture`, private to that module. Reproduced here rather than
/// imported: both modules live in this crate, but neither test module is
/// visible to the other (default item visibility).
fn store_sealed_fixture(
    store: &crate::test_support::BlobStore,
    original: &[u8],
    root: &[u8; 32],
) -> ContentHash {
    let plain = fauna_core::chunker::chunk_file(original);
    let mut stored_hashes = Vec::new();
    for (hash, data) in fauna_core::chunker::extract_chunks(original, &plain) {
        let (store_key, ciphertext) = crate::seal::seal_chunk_body(&hash, &data, root).unwrap();
        stored_hashes.push(store_key);
        store
            .chunks
            .lock()
            .unwrap()
            .insert(hex::encode(store_key.digest()), ciphertext);
    }
    let manifest = fauna_core::chunk::ChunkManifest {
        stored_hashes: Some(stored_hashes),
        ..plain
    };
    let manifest_bytes =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(root)).unwrap()).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    store
        .manifests
        .lock()
        .unwrap()
        .insert(hex::encode(manifest_hash.digest()), manifest_bytes);
    manifest_hash
}

/// The minimal WS-RPC control plane `build_engine` needs: `fauna.sync.register`
/// (best-effort, uncounted here) and `fauna.folders.list` (one fixed summary).
/// Answers every kind immediately on the first attempt —
/// unlike `connected_arm_heal_test.rs`'s arm-healing harness, nothing here
/// ever needs to simulate a not-yet-connected nest.
struct TestNest {
    kinds: Mutex<Vec<String>>,
    folder: FolderSummary,
}

impl TestNest {
    fn new(folder: FolderSummary) -> Arc<Self> {
        Arc::new(Self {
            kinds: Mutex::new(Vec::new()),
            folder,
        })
    }

    /// How many times `kind` has been requested since the double was built.
    fn count(&self, kind: &str) -> usize {
        self.kinds
            .lock()
            .unwrap()
            .iter()
            .filter(|k| *k == kind)
            .count()
    }
}

/// A typed reply body as the dispatcher wants it: canonical CBOR bytes decoded
/// back to the generic dag-cbor node a `Frame::Reply` carries.
fn value_of<T: serde::Serialize>(v: &T) -> Value {
    let bytes = fauna_core::encoding::canonical_encode(v).unwrap();
    fauna_core::encoding::canonical_decode(&bytes).unwrap()
}

/// Answer one decoded request frame, logging its kind first.
fn reply_for(nest: &TestNest, kind: &str) -> (bool, Value) {
    nest.kinds.lock().unwrap().push(kind.to_string());
    match kind {
        "fauna.sync.register" => (
            true,
            value_of(&SyncRegisterReply {
                device_id: String::new(),
                extra: Default::default(),
            }),
        ),
        KIND_FOLDERS_LIST => (
            true,
            value_of(&FoldersListReply {
                folders: vec![nest.folder.clone()],
                ..Default::default()
            }),
        ),
        _ => (
            false,
            value_of(&RpcError::new(
                "fauna.test.unexpected_kind",
                "error.test.unexpected_kind",
            )),
        ),
    }
}

/// Stand up a `NestClient` over an in-memory mocked socket answering `nest` —
/// the same wiring `connected_arm_heal_test.rs` uses for its arm-healing
/// harness, trimmed to a socket that is ready from the first request
/// (`build_engine`'s own calls retry through the WS-RPC warm-up, so nothing
/// here needs to simulate that race).
fn stand_up_ws(
    nest: Arc<TestNest>,
) -> (
    Arc<NestClient>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    let auth = Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret([9u8; 32]),
    ));
    let client = NestClient::with_auth(Arc::clone(&auth));
    let (slot, connection_state_tx) = client.supervisor_channels_for_test();

    let (adapter, mut server) = mpsc_pair();
    let server_nest = Arc::clone(&nest);
    let server_task = tokio::spawn(async move {
        while let Some(bytes) = server.rx_from_client.recv().await {
            let Frame::Request(req) = decode_frame(&bytes).expect("decodable frame") else {
                continue; // pushes/acks are not this double's business
            };
            let (ok, payload) = reply_for(&server_nest, &req.kind);
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
    let supervisor = tokio::spawn(async move {
        let _ = run_supervisor(Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        })
        .await;
    });

    (client, server_task, supervisor)
}

/// A byte-plane `AuthClient` that sends a fixed bearer rather than minting one
/// over a real `fauna.auth.handshake` WS round trip — `download_file_bytes_test
/// ::test_sync_client`'s own pattern. `MockNest` is an HTTP-only wiremock
/// double with no WS endpoint at all, so the ordinary `AuthClient::new` (which
/// authenticates by *dialing a websocket* to `nest_url`) 404s before a single
/// chunk request is made.
fn byte_plane_auth(server_uri: &str, keypair: ActorKeypair) -> Arc<AuthClient> {
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    Arc::new(AuthClient::with_bearer_source(
        server_uri.to_string(),
        keypair,
        bearer,
        reqwest::Client::new(),
    ))
}

/// A currently owner-only (group-less), unserved summary for `name` — the
/// shape `decide_engine_content_binding` resolves `Unbound` regardless of
/// whether local/custody evidence says the set was served in the past.
fn owner_only_unserved_summary(id: i64, name: &str) -> FolderSummary {
    FolderSummary {
        id,
        name: name.to_string(),
        ..Default::default()
    }
}

/// `build_engine`'s wiring for a once-served, now-disabled, group-less set
/// (`webdav-server.md` § Key model, Revocation): its custody still holds the
/// serve-window generation, and the local corpus carries one content-key-
/// stamped entry — the one local evidence `build_engine` looks for before
/// paying the custody load. Mutating the trigger (`engine_lifecycle.rs:550`),
/// the producer call (`:580-583`) or the setter (`:687`) must each red this
/// test: with any of them gone, `retired_content_keys` never reaches the
/// engine, so the served-era chunk stays unopenable and
/// `reseal_predecessor_sealed` posts no new manifest.
#[tokio::test]
async fn build_engine_threads_a_retired_generation_for_a_once_stamped_group_less_set() {
    retired_generation_reaches_the_reseal(|secret| EngineCredential::Seed(hex::encode(secret)))
        .await;
}

/// The same wiring on a seed-less capability host: its custody load under the
/// `BackupKey` alone (`on-demand-files.md` § Shared sets on a capability host,
/// decision 1) is the load retired serve custody rides, so the once-served
/// set's back-catalogue re-seals there too.
#[tokio::test]
async fn a_seedless_build_threads_the_retired_generation_from_its_own_custody_load() {
    retired_generation_reaches_the_reseal(|secret| {
        EngineCredential::BackupKey(BackupKey::derive(&secret))
    })
    .await;
}

async fn retired_generation_reaches_the_reseal(credential: fn([u8; 32]) -> EngineCredential) {
    let server = wiremock::MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;

    let secret = [0x37u8; 32];
    let folder_name = "docs";
    let folder_ref = FolderRef::Local(1);
    let served_generation = FolderContentKeys::genesis(RETIRED_KEY, RETIRED_KEY_MICROS);

    let original: Vec<u8> = (0..40_000u32)
        .map(|i| (i.wrapping_mul(7) % 233) as u8)
        .collect();
    let rel = "docs/served-while-on.bin";
    let served_manifest = store_sealed_fixture(&store, &original, served_generation.current_key());

    let state_dir = tempfile::tempdir().unwrap();
    {
        let db = SyncDb::open(folder_ref.state_db_path(state_dir.path())).unwrap();
        db.upsert_entry(
            rel,
            Some(ContentHash::of_raw(&original)),
            Some(ContentHash::of_raw(&original)),
            Some(served_manifest),
            SyncState::Synced,
            1,
            1,
            original.len() as i64,
            1,
            Some(served_generation.current_version()),
        )
        .unwrap();
    }

    let nest = TestNest::new(owner_only_unserved_summary(1, folder_name));
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));

    // Seed the owner's custody with the retired serve-window generation.
    let mut cfg = FoldersConfig::default();
    fauna_client_folders::custody::record_new_set(
        &mut cfg,
        fauna_core::folder_keys::serve_custody_channel_id(folder_name),
        RETIRED_KEY,
        RETIRED_KEY_MICROS,
    );
    let custody = TestCustody::holding(cfg);

    let watch_dir = tempfile::tempdir().unwrap();
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: watch_dir.path().to_path_buf(),
        folder_ref,
        device_id: [6u8; 32],
        device_label: Some("test-device".to_string()),
        auth: byte_plane_auth(&server.uri(), ActorKeypair::from_secret(secret)),
        nest_rpc: Arc::clone(&nest_rpc),
        mls: None,
        credential: credential(secret),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: custody.clone(),
        reader_hosting: Default::default(),
    })
    .await
    .expect("a group-less, unserved set with local content-keyed evidence must still build");

    assert_eq!(
        custody.loads(),
        1,
        "the local content-keyed evidence must trigger exactly one custody load"
    );

    built
        .engine
        .reseal_predecessor_sealed()
        .await
        .expect("re-seal pass");
    let resealed_hash = store.last_manifest_hash.lock().unwrap().expect(
        "build_engine's retired_content_keys must let the shared re-seal walk open and \
         re-upload the served-era file — nothing else exercised the byte plane in this test",
    );
    assert_ne!(
        resealed_hash, served_manifest,
        "a NEW manifest, not the served-era one"
    );

    server_task.abort();
    supervisor.abort();
}

/// A group-less, unserved, **never**-content-keyed set built with **no
/// signer** still pays the one custody load: the READER needs the set's
/// nonce too — it holds every signed row it cannot bind to one, so a
/// signer-less host that skipped custody would list nothing. The seed-less
/// credential is what makes this build unsigned (a `Seed` credential signs
/// with the identity key). The discriminating mutation is gating the custody
/// load on the signer, the binding or the serve flag.
#[tokio::test]
async fn build_engine_reads_custody_for_an_unsigned_owner_only_set_so_its_reader_binds() {
    let secret = [0x55u8; 32];
    let folder_name = "docs";
    let folder_ref = FolderRef::Local(2);

    let state_dir = tempfile::tempdir().unwrap();
    {
        // Open + immediately drop: an empty state db, the shape a fresh
        // device starts with — no entry ever stamped a content-key version.
        SyncDb::open(folder_ref.state_db_path(state_dir.path())).unwrap();
    }

    let nest = TestNest::new(owner_only_unserved_summary(2, folder_name));
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));

    let custody = TestCustody::holding(FoldersConfig::default());
    let watch_dir = tempfile::tempdir().unwrap();
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: watch_dir.path().to_path_buf(),
        folder_ref,
        device_id: [7u8; 32],
        device_label: Some("test-device".to_string()),
        auth: Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret(secret),
        )),
        nest_rpc: Arc::clone(&nest_rpc),
        mls: None,
        credential: EngineCredential::BackupKey(BackupKey::derive(&secret)),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: custody.clone(),
        reader_hosting: Default::default(),
    })
    .await
    .expect("a plain owner-only set must always build");

    assert_eq!(
        custody.loads(),
        1,
        "a signer-less host still reads custody once — its reader needs the set's nonce"
    );

    drop(built);
    server_task.abort();
    supervisor.abort();
}

/// Ruling (8)(b), source (ii), on the engine: a build handed the account's
/// attested predecessor ids binds its reader to them — the current identity as
/// `account`, the ids (and any a paired key names that they lack) as
/// `account_predecessors` — so a retired identity's rows verify as this
/// account's. Before the engine's half it bound `account: None` and no
/// predecessors whatever it was handed.
#[tokio::test]
async fn build_engine_binds_its_reader_to_the_accounts_attested_predecessors() {
    let secret = [0x56u8; 32];
    let folder_ref = FolderRef::Local(2);
    let state_dir = tempfile::tempdir().unwrap();
    SyncDb::open(folder_ref.state_db_path(state_dir.path())).unwrap();
    let nest = TestNest::new(owner_only_unserved_summary(2, "docs"));
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));
    let (attested, paired_only) = ([0xA1u8; 32], [0xA2u8; 32]);
    let watch_dir = tempfile::tempdir().unwrap();
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: watch_dir.path().to_path_buf(),
        folder_ref,
        device_id: [7u8; 32],
        device_label: Some("test-device".to_string()),
        auth: Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret(secret),
        )),
        nest_rpc: Arc::clone(&nest_rpc),
        mls: None,
        credential: EngineCredential::BackupKey(BackupKey::derive(&secret)),
        progress_tx: None,
        predecessor_backup_keys: fauna_core::file_download::PredecessorSealKey::chain([
            (
                fauna_core::identity::ActorId(attested),
                BackupKey::from_bytes([1u8; 32]),
            ),
            (
                fauna_core::identity::ActorId(paired_only),
                BackupKey::from_bytes([2u8; 32]),
            ),
        ]),
        predecessor_actor_ids: vec![attested],
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: TestCustody::holding(FoldersConfig::default()),
        reader_hosting: Default::default(),
    })
    .await
    .expect("a plain owner-only set must always build");

    let binding = built.engine.reader_binding();
    assert_eq!(
        binding.account,
        Some(ActorKeypair::from_secret(secret).actor_id().0)
    );
    assert_eq!(binding.account_predecessors, vec![attested, paired_only]);
    drop(built);
    server_task.abort();
    supervisor.abort();
}

/// Who names the owner on a host that holds no marker
/// (`writer-signed-change-records.md` ruling (11)(c)): a same-nest **member**
/// row built with no MLS engine — every engine the sync agent builds — binds
/// its reader to NO owner, whatever `owner_actor_id` the nest filled on the
/// list row, and never to this account. The judge takes the owner off the
/// roster's owner row instead. Before the build it bound the list row's field.
#[tokio::test]
async fn a_member_row_on_an_mls_less_host_builds_with_no_owner_whatever_the_row_names() {
    let secret = [0x58u8; 32];
    let folder_ref = FolderRef::Local(2);
    let state_dir = tempfile::tempdir().unwrap();
    SyncDb::open(folder_ref.state_db_path(state_dir.path())).unwrap();
    let nest_named = ActorKeypair::from_secret([0x59u8; 32]).actor_id();
    let nest = TestNest::new(FolderSummary {
        role: Some("member".to_string()),
        access: Some("writer".to_string()),
        mls_group_id: Some(hex::encode(b"member-row-group")),
        owner_actor_id: Some(nest_named.to_hex()),
        ..owner_only_unserved_summary(2, "shared")
    });
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));
    let watch_dir = tempfile::tempdir().unwrap();
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: watch_dir.path().to_path_buf(),
        folder_ref,
        device_id: [7u8; 32],
        device_label: Some("test-device".to_string()),
        auth: Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret(secret),
        )),
        nest_rpc: Arc::clone(&nest_rpc),
        mls: None,
        credential: EngineCredential::BackupKey(BackupKey::derive(&secret)),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: TestCustody::holding(FoldersConfig::default()),
        reader_hosting: Default::default(),
    })
    .await
    .expect("a writer member's bound row builds, keyless");

    let binding = built.engine.reader_binding();
    assert_eq!(
        binding.owner, None,
        "no marker ⇒ no owner in the binding: not the list row's, not this account"
    );
    assert_eq!(
        binding.account,
        Some(ActorKeypair::from_secret(secret).actor_id().0)
    );
    assert!(binding.owner_chain.is_empty());
    drop(built);
    server_task.abort();
    supervisor.abort();
}

/// Writer-signed change records: a **signed** build (a `Seed` credential signs
/// with the identity key) loads custody once even for a never-served owner-only
/// set, because the set's nonce — the binding every record it writes covers —
/// lives in custody from the set's creation.
#[tokio::test]
async fn build_engine_loads_custody_once_for_a_signed_owner_only_set() {
    let secret = [0x57u8; 32];
    let folder_ref = FolderRef::Local(2);
    let state_dir = tempfile::tempdir().unwrap();
    {
        SyncDb::open(folder_ref.state_db_path(state_dir.path())).unwrap();
    }
    let nest = TestNest::new(owner_only_unserved_summary(2, "docs"));
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));

    let custody = TestCustody::holding(FoldersConfig::default());
    let watch_dir = tempfile::tempdir().unwrap();
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: watch_dir.path().to_path_buf(),
        folder_ref,
        device_id: [7u8; 32],
        device_label: Some("test-device".to_string()),
        auth: Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret(secret),
        )),
        nest_rpc: Arc::clone(&nest_rpc),
        mls: None,
        credential: EngineCredential::Seed(hex::encode(secret)),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: custody.clone(),
        reader_hosting: Default::default(),
    })
    .await
    .expect("a plain owner-only set must always build");

    assert_eq!(
        custody.loads(),
        1,
        "a signed build reads custody for the set's nonce, once"
    );

    drop(built);
    server_task.abort();
    supervisor.abort();
}

/// The build is keyed by the set's IDENTITY, never its name: the nest lists a
/// set named `docs` under id 2, and a build asked for `local:9` must refuse —
/// the by-name lookup this replaced would have adopted the same-named row and
/// run an engine for a set the caller never named. Nothing under the state dir
/// but the (empty) state DB the open created; no custody load.
#[tokio::test]
async fn build_engine_refuses_a_ref_the_nest_does_not_list_even_beside_a_same_named_row() {
    let secret = [0x66u8; 32];
    let state_dir = tempfile::tempdir().unwrap();

    let nest = TestNest::new(owner_only_unserved_summary(2, "docs"));
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));

    let custody = TestCustody::holding(FoldersConfig::default());
    let watch_dir = tempfile::tempdir().unwrap();
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: watch_dir.path().to_path_buf(),
        folder_ref: FolderRef::Local(9),
        device_id: [8u8; 32],
        device_label: Some("test-device".to_string()),
        auth: Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret(secret),
        )),
        nest_rpc: Arc::clone(&nest_rpc),
        mls: None,
        credential: EngineCredential::Seed(hex::encode(secret)),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: custody.clone(),
        reader_hosting: Default::default(),
    })
    .await;

    assert!(
        built.is_none(),
        "a ref with no row on this nest must not build — least of all onto a same-named row"
    );
    assert_eq!(
        nest.count(KIND_FOLDERS_LIST),
        1,
        "the refusal is the list's verdict, read once"
    );
    assert_eq!(custody.loads(), 0);

    server_task.abort();
    supervisor.abort();
}

// ── The cross-nest arm (`on-demand-files.md` § Shared sets on a capability host
//    → *One mechanism*, question 2: one builder, custody is the row) ──

/// The raw group of the cross-nest set these tests build, and its channel.
const FOREIGN_GROUP: &[u8] = b"foreign-openmls-group";
const HOME_NEST: &str = "http://127.0.0.1:7999";

fn foreign_channel() -> [u8; 32] {
    fauna_core::folder_keys::channel_id_for_group(FOREIGN_GROUP)
}

/// The member's custody record for the cross-nest set — what share-accept
/// writes into custody (the home nest over plain http, so no pin to
/// graduate).
fn foreign_record() -> fauna_core::data::ForeignFolder {
    fauna_core::data::ForeignFolder {
        channel_id: foreign_channel(),
        mls_group_id: FOREIGN_GROUP.to_vec(),
        home_nest_url: HOME_NEST.to_string(),
        home_nest_actor_id: Some("cd".repeat(32)),
        set_name: Some("xnest-docs".to_string()),
        access: Some("writer".to_string()),
        content_key_floor: None,
        ..Default::default()
    }
}

/// Build `folder_ref` on the seed-less credential of `reader_secret` over custody
/// sealed under `writer_secret`, holding `record` (when `Some`) and the
/// generations `custody` of the foreign channel. Returns the build, the state
/// dir, and the nest double.
async fn seedless_foreign_build(
    writer_secret: [u8; 32],
    reader_secret: [u8; 32],
    record: Option<fauna_core::data::ForeignFolder>,
    generations: &[([u8; 32], u64)],
) -> (
    Option<crate::engine_lifecycle::BuiltEngine>,
    tempfile::TempDir,
    Arc<TestNest>,
) {
    let nest = TestNest::new(owner_only_unserved_summary(2, "own-docs"));
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));
    let mut cfg = FoldersConfig::default();
    if let Some(record) = record {
        fauna_client_folders::custody::record_foreign_set(&mut cfg, record, 1_000);
    }
    if let Some((first, rest)) = generations.split_first() {
        fauna_client_folders::custody::record_new_set(
            &mut cfg,
            foreign_channel(),
            first.0,
            first.1,
        );
        for (key, at) in rest {
            fauna_client_folders::custody::rotate_set(&mut cfg, &foreign_channel(), *key, *at);
        }
    }
    // A host whose credential is not the account's cannot open its custody.
    let custody = if writer_secret == reader_secret {
        TestCustody::holding(cfg)
    } else {
        TestCustody::unreadable()
    };

    let state_dir = tempfile::tempdir().unwrap();
    let watch_dir = tempfile::tempdir().unwrap();
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: watch_dir.path().to_path_buf(),
        folder_ref: FolderRef::Foreign(foreign_channel()),
        device_id: [8u8; 32],
        device_label: None,
        auth: Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret(reader_secret),
        )),
        nest_rpc: Arc::clone(&nest_rpc),
        mls: None,
        credential: EngineCredential::BackupKey(BackupKey::derive(&reader_secret)),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: custody.clone(),
        reader_hosting: Default::default(),
    })
    .await;
    server_task.abort();
    supervisor.abort();
    (built, state_dir, nest)
}

/// A capability host handed only the `BackupKey` and a bearer builds a
/// cross-nest set from its custody record alone: keyed at the generation
/// custody holds, its byte plane at the HOME nest (its chunks live nowhere
/// else), its control plane relayed through the own nest by channel — and it
/// reads no folder list, because the set has no row here.
#[tokio::test]
async fn a_seedless_build_runs_a_cross_nest_set_from_its_custody_record() {
    let secret = [0x5au8; 32];
    let (built, _state, nest) = seedless_foreign_build(
        secret,
        secret,
        Some(foreign_record()),
        &[([0x42; 32], 1_000), ([0x43; 32], 2_000)],
    )
    .await;
    let built = built.expect("a cross-nest set with a custody record builds");

    assert_eq!(
        built.held_version,
        Some(2),
        "keyed at custody's current generation"
    );
    assert_eq!(
        built.engine.foreign_routing(),
        Some((HOME_NEST.to_string(), hex::encode(foreign_channel()))),
        "both control-plane kinds relay through the own nest to the home nest"
    );
    assert_eq!(
        built.engine.byte_plane_nest_url(),
        HOME_NEST,
        "chunks and manifests live on the HOME nest — the byte plane never rides the relay"
    );
    assert_eq!(
        built.basis,
        crate::engine_lifecycle::BindingBasis::of_foreign(&foreign_record(), Some(2)),
        "a cross-nest set's basis is its custody record and the generation held"
    );
    assert_eq!(
        nest.count(KIND_FOLDERS_LIST),
        0,
        "custody is the row: a cross-nest set reads no list"
    );
    assert_eq!(
        nest.count("fauna.sync.register"),
        0,
        "a build with no device label registers nothing"
    );
}

/// No custody record — the member left, or was removed — refuses, as a deleted
/// row does, and mints no state DB for a set this host cannot serve.
#[tokio::test]
async fn a_cross_nest_ref_without_a_custody_record_refuses() {
    let secret = [0x5bu8; 32];
    let (built, state, _nest) =
        seedless_foreign_build(secret, secret, None, &[([0x42; 32], 1_000)]).await;
    assert!(built.is_none(), "no record ⇒ refusal");
    assert_eq!(
        std::fs::read_dir(state.path()).unwrap().count(),
        0,
        "no state DB minted for a set this host cannot serve"
    );
}

/// Custody this host cannot open refuses outright: a cross-nest set's transport
/// IS its custody record, so there is no keyless shape to build — never a
/// build against a guessed nest, never plaintext.
#[tokio::test]
async fn a_cross_nest_set_whose_custody_cannot_be_opened_refuses() {
    let (built, state, _nest) = seedless_foreign_build(
        [0x5cu8; 32],
        // The host's `BackupKey` is not the one custody is sealed under.
        [0x5du8; 32],
        Some(foreign_record()),
        &[([0x42; 32], 1_000)],
    )
    .await;
    assert!(
        built.is_none(),
        "unreadable custody ⇒ refusal (fail closed)"
    );
    assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 0);
}

/// A foreign record with custody but no generation yet (the member's app has not
/// ingested one) builds bound-keyless — every operation fails closed — rather
/// than refusing: the record names the transport, and the next edge re-keys.
#[tokio::test]
async fn a_cross_nest_set_without_a_generation_builds_keyless() {
    let secret = [0x5eu8; 32];
    let (built, _state, _nest) =
        seedless_foreign_build(secret, secret, Some(foreign_record()), &[]).await;
    let built = built.expect("the record alone names the transport");
    assert_eq!(
        built.held_version, None,
        "bound-keyless: fails closed per operation"
    );
    assert!(built.engine.foreign_routing().is_some());
}

/// The cross-nest edge: custody is the row, so a rotation is a basis change —
/// the resident host's re-read sees the new generation and rebuilds; an
/// unchanged custody keeps the engine; a record gone rebuilds into a refusal.
#[tokio::test]
async fn the_cross_nest_edge_reads_custody_and_rebuilds_on_a_rotation() {
    use crate::engine_lifecycle::{EdgeVerdict, fetch_binding_basis};

    let secret = [0x5fu8; 32];
    let nest = TestNest::new(owner_only_unserved_summary(2, "own-docs"));
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));
    let mut cfg = FoldersConfig::default();
    fauna_client_folders::custody::record_foreign_set(&mut cfg, foreign_record(), 1_000);
    fauna_client_folders::custody::record_new_set(&mut cfg, foreign_channel(), [0x42; 32], 1_000);
    let _ = secret;

    let folder_ref = FolderRef::Foreign(foreign_channel());
    let built_on = crate::engine_lifecycle::BindingBasis::of_foreign(&foreign_record(), Some(1));
    let verdict = |now: Option<Option<crate::engine_lifecycle::BindingBasis>>| {
        crate::engine_lifecycle::edge_verdict(
            &built_on,
            Some(1),
            now.expect("custody is readable").as_ref(),
        )
    };

    let now = fetch_binding_basis(&nest_rpc, &*TestCustody::holding(cfg.clone()), folder_ref).await;
    assert_eq!(verdict(now), EdgeVerdict::Keep, "custody unchanged: keep");

    fauna_client_folders::custody::rotate_set(&mut cfg, &foreign_channel(), [0x43; 32], 2_000);
    let now = fetch_binding_basis(&nest_rpc, &*TestCustody::holding(cfg.clone()), folder_ref).await;
    assert_eq!(
        verdict(now),
        EdgeVerdict::Rebuild,
        "a rotation in custody reaches the resident foreign engine"
    );

    // A leave tombstones the record; a custody with no live record reads as
    // the row gone.
    cfg.foreign_sets.clear();
    let now = fetch_binding_basis(&nest_rpc, &*TestCustody::holding(cfg), folder_ref).await;
    assert_eq!(now, Some(None), "the record gone reads as the row gone");

    server_task.abort();
    supervisor.abort();
}

// ── Shared sets on a capability host (`on-demand-files.md` § Shared sets on a
//    capability host, decisions 1 and 2) ──

/// Build a BOUND set on the seed-less credential — the `BackupKey` alone, with
/// the actor id the bearer carries — over custody holding `custody` (the
/// generations of the set's derived channel), the row projecting `floor`.
async fn seedless_bound_build(
    generations: &[([u8; 32], u64)],
    floor: Option<u64>,
) -> (
    Option<crate::engine_lifecycle::BuiltEngine>,
    FolderSummary,
    usize,
) {
    let secret = [0x44u8; 32];
    let raw = b"raw-openmls-group-id".to_vec();
    let channel = fauna_mls::types::ChannelId::from_group_id(&raw).0;
    let mut row = owner_only_unserved_summary(3, "shared");
    row.mls_group_id = Some(hex::encode(&raw));
    row.content_key_floor = floor;

    let nest = TestNest::new(row.clone());
    let (nest_rpc, server_task, supervisor) = stand_up_ws(Arc::clone(&nest));
    let mut cfg = FoldersConfig::default();
    let (first, rest) = generations.split_first().expect("at least one generation");
    fauna_client_folders::custody::record_new_set(&mut cfg, channel, first.0, first.1);
    for (key, at) in rest {
        fauna_client_folders::custody::rotate_set(&mut cfg, &channel, *key, *at);
    }
    // The identity-holding app wrote this custody; the host only reads it.
    let custody = TestCustody::holding(cfg);

    let state_dir = tempfile::tempdir().unwrap();
    let watch_dir = tempfile::tempdir().unwrap();
    let built = build_engine(EngineParams {
        state_dir: state_dir.path().to_path_buf(),
        watch_dir: watch_dir.path().to_path_buf(),
        folder_ref: FolderRef::Local(3),
        device_id: [9u8; 32],
        device_label: Some("fauna-file-provider".to_string()),
        auth: Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret(secret),
        )),
        nest_rpc: Arc::clone(&nest_rpc),
        mls: None,
        credential: EngineCredential::BackupKey(fauna_core::crypto::BackupKey::derive(&secret)),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys: custody.clone(),
        reader_hosting: Default::default(),
    })
    .await;
    let custody_reads = custody.loads();
    server_task.abort();
    supervisor.abort();
    (built, row, custody_reads)
}

/// Decision 1: a seed-less build of a bound set reads the account's custody
/// under its `BackupKey` and runs keyed at the current generation — where it
/// once built keyless and failed every operation closed.
#[tokio::test]
async fn a_seedless_build_loads_a_bound_sets_keys_from_custody_under_its_backup_key() {
    let (built, row, custody_reads) =
        seedless_bound_build(&[([0x42; 32], 1_000), ([0x43; 32], 2_000)], Some(2)).await;
    let built = built.expect("a bound set with custody builds");
    assert_eq!(custody_reads, 1, "one custody read, under the BackupKey");
    assert_eq!(
        built.held_version,
        Some(2),
        "keyed at the current generation"
    );
    assert_eq!(
        built.basis,
        crate::engine_lifecycle::BindingBasis::of(&row),
        "the build records the row it resolved from"
    );
    assert_eq!(built.seal_hold(), None, "at the floor: seals proceed");
    assert_eq!(
        built.edge_verdict(Some(&crate::engine_lifecycle::BindingBasis::of(&row))),
        crate::engine_lifecycle::EdgeVerdict::Keep
    );
}

/// Decision 2's fail-closed arm at build: custody behind the floor builds keyed
/// at what it holds (older bodies still open), but every seal is held and every
/// edge re-reads custody until the generation arrives.
#[tokio::test]
async fn a_seedless_build_behind_the_floor_holds_its_seals_and_keeps_re_resolving() {
    let (built, row, _) = seedless_bound_build(&[([0x42; 32], 1_000)], Some(2)).await;
    let built = built.expect("custody behind the floor still builds");
    assert_eq!(built.held_version, Some(1));
    assert_eq!(
        built.seal_hold(),
        Some(crate::engine_lifecycle::SealHold::Behind {
            floor: 2,
            held: Some(1)
        }),
        "a seal now would be under a generation the owner rotated past"
    );
    assert_eq!(
        built.edge_verdict(Some(&crate::engine_lifecycle::BindingBasis::of(&row))),
        crate::engine_lifecycle::EdgeVerdict::Rebuild,
        "custody arriving is invisible on the row, so the host keeps re-reading"
    );
}

// ── The federated floor (`on-demand-files.md` § Shared sets on a capability
//    host → *One mechanism*, question 2: the foreign record carries the set's
//    `content_key_floor`, refreshed from every federated content-key read) ──

/// The member's record with the home nest's floor stamp on it.
fn foreign_record_at_floor(floor: u64) -> fauna_core::data::ForeignFolder {
    fauna_core::data::ForeignFolder {
        content_key_floor: Some(floor),
        ..foreign_record()
    }
}

/// Decision 2's fail-closed arm for a cross-nest set: a record whose floor is
/// ahead of the generation custody holds builds keyed at what it holds (older
/// bodies still open), but every seal is held — locally, ahead of the home
/// nest's `stale_content_key` refusal — and the edge keeps re-reading custody
/// until the generation arrives.
#[tokio::test]
async fn a_cross_nest_build_behind_the_record_floor_holds_its_seals_and_keeps_re_resolving() {
    use crate::engine_lifecycle::{BindingBasis, EdgeVerdict, SealHold};

    let secret = [0x5bu8; 32];
    let (built, _state, _nest) = seedless_foreign_build(
        secret,
        secret,
        Some(foreign_record_at_floor(2)),
        &[([0x42; 32], 1_000)],
    )
    .await;
    let built = built.expect("custody behind the floor still builds");
    assert_eq!(built.held_version, Some(1), "keyed at what custody holds");
    assert_eq!(
        built.seal_hold(),
        Some(SealHold::Behind {
            floor: 2,
            held: Some(1)
        }),
        "a seal now would be under a generation the owner rotated past"
    );
    assert_eq!(
        built.basis,
        BindingBasis::of_foreign(&foreign_record_at_floor(2), Some(1)),
        "the basis carries the record's floor"
    );
    assert_eq!(built.basis.content_key_floor(), Some(2));
    assert_eq!(
        built.edge_verdict(Some(&BindingBasis::of_foreign(
            &foreign_record_at_floor(2),
            Some(1)
        ))),
        EdgeVerdict::Rebuild,
        "custody arriving is invisible on the record, so the host keeps re-reading"
    );
}

/// At the floor a cross-nest engine seals; a floor move on the record — the
/// federated read stamped a rotation — is a basis change at the host's custody
/// edge; and a record that never carried a floor (unbound, no floor held) arms
/// nothing, leaving the home nest's refusal alone.
#[tokio::test]
async fn a_cross_nest_build_at_the_record_floor_seals_and_a_floor_move_is_an_edge() {
    use crate::engine_lifecycle::{BindingBasis, EdgeVerdict};

    let secret = [0x5cu8; 32];
    let (built, _state, _nest) = seedless_foreign_build(
        secret,
        secret,
        Some(foreign_record_at_floor(2)),
        &[([0x42; 32], 1_000), ([0x43; 32], 2_000)],
    )
    .await;
    let built = built.expect("builds");
    assert_eq!(built.held_version, Some(2));
    assert_eq!(built.seal_hold(), None, "at the floor: seals proceed");
    assert_eq!(
        built.edge_verdict(Some(&BindingBasis::of_foreign(
            &foreign_record_at_floor(2),
            Some(2)
        ))),
        EdgeVerdict::Keep
    );
    assert_eq!(
        built.edge_verdict(Some(&BindingBasis::of_foreign(
            &foreign_record_at_floor(3),
            Some(2)
        ))),
        EdgeVerdict::Rebuild,
        "a floor move on the record is a basis change"
    );

    let secret = [0x5du8; 32];
    let (built, _state, _nest) = seedless_foreign_build(
        secret,
        secret,
        Some(foreign_record()),
        &[([0x42; 32], 1_000)],
    )
    .await;
    let built = built.expect("builds");
    assert_eq!(
        built.seal_hold(),
        None,
        "no stamp on the record ⇒ nothing armed: the home nest's refusal alone, as before"
    );
}

// ── Residency reaches a cross-nest seat (`file-sync.md` § Relay serving → *A
//    member on another nest*, step (1)): the foreign record carries the home
//    nest's residency stamp, and the engine arms from it what a same-nest seat
//    arms from its row ──

/// The member's record with the home nest's residency stamp on it.
fn foreign_record_at_residency(metadata_only: bool) -> fauna_core::data::ForeignFolder {
    fauna_core::data::ForeignFolder {
        residency: Some(fauna_core::data::ForeignResidency {
            metadata_only,
            stamped_at: 1,
        }),
        ..foreign_record()
    }
}

/// A record the home nest stamped metadata-only builds an engine with the
/// upload skip armed and the reading persisted for the holder-keeps gate.
#[tokio::test]
async fn a_cross_nest_record_stamped_metadata_only_arms_the_upload_skip() {
    let secret = [0x5eu8; 32];
    let (built, _state, _nest) = seedless_foreign_build(
        secret,
        secret,
        Some(foreign_record_at_residency(true)),
        &[([0x42; 32], 1_000)],
    )
    .await;
    let built = built.expect("builds");
    assert!(
        built.engine.is_metadata_only_residency(),
        "the seat records a manifest and uploads no chunk"
    );
    assert_eq!(
        built.engine.db().residency_reading().unwrap(),
        Some(true),
        "the holder-keeps gate reads metadata-only"
    );
}

/// A record no home nest has stamped is UNKNOWN, never full: the seat uploads
/// (nothing unparseable stops bytes resting) and persists no reading, so the
/// holder-keeps gate keeps every own-record body. A record stamped full
/// persists *full*.
#[tokio::test]
async fn an_unstamped_cross_nest_record_uploads_and_persists_no_reading() {
    let secret = [0x5fu8; 32];
    let (built, _state, _nest) = seedless_foreign_build(
        secret,
        secret,
        Some(foreign_record()),
        &[([0x42; 32], 1_000)],
    )
    .await;
    let built = built.expect("builds");
    assert!(
        !built.engine.is_metadata_only_residency(),
        "the seat uploads"
    );
    assert_eq!(
        built.engine.db().residency_reading().unwrap(),
        None,
        "unknown persists no reading — never *full*"
    );

    let secret = [0x60u8; 32];
    let (built, _state, _nest) = seedless_foreign_build(
        secret,
        secret,
        Some(foreign_record_at_residency(false)),
        &[([0x42; 32], 1_000)],
    )
    .await;
    let built = built.expect("builds");
    assert!(!built.engine.is_metadata_only_residency());
    assert_eq!(built.engine.db().residency_reading().unwrap(), Some(false));
}

/// A flip on the record — the federated read stamped a new residency — is a
/// basis change at the host's custody edge: a foreign engine's per-tick
/// sync-mode read never carries a residency, so the rebuild is the one path a
/// flip takes to a running foreign engine.
#[tokio::test]
async fn a_residency_stamp_change_on_the_record_is_an_edge() {
    use crate::engine_lifecycle::{BindingBasis, EdgeVerdict};

    let secret = [0x61u8; 32];
    let (built, _state, _nest) = seedless_foreign_build(
        secret,
        secret,
        Some(foreign_record()),
        &[([0x42; 32], 1_000)],
    )
    .await;
    let built = built.expect("builds");
    assert_eq!(
        built.edge_verdict(Some(&BindingBasis::of_foreign(&foreign_record(), Some(1)))),
        EdgeVerdict::Keep
    );
    for metadata_only in [true, false] {
        assert_eq!(
            built.edge_verdict(Some(&BindingBasis::of_foreign(
                &foreign_record_at_residency(metadata_only),
                Some(1)
            ))),
            EdgeVerdict::Rebuild,
            "unknown → {metadata_only}: a stamp change is a basis change"
        );
    }
    assert_ne!(
        BindingBasis::of_foreign(&foreign_record_at_residency(true), Some(1)),
        BindingBasis::of_foreign(&foreign_record_at_residency(false), Some(1)),
        "a flip either way is a basis change"
    );
}
