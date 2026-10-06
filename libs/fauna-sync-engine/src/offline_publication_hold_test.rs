//! Decision 2′'s engine tier (`on-demand-files.md` § Shared sets on a
//! capability host, decision 2′): a write made on a same-nest shared set while
//! its row cannot be read **seals on the last floor the engine read and mints
//! its own-pending row** (b), but **publishes nothing to a nest** (c) until a
//! pass reads the row again; a floor read ahead of the generation held
//! **withdraws** the stale pending row (d) and still holds the seal (a).
//!
//! The harness splits the two planes the way production can: the byte plane is
//! a real HTTP server ([`MockNest`] on wiremock — every chunk and manifest the
//! engine sends lands there), the control plane a `NestClient` that never
//! connects, so the pre-seal row read ([`SyncEngine::refresh_seal_floor`]) fails
//! while the byte plane stays reachable. That is why (c) must be a gate and not
//! a side effect of being offline: here nothing would stop the chunks but the
//! gate. A successful row read is driven by `apply_binding_read` — the method
//! the read's success arm calls — since no WS-RPC nest runs in this crate; the
//! record that would upgrade the pending row therefore never lands here (the
//! upgrade is `retain_own_change`'s, pinned in `fauna-account-store`; the whole
//! journey against a real nest is `agent_custody_rekey.rs` and the share-pump
//! e2e).

use std::sync::Arc;

use fauna_core::folder_keys::{FolderContentKeys, FolderRef};
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::folders::FolderSummary;
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::binding_edge::{BindingBasis, BindingEdge};
use crate::db::{SyncDb, SyncState};
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::{BlobStore, MockNest};
use crate::transfer::TransferPool;

const DEVICE_ID: [u8; 32] = [0u8; 32];
const SET_ID: i64 = 7;
const GEN_1: [u8; 32] = [0x51; 32];
const GEN_2: [u8; 32] = [0x52; 32];
const REL: &str = "cabin.txt";

/// The set's row as a read returns it, carrying `floor`.
fn basis(floor: u64) -> BindingBasis {
    BindingBasis::of(&FolderSummary {
        id: SET_ID,
        name: "__test".into(),
        mls_group_id: Some(hex::encode([0x42u8; 24])),
        content_key_floor: Some(floor),
        ..Default::default()
    })
}

/// A resident engine on a bound set holding `keys`, its edge built on a row
/// whose floor is `floor`: byte plane at `server_uri`, control plane
/// unreachable.
fn bound_engine(
    server_uri: &str,
    watch_dir: std::path::PathBuf,
    db: SyncDb,
    keys: FolderContentKeys,
    floor: u64,
) -> SyncEngine {
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        ActorKeypair::generate(),
        bearer,
        reqwest::Client::new(),
    ));
    let client = SyncClient::new(auth, &DEVICE_ID);
    // Never connected: every control-plane read and record fails.
    let nest_client =
        fauna_client::NestClient::new(server_uri.to_string(), ActorKeypair::generate());
    SyncEngine::new(
        watch_dir,
        db,
        client,
        Some("__test".to_string()),
        DEVICE_ID,
        None,
        None,
        None, // no backup key: a bound set seals under its content key
        Some(vec![0x42u8; 24]),
        Some(keys),
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        nest_client,
        crate::config::SyncMode::Sync,
    )
    .with_binding_edge(BindingEdge {
        folder_ref: FolderRef::Local(SET_ID),
        basis: basis(floor),
        on_rebuild: Arc::new(|| {}),
    })
}

/// Every request that sent something to the nest's byte plane (a chunk check,
/// a chunk, a manifest) — anything but a read.
async fn sends(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method != wiremock::http::Method::GET)
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect()
}

fn state_of(engine: &SyncEngine) -> SyncState {
    engine.db().get_entry(REL).unwrap().expect("row").state
}

/// The single own-pending row (path, generation stamp, manifest), if any.
fn pending(engine: &SyncEngine) -> Vec<(String, Option<u64>, Option<String>)> {
    engine
        .db()
        .own_pending_changes(16)
        .unwrap()
        .into_iter()
        .map(|r| (r.path, r.content_key_version, r.manifest_hash))
        .collect()
}

/// One upload pass with the row unreadable: (b) seal + mint, (c) nothing sent,
/// the path still pending. Returns the pending row's manifest hash.
async fn held_pass(engine: &SyncEngine, server: &MockServer, generation: u64) -> String {
    engine.reconcile().await.unwrap();
    let (recorded, _) = engine.upload_pending(1).await.unwrap();
    assert!(recorded.is_empty(), "nothing records while held");
    let rows = pending(engine);
    assert_eq!(
        rows.len(),
        1,
        "(b) the write is sealed and its own-pending row minted although the row \
         could not be read: {rows:?}"
    );
    let (path, stamp, manifest) = rows.into_iter().next().unwrap();
    assert_eq!(path, REL);
    assert_eq!(stamp, Some(generation), "stamped with the generation held");
    assert_eq!(
        sends(server).await,
        Vec::<String>::new(),
        "(c) a pass whose row read failed sends no chunk, manifest or record to a nest"
    );
    assert_eq!(
        state_of(engine),
        SyncState::LocallyModified,
        "the path stays pending, so the next pass re-drives it"
    );
    manifest.expect("a pending row names its manifest")
}

fn last_manifest(store: &BlobStore) -> String {
    hex::encode(store.expect_last_manifest_hash().digest())
}

/// Floor met at the next read: what was sealed offline is what is published.
#[tokio::test]
async fn an_unread_floor_seals_and_mints_but_publishes_only_after_a_read_meets_it() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(REL), b"authored with the nest out of reach").unwrap();
    let engine = bound_engine(
        &server.uri(),
        dir.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
        FolderContentKeys::genesis(GEN_1, 1_000),
        1,
    );

    let offline_manifest = held_pass(&engine, &server, 1).await;

    // The next pass reads the row: the floor is still met.
    engine.apply_binding_read(&[(SET_ID, basis(1))]);
    engine.upload_file(REL).await.unwrap();
    assert!(
        !sends(&server).await.is_empty(),
        "a read floor that is met publishes"
    );
    assert_eq!(
        last_manifest(&store),
        offline_manifest,
        "the seal is convergent: the bytes published are the ones peers were served"
    );
}

/// Floor ahead at the next read: the stale pending row leaves the serve set,
/// nothing stamped with the older generation is published, and after the
/// rebuild the write is sealed, minted and published under the new one.
#[tokio::test]
async fn a_floor_read_ahead_withdraws_the_stale_row_and_publishes_only_the_new_generation() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let watch = dir.path().join("watch");
    std::fs::create_dir_all(&watch).unwrap();
    std::fs::write(watch.join(REL), b"authored before the rotation was seen").unwrap();

    let gen_1_manifest = {
        let engine = bound_engine(
            &server.uri(),
            watch.clone(),
            SyncDb::open(&db_path).unwrap(),
            FolderContentKeys::genesis(GEN_1, 1_000),
            1,
        );
        let gen_1_manifest = held_pass(&engine, &server, 1).await;

        // The row reads again — the owner rotated past generation 1.
        engine.apply_binding_read(&[(SET_ID, basis(2))]);
        assert_eq!(
            pending(&engine),
            Vec::new(),
            "(d) an own-pending row stamped below a floor the host has read is withdrawn"
        );
        let held = engine.upload_file(REL).await;
        assert!(held.is_err(), "(a) a floor read ahead holds the seal");
        assert_eq!(sends(&server).await, Vec::<String>::new());
        assert_eq!(state_of(&engine), SyncState::LocallyModified);
        gen_1_manifest
    };

    // Custody brings generation 2; the host rebuilds over the same state DB.
    let mut keys = FolderContentKeys::genesis(GEN_1, 1_000);
    keys.rotate(GEN_2, 2_000);
    let engine = bound_engine(
        &server.uri(),
        watch.clone(),
        SyncDb::open(&db_path).unwrap(),
        keys,
        2,
    );
    let gen_2_manifest = held_pass(&engine, &server, 2).await;
    assert_ne!(
        gen_2_manifest, gen_1_manifest,
        "re-sealed under generation 2"
    );

    engine.apply_binding_read(&[(SET_ID, basis(2))]);
    engine.upload_file(REL).await.unwrap();
    assert_eq!(
        last_manifest(&store),
        gen_2_manifest,
        "what reaches the nest is the generation-2 seal"
    );
    assert!(
        !store
            .manifests
            .lock()
            .unwrap()
            .contains_key(&gen_1_manifest),
        "nothing sealed under generation 1 was ever published"
    );
}
