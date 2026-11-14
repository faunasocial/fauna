//! The binding record at the builder (`on-demand-files-shared-sets.md` § Shared
//! sets on a capability host, decision 2″): a build that read the row from a
//! nest in reach records it, and a cold build that cannot reach the nest
//! stands on that record — through the one builder, in decision 2′'s posture
//! for a failed read — while a nest that ANSWERS is never treated as offline.
//!
//! The control plane is the in-memory socket of
//! `build_engine_retired_custody_test` (served, or refusing the list); an
//! unreachable nest is a `NestClient` torn down (`disconnect`), whose every
//! request fails as a transport fault. The host's custody reader is
//! switchable, because a capability host's reader walks from the nest and
//! so fails with it.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fauna_client::{AuthClient, NestClient};
use fauna_core::crypto::{BackupKey, BindingRecordKey};
use fauna_core::data::FoldersConfig;
use fauna_core::folder_keys::FolderRef;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::folders::FolderSummary;

use crate::binding_record::{self, OpenError};
use crate::build_engine_retired_custody_test::{
    TestNest, foreign_channel, foreign_record, owner_only_unserved_summary, stand_up_ws,
};
use crate::db::SyncDb;
use crate::engine_lifecycle::{
    BindingBasis, BuiltEngine, EngineCredential, EngineParams, SealHold, build_engine,
};

const SECRET: [u8; 32] = [0x44; 32];
const SET_ID: i64 = 3;

/// A custody reader the test can cut off, as a capability host's replica is
/// cut off with its nest.
struct SwitchableCustody {
    held: FoldersConfig,
    readable: AtomicBool,
}

#[async_trait::async_trait]
impl fauna_client_folders::FolderKeyReader for SwitchableCustody {
    async fn load(&self) -> anyhow::Result<FoldersConfig> {
        if self.readable.load(Ordering::SeqCst) {
            Ok(self.held.clone())
        } else {
            anyhow::bail!("the fleet replica cannot walk: the nest is out of reach")
        }
    }
}

fn custody(held: FoldersConfig) -> Arc<SwitchableCustody> {
    Arc::new(SwitchableCustody {
        held,
        readable: AtomicBool::new(true),
    })
}

/// A bound same-nest set: its row, and custody holding `generations` at its
/// channel (plus `extra` entries of other sets).
fn bound_set(
    generations: &[([u8; 32], u64)],
    floor: Option<u64>,
) -> (FolderSummary, FoldersConfig) {
    let raw = b"raw-openmls-group-id".to_vec();
    let channel = fauna_core::folder_keys::channel_id_for_group(&raw);
    let mut row = owner_only_unserved_summary(SET_ID, "shared");
    row.mls_group_id = Some(hex::encode(&raw));
    row.content_key_floor = floor;
    let mut cfg = FoldersConfig::default();
    let (first, rest) = generations.split_first().expect("a generation");
    fauna_client_folders::custody::record_new_set(&mut cfg, channel, first.0, first.1);
    for (key, at) in rest {
        fauna_client_folders::custody::rotate_set(&mut cfg, &channel, *key, *at);
    }
    (row, cfg)
}

async fn build(
    state_dir: &Path,
    folder_ref: FolderRef,
    nest_rpc: &Arc<NestClient>,
    folder_keys: Arc<SwitchableCustody>,
    secret: [u8; 32],
) -> Option<BuiltEngine> {
    let watch_dir = tempfile::tempdir().unwrap();
    build_at(
        state_dir,
        watch_dir.path(),
        folder_ref,
        nest_rpc,
        folder_keys,
        secret,
    )
    .await
}

/// [`build`] over a caller's root — the door test's staging root.
async fn build_at(
    state_dir: &Path,
    watch_dir: &Path,
    folder_ref: FolderRef,
    nest_rpc: &Arc<NestClient>,
    folder_keys: Arc<SwitchableCustody>,
    secret: [u8; 32],
) -> Option<BuiltEngine> {
    build_engine(EngineParams {
        state_dir: state_dir.to_path_buf(),
        watch_dir: watch_dir.to_path_buf(),
        folder_ref,
        device_id: [9u8; 32],
        device_label: None,
        auth: Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret(SECRET),
        )),
        nest_rpc: Arc::clone(nest_rpc),
        mls: None,
        credential: EngineCredential::BackupKey(BackupKey::derive(&secret)),
        progress_tx: None,
        predecessor_backup_keys: Vec::new(),
        predecessor_actor_ids: Vec::new(),
        learned_predecessors: Default::default(),
        access_gate: None,
        change_signer: None,
        folder_keys,
        reader_hosting: Default::default(),
    })
    .await
}

/// A control plane that can never be reached: every request fails as a
/// transport fault.
async fn unreachable_nest() -> Arc<NestClient> {
    let client = NestClient::with_auth(Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret(SECRET),
    )));
    client.disconnect().await;
    client
}

fn actor_hex() -> String {
    hex::encode(ActorKeypair::from_secret(SECRET).actor_id().0)
}

fn opened(
    state_dir: &Path,
    folder_ref: FolderRef,
) -> Result<binding_record::BindingRecord, OpenError> {
    let db = SyncDb::open(folder_ref.state_db_path(state_dir)).unwrap();
    binding_record::get(
        &db,
        &BindingRecordKey::derive(&BackupKey::derive(&SECRET)),
        &actor_hex(),
        folder_ref,
    )
}

/// Build once online (which writes the record), then cut both the nest and
/// the host's custody reader off.
async fn built_online_then_cut_off(
    row: FolderSummary,
    cfg: FoldersConfig,
) -> (tempfile::TempDir, Arc<TestNest>, Arc<SwitchableCustody>) {
    let state = tempfile::tempdir().unwrap();
    let nest = TestNest::new(row);
    let (rpc, server, supervisor) = stand_up_ws(Arc::clone(&nest));
    let keys = custody(cfg);
    build(
        state.path(),
        FolderRef::Local(SET_ID),
        &rpc,
        keys.clone(),
        SECRET,
    )
    .await
    .expect("the online build runs");
    server.abort();
    supervisor.abort();
    keys.readable.store(false, Ordering::SeqCst);
    (state, nest, keys)
}

/// Decision 2″ (4)–(5): a cold build with the nest unreachable runs the
/// ordinary engine over the recorded row and custody — keyed, its floor the
/// recorded one marked unread, so it seals and publishes nothing until a read
/// reaches the nest. Red before the record: the builder refused.
#[tokio::test]
async fn a_cold_build_with_the_nest_unreachable_stands_on_the_binding_record() {
    let (row, cfg) = bound_set(&[([0x42; 32], 1_000), ([0x43; 32], 2_000)], Some(2));
    let (state, _nest, keys) = built_online_then_cut_off(row.clone(), cfg).await;
    let record = opened(state.path(), FolderRef::Local(SET_ID)).expect("the online build recorded");
    assert_eq!(record.row.as_ref(), Some(&row), "the row exactly as read");

    let built = build(
        state.path(),
        FolderRef::Local(SET_ID),
        &unreachable_nest().await,
        keys,
        SECRET,
    )
    .await
    .expect("the cold build stands on the record");
    assert_eq!(
        built.held_version,
        Some(2),
        "keyed from the recorded custody"
    );
    assert_eq!(
        built.basis,
        BindingBasis::of(&row),
        "built on the recorded row"
    );
    assert_eq!(
        built.seal_hold(),
        None,
        "the recorded floor is met: seals proceed (2′ (b))"
    );
    assert_eq!(
        built.publish_hold(),
        Some(SealHold::Unreadable),
        "nothing sealed reaches a nest until a read checks the floor (2′ (c))"
    );
}

/// Decision 2″ (6): the record carries only the custody the host had, so a
/// row stamped with a generation it lacks builds behind its floor — every
/// seal held.
#[tokio::test]
async fn a_record_lacking_the_rows_generation_builds_behind_its_floor() {
    let (row, cfg) = bound_set(&[([0x42; 32], 1_000)], Some(2));
    let (state, _nest, keys) = built_online_then_cut_off(row, cfg).await;
    let built = build(
        state.path(),
        FolderRef::Local(SET_ID),
        &unreachable_nest().await,
        keys,
        SECRET,
    )
    .await
    .expect("behind the floor still builds");
    assert_eq!(
        built.seal_hold(),
        Some(SealHold::Behind {
            floor: 2,
            held: Some(1)
        })
    );
}

/// Decision 2″ (3): a successful read that finds the row gone removes the
/// record, so the next unreachable build refuses rather than resurrect it.
#[tokio::test]
async fn a_read_that_finds_the_row_gone_retires_the_record() {
    let (row, cfg) = bound_set(&[([0x42; 32], 1_000)], None);
    let (state, nest, keys) = built_online_then_cut_off(row, cfg).await;
    *nest.folder.lock().unwrap() = None;
    keys.readable.store(true, Ordering::SeqCst);
    let (rpc, server, supervisor) = stand_up_ws(Arc::clone(&nest));
    assert!(
        build(
            state.path(),
            FolderRef::Local(SET_ID),
            &rpc,
            keys.clone(),
            SECRET
        )
        .await
        .is_none(),
        "a row gone refuses"
    );
    server.abort();
    supervisor.abort();
    assert!(matches!(
        opened(state.path(), FolderRef::Local(SET_ID)),
        Err(OpenError::Missing)
    ));
    keys.readable.store(false, Ordering::SeqCst);
    assert!(
        build(
            state.path(),
            FolderRef::Local(SET_ID),
            &unreachable_nest().await,
            keys,
            SECRET
        )
        .await
        .is_none()
    );
}

/// Decision 2″ (4): a nest that ANSWERS — here a typed refusal of the list —
/// is a read, never an outage: nothing is built from the record.
#[tokio::test]
async fn a_nest_that_answers_with_a_refusal_builds_nothing_from_the_record() {
    let (row, cfg) = bound_set(&[([0x42; 32], 1_000)], None);
    let (state, nest, keys) = built_online_then_cut_off(row, cfg).await;
    nest.refuse_list.store(true, Ordering::SeqCst);
    let (rpc, server, supervisor) = stand_up_ws(Arc::clone(&nest));
    let built = build(state.path(), FolderRef::Local(SET_ID), &rpc, keys, SECRET).await;
    server.abort();
    supervisor.abort();
    assert!(built.is_none(), "an answer refuses as today");
    assert!(
        opened(state.path(), FolderRef::Local(SET_ID)).is_ok(),
        "the record stays — an answer about the bearer is not the row"
    );
}

/// Decision 2″ (7): no record, or a record under another `BackupKey` (a
/// succession), refuses the cold build.
#[tokio::test]
async fn a_missing_record_or_one_under_another_backup_key_refuses() {
    let (row, cfg) = bound_set(&[([0x42; 32], 1_000)], None);
    let never_built = tempfile::tempdir().unwrap();
    let keys = custody(cfg.clone());
    keys.readable.store(false, Ordering::SeqCst);
    assert!(
        build(
            never_built.path(),
            FolderRef::Local(SET_ID),
            &unreachable_nest().await,
            keys,
            SECRET
        )
        .await
        .is_none(),
        "no record: refused"
    );

    let (state, _nest, keys) = built_online_then_cut_off(row, cfg).await;
    assert!(
        build(
            state.path(),
            FolderRef::Local(SET_ID),
            &unreachable_nest().await,
            keys,
            [0x45; 32]
        )
        .await
        .is_none(),
        "a record that will not open under this BackupKey: refused"
    );
}

/// Decision 2″ (1): the record carries THIS set's custody entries alone —
/// another set's entry the account holds never rides along — and the slice
/// resolves the set exactly as the full custody does.
#[tokio::test]
async fn the_record_carries_only_this_sets_custody() {
    let (row, mut cfg) = bound_set(&[([0x42; 32], 1_000)], None);
    let other = fauna_core::folder_keys::channel_id_for_group(b"another-group");
    fauna_client_folders::custody::record_new_set(&mut cfg, other, [0x77; 32], 1_000);
    let (state, _nest, _keys) = built_online_then_cut_off(row.clone(), cfg.clone()).await;
    let record = opened(state.path(), FolderRef::Local(SET_ID)).unwrap();
    assert!(!record.custody.sets.is_empty());
    assert!(
        record
            .custody
            .sets
            .iter()
            .all(|e| e.channel_id != Some(other)),
        "no entry of another set"
    );
    assert_eq!(
        fauna_client_folders::resolve_engine_key_binding(&record.custody, &row).unwrap(),
        fauna_client_folders::resolve_engine_key_binding(&cfg, &row).unwrap()
    );
}

/// Decision 2″ (1)–(5) on the cross-nest arm: its custody record is its row,
/// so a cold build whose custody walk fails with the nest out of reach builds
/// over the recorded record, its floor marked unread.
#[tokio::test]
async fn a_cross_nest_cold_build_stands_on_its_recorded_custody_record() {
    let mut cfg = FoldersConfig::default();
    fauna_client_folders::custody::record_foreign_set(&mut cfg, foreign_record(), 1_000);
    fauna_client_folders::custody::record_new_set(&mut cfg, foreign_channel(), [0x42; 32], 1_000);
    let other = fauna_core::folder_keys::channel_id_for_group(b"another-group");
    fauna_client_folders::custody::record_new_set(&mut cfg, other, [0x77; 32], 1_000);
    let folder_ref = FolderRef::Foreign(foreign_channel());

    let state = tempfile::tempdir().unwrap();
    let nest = TestNest::new(owner_only_unserved_summary(2, "own-docs"));
    let (rpc, server, supervisor) = stand_up_ws(Arc::clone(&nest));
    let keys = custody(cfg);
    let online = build(state.path(), folder_ref, &rpc, keys.clone(), SECRET)
        .await
        .expect("the online cross-nest build runs");
    server.abort();
    supervisor.abort();
    let record = opened(state.path(), folder_ref).expect("recorded");
    assert!(record.row.is_none());
    assert_eq!(record.custody.foreign_sets.len(), 1);
    assert!(
        record
            .custody
            .sets
            .iter()
            .all(|e| e.channel_id != Some(other))
    );

    keys.readable.store(false, Ordering::SeqCst);
    let cold = build(
        state.path(),
        folder_ref,
        &unreachable_nest().await,
        keys,
        SECRET,
    )
    .await
    .expect("the cold cross-nest build stands on the record");
    assert_eq!(cold.held_version, online.held_version);
    assert_eq!(cold.basis, online.basis);
    assert_eq!(cold.publish_hold(), Some(SealHold::Unreadable));
}

/// Decision 2″ at the door (`p2p-shared-set-build.md` § *Phone peers —
/// design*, decision 3): a host relaunched cold with the nest unreachable
/// builds on the record, and its door lands a peer's un-sequenced body in the
/// staging root, which the kept-root lookup answers. Red before the record:
/// the build refused, so the door had no engine to land into.
#[tokio::test]
async fn a_cold_built_engine_lands_a_peers_unsequenced_body_and_the_kept_lookup_serves_it() {
    use crate::always_resident::share_ingest_on_demand;
    use crate::peer_share_ingest_test::{
        CONTENT_KEY, PEER_ACTOR_HEX, SET_NONCE, encoded, holiday_bytes, peer_row, sealed_blob,
        spool_blobs,
    };
    use crate::provider_face::owned_tree::StagingTree;

    // The set's custody under the fixtures' content key and nonce, so the
    // peer's signed, sealed row verifies and opens.
    let (row, mut cfg) = bound_set(&[(CONTENT_KEY, 1_000)], Some(1));
    for entry in &mut cfg.sets {
        entry.set_nonce = Some(SET_NONCE);
    }
    let (state, _nest, keys) = built_online_then_cut_off(row, cfg).await;

    let dir = tempfile::tempdir().unwrap();
    let tree = StagingTree::new(dir.path().join("staging"));
    std::fs::create_dir_all(tree.staging_root()).unwrap();
    let built = build_at(
        state.path(),
        tree.staging_root(),
        FolderRef::Local(SET_ID),
        &unreachable_nest().await,
        keys,
        SECRET,
    )
    .await
    .expect("the cold build stands on the record");
    built
        .engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None, &[])
        .unwrap();

    let draft = holiday_bytes();
    let blob = sealed_blob(&draft);
    let mut pending = peer_row(0, "cabin/draft.mp4", blob.manifest_hash, "create");
    pending.sequenced = false;
    let spool = tempfile::tempdir().unwrap();
    spool_blobs(spool.path(), &[&blob]);
    let (report, _) = share_ingest_on_demand(
        &built.engine,
        &tree,
        &PEER_ACTOR_HEX,
        &encoded(&[pending]),
        spool.path().to_path_buf(),
    )
    .await
    .expect("door");
    assert_eq!(report.refused, 0, "report: {report:?}");
    assert_eq!(
        std::fs::read(tree.staging_root().join("cabin/draft.mp4"))
            .ok()
            .as_deref(),
        Some(&draft[..]),
        "the peer's body lands in the staging root"
    );
    assert_eq!(
        tree.kept_body(&built.engine, "cabin/draft.mp4").unwrap(),
        Some(tree.staging_root().join("cabin/draft.mp4")),
        "fetchContents answers from the kept root with the nest out of reach"
    );
    assert_eq!(
        built.publish_hold(),
        Some(SealHold::Unreadable),
        "landing publishes nothing"
    );
}

/// Decision 2″ (5) under a resident host's edge: installing the edge on an
/// engine built from the record keeps its floor unread — publication held,
/// the edge being no read — and the first read that reaches the nest lifts
/// the hold, asking the host to rebuild once the row reads differently from
/// the one recorded.
#[tokio::test]
async fn an_engine_built_on_the_record_holds_until_a_read_and_rebuilds_on_a_moved_row() {
    use std::sync::atomic::AtomicUsize;

    use crate::binding_edge::BindingEdge;

    let (row, cfg) = bound_set(&[([0x42; 32], 1_000)], None);
    let (state, _nest, keys) = built_online_then_cut_off(row.clone(), cfg).await;
    let BuiltEngine { engine, basis, .. } = build(
        state.path(),
        FolderRef::Local(SET_ID),
        &unreachable_nest().await,
        keys,
        SECRET,
    )
    .await
    .expect("the cold build stands on the record");
    let rebuilds = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&rebuilds);
    let engine = engine.with_binding_edge(BindingEdge {
        folder_ref: FolderRef::Local(SET_ID),
        basis,
        on_rebuild: Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    });
    assert_eq!(
        engine.publish_hold(),
        Some(SealHold::Unreadable),
        "installing the edge is no read of the row"
    );

    engine.apply_binding_read(&[(SET_ID, BindingBasis::of(&row))]);
    assert_eq!(
        engine.publish_hold(),
        None,
        "a read in reach lifts the hold"
    );
    assert_eq!(
        rebuilds.load(Ordering::SeqCst),
        0,
        "the row reads as recorded"
    );

    let mut moved = row;
    moved.content_key_floor = Some(1);
    engine.apply_binding_read(&[(SET_ID, BindingBasis::of(&moved))]);
    assert_eq!(
        rebuilds.load(Ordering::SeqCst),
        1,
        "a row that reads differently rebuilds"
    );
}
