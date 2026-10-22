//! A row no held key opens holds the anchor for a bounded time, then passes —
//! and a content-key gain re-folds it (`docs/goal/behavior/file-sync.md`
//! § *A failed change must not strand the device*, TRANSIENT IS THE DEFAULT,
//! the KEY-PENDING class; `writer-signed-change-records.md` ruling (8)(f)).
//!
//! A writer co-member's row of random chunk bytes, a label sealed under no
//! root this reader holds, or a generation stamped above any held, all fail
//! every open after every fetch succeeded. Unbounded, each held every other
//! member's catch-up below it for ever. Each test drives the
//! bound with a pinned clock, never a sleep.

use std::sync::Arc;

use fauna_core::apply_failure::{KEY_PENDING_HOLD, PermanentApplyFailure};
use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderContentKeys;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_core::path_crypto::LabelRoot;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::sync::SyncChange;
use fauna_protocol::sync_row_verify::ReaderBinding;
use fauna_protocol::sync_writer_sig::{ChangeSigner, SignedChange};
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::catchup_failed_report_test::{create_change_with_manifest, test_sync_engine};
use crate::db::SyncDb;
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::{BlobStore, MockNest};
use crate::transfer::TransferPool;

const FOLDER: &str = "__test";
const HOSTILE: &str = "hostile.bin";
const HONEST: &str = "honest.txt";
const HONEST_BODY: &[u8] = b"an honest peer's change after the unopenable one";
const T0: i64 = 1_900_000_000;

fn hold() -> i64 {
    KEY_PENDING_HOLD.as_secs() as i64
}

fn owner_key() -> BackupKey {
    BackupKey::from_bytes([0x61u8; 32])
}

const SET_NONCE: [u8; 32] = [7; 32];

/// The set's owner — this reader's account, and the writer that signs every
/// row here, so the head re-judge's judge admits them.
fn owner() -> ActorKeypair {
    ActorKeypair::from_secret([0x51; 32])
}

/// `row` signed by [`owner`] under the set's nonce — LAST, over its final
/// fields.
fn signed(mut row: SyncChange) -> SyncChange {
    row.author_actor_id = Some(owner().actor_id().to_hex());
    let key = ChangeSigner::direct(&owner());
    let statement = SignedChange::for_row(&row, SET_NONCE).unwrap();
    row.signature = Some(fauna_protocol::ByteBuf::from(
        key.sign_statement(&statement).to_vec(),
    ));
    row.signer_key = Some(fauna_protocol::ByteBuf::from(key.signer_key().to_vec()));
    row
}

/// A peer's signed create of `path` naming `manifest`.
fn row(seq: i64, path: &str, manifest: ContentHash) -> SyncChange {
    signed(create_change_with_manifest(seq, path, manifest))
}

/// A reader on `server`'s byte plane over an on-disk db, so a rebuild with
/// merged custody keeps the anchor, the skip records and the trigger tokens.
fn reader(
    server: &MockServer,
    watch: &std::path::Path,
    db: &std::path::Path,
    content_keys: Option<FolderContentKeys>,
) -> SyncEngine {
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server.uri(),
        ActorKeypair::generate(),
        bearer,
        reqwest::Client::new(),
    ));
    let device = [0u8; 32];
    let mls_group_id = content_keys.as_ref().map(|_| vec![7u8; 16]);
    let engine = SyncEngine::new(
        watch.to_path_buf(),
        SyncDb::open(db).unwrap(),
        SyncClient::new(auth, &device),
        Some(FOLDER.to_string()),
        device,
        None,
        None,
        Some(owner_key().into()),
        mls_group_id,
        content_keys,
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        fauna_client::NestClient::new(server.uri(), owner()),
        crate::config::SyncMode::Sync,
    );
    engine.set_reader_binding(ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(owner().actor_id().0),
        account: Some(owner().actor_id().0),
        ..Default::default()
    });
    engine
}

/// Seed a manifest over `(plaintext hash it claims, stored chunk body)` pairs,
/// its hash list sealed under `root`, as a writer holding `root` would.
fn seed_manifest(
    store: &BlobStore,
    root: &[u8; 32],
    file_hash: ContentHash,
    chunks: Vec<(ContentHash, Vec<u8>, u64)>,
) -> ContentHash {
    let mut stored = Vec::new();
    for (_, body, _) in &chunks {
        let key = ContentHash::of_raw(body);
        store.seed_chunk(key, body.clone());
        stored.push(key);
    }
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash,
        total_size: chunks.iter().map(|(_, _, n)| n).sum(),
        chunk_hashes: chunks.iter().map(|(h, _, _)| *h).collect(),
        chunk_sizes: chunks.iter().map(|(_, _, n)| *n).collect(),
        stored_hashes: Some(stored),
        sealed_hashes: None,
        min_reader: None,
    };
    let bytes =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(root)).unwrap()).unwrap();
    let hash = ContentHash::of_raw(&bytes);
    store.seed_manifest(hash, bytes);
    hash
}

/// An honest one-chunk file sealed under `root`.
fn seed_honest(store: &BlobStore, root: &[u8; 32], body: &[u8]) -> ContentHash {
    let plain = ContentHash::of_raw(body);
    let framed = fauna_core::chunk_seal::FramedChunk::arbitrary_for_fixtures(
        plain,
        fauna_core::compress::compress_chunk(body),
    );
    let (_, sealed) = fauna_core::chunk_seal::seal_framed_chunk(&framed, root).unwrap();
    seed_manifest(store, root, plain, vec![(plain, sealed, body.len() as u64)])
}

/// A file whose one stored chunk is random bytes: content-addressed, so the
/// nest stores it, and no root's tag passes over it.
fn seed_random_chunk(store: &BlobStore, root: &[u8; 32]) -> ContentHash {
    let claimed = ContentHash::of_raw(b"what the manifest claims");
    let random: Vec<u8> = (0..96u32).map(|i| (i * 37 + 11) as u8).collect();
    seed_manifest(store, root, claimed, vec![(claimed, random, 24)])
}

/// [`row`] stamped with content-key generation `version`.
fn stamped(seq: i64, path: &str, manifest: ContentHash, version: u64) -> SyncChange {
    let mut change = create_change_with_manifest(seq, path, manifest);
    change.content_key_version = Some(version);
    signed(change)
}

/// A row whose label is a well-formed envelope this reader's roots do not
/// open (sealed under a root it never held).
fn unopenable_label_row(seq: i64) -> SyncChange {
    signed(SyncChange {
        path: None,
        path_sealed: Some(
            fauna_core::label_custody::seal_path(
                &LabelRoot::owner_of(&BackupKey::from_bytes([0x99u8; 32])),
                HOSTILE,
            )
            .unwrap()
            .into(),
        ),
        ..create_change_with_manifest(seq, HOSTILE, ContentHash::of_raw(b"never fetched"))
    })
}

async fn apply(engine: &SyncEngine, rows: &[SyncChange], at: i64) {
    engine.set_key_pending_clock(at);
    let mut rows = rows.to_vec();
    engine.open_sealed_change_paths(&mut rows);
    let since = engine.db().get_anchor().unwrap();
    engine
        .apply_remote_changes(&rows, since)
        .await
        .expect("apply_remote_changes");
}

fn catchup_failures(engine: &SyncEngine, path: &str) -> Vec<String> {
    engine
        .db()
        .list_unresolved_conflicts()
        .unwrap()
        .into_iter()
        .filter(|(_, p, kind, _, _)| p == path && kind == "catchup_failed")
        .map(|(_, _, _, details, _)| details.unwrap_or_default())
        .collect()
}

/// The shared walk: the hostile row 7 holds the anchor inside the bound,
/// passes at it, and the honest row 8 behind it lands. `recorded_as` is the
/// handle the skip is filed under (the path, or the row's hash for a label
/// that never opened).
async fn assert_held_then_passed(
    engine: &SyncEngine,
    watch: &std::path::Path,
    rows: &[SyncChange],
    recorded_as: &str,
) {
    apply(engine, rows, T0).await;
    assert_eq!(engine.db().get_anchor().unwrap(), 6, "held at first");
    apply(engine, rows, T0 + hold() - 1).await;
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        6,
        "still held inside the bound"
    );
    assert!(!watch.join(HONEST).exists());

    apply(engine, rows, T0 + hold()).await;
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        8,
        "past the bound the anchor passes the unopenable row and the honest one"
    );
    assert_eq!(std::fs::read(watch.join(HONEST)).unwrap(), HONEST_BODY);
    assert!(!watch.join(HOSTILE).exists());
    assert_eq!(
        catchup_failures(engine, recorded_as),
        vec![PermanentApplyFailure::KEY_PENDING.reason.to_string()],
        "recorded once, as key-pending"
    );
    assert!(
        engine.causal().skip_noted(&rows[0].path_hash, rows[0].seq),
        "the per-path floor notes the skip"
    );
}

/// An owner reader and a peer's honest change on the mock nest.
async fn owner_scenario(
    server: &MockServer,
) -> (SyncEngine, tempfile::TempDir, BlobStore, ContentHash) {
    let store = MockNest::new().mount(server).await;
    let peer_watch = tempfile::tempdir().unwrap();
    let peer = test_sync_engine(
        &server.uri(),
        peer_watch.path().to_path_buf(),
        [0xAAu8; 32],
        FOLDER,
    );
    std::fs::write(peer_watch.path().join(HONEST), HONEST_BODY).unwrap();
    peer.upload_file(HONEST).await.expect("peer upload_file");
    let honest = store.expect_last_manifest_hash();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("w")).unwrap();
    let engine = reader(
        server,
        &dir.path().join("w"),
        &dir.path().join("sync.db"),
        None,
    );
    engine.db().set_anchor(6).unwrap();
    (engine, dir, store, honest)
}

/// (a) Random chunk bytes under a well-formed manifest.
#[tokio::test]
async fn a_random_chunk_row_holds_inside_the_bound_and_passes_after_it() {
    let server = MockServer::start().await;
    let (engine, dir, store, honest) = owner_scenario(&server).await;
    let hostile = seed_random_chunk(&store, &owner_key().convergent_chunk_root());
    let rows = [row(7, HOSTILE, hostile), row(8, HONEST, honest)];

    assert_held_then_passed(&engine, &dir.path().join("w"), &rows, HOSTILE).await;
}

/// (a), a second hostile row: the bound's clock is per lowest held seq. Once
/// row 7 passes, row 9 is the lowest held and holds on a clock of its own
/// that starts then — neither riding row 7's elapsed hold nor stranded
/// behind row 7's spent one.
#[tokio::test]
async fn a_second_unopenable_row_holds_on_its_own_clock_after_the_first_passes() {
    let server = MockServer::start().await;
    let (engine, dir, store, honest) = owner_scenario(&server).await;
    let root = owner_key().convergent_chunk_root();
    let hostile = seed_random_chunk(&store, &root);
    const LATER: &[u8] = b"an honest change behind the second unopenable row";
    let later = seed_honest(&store, &root, LATER);
    let rows = [
        row(7, HOSTILE, hostile),
        row(8, HONEST, honest),
        row(9, "hostile-2.bin", hostile),
        row(10, "later.txt", later),
    ];
    let watch = dir.path().join("w");

    apply(&engine, &rows, T0).await;
    apply(&engine, &rows, T0 + hold()).await;
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        8,
        "row 7 passed, row 9 held"
    );
    apply(&engine, &rows, T0 + 2 * hold() - 1).await;
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        8,
        "row 9's clock started when it became the lowest held"
    );
    assert!(!watch.join("later.txt").exists());

    apply(&engine, &rows, T0 + 2 * hold()).await;
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        10,
        "row 9 passes on its own bound"
    );
    assert_eq!(std::fs::read(watch.join("later.txt")).unwrap(), LATER);
}

/// (b) A well-formed label envelope no held root opens — no chunk upload
/// needed, the cheapest of the three.
#[tokio::test]
async fn an_unopenable_label_row_holds_inside_the_bound_and_passes_after_it() {
    let server = MockServer::start().await;
    let (engine, dir, _store, honest) = owner_scenario(&server).await;
    let label = unopenable_label_row(7);
    let recorded_as = label.path_hash.clone();
    let rows = [label, row(8, HONEST, honest)];

    assert_held_then_passed(&engine, &dir.path().join("w"), &rows, &recorded_as).await;
}

/// A content-keyed reader holding `keys`, rows `[hostile 7, honest 8]` whose
/// honest half is sealed under `honest_root` at `honest_version`.
async fn keyed_scenario(
    server: &MockServer,
    keys: FolderContentKeys,
) -> (SyncEngine, tempfile::TempDir, BlobStore) {
    let store = MockNest::new().mount(server).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("w")).unwrap();
    let engine = reader(
        server,
        &dir.path().join("w"),
        &dir.path().join("sync.db"),
        Some(keys),
    );
    engine.db().set_anchor(6).unwrap();
    (engine, dir, store)
}

fn k(b: u8) -> [u8; 32] {
    [b; 32]
}

/// (c) A generation stamped above any that exists — the nest checks a floor,
/// never a ceiling.
#[tokio::test]
async fn an_over_stamped_generation_holds_inside_the_bound_and_passes_after_it() {
    let server = MockServer::start().await;
    let (engine, dir, store) =
        keyed_scenario(&server, FolderContentKeys::genesis(k(1), 1_000)).await;
    let hostile = seed_honest(&store, &k(1), b"sealed under a generation that exists");
    let honest = seed_honest(&store, &k(1), HONEST_BODY);
    let rows = [
        stamped(7, HOSTILE, hostile, 99),
        stamped(8, HONEST, honest, 1),
    ];

    assert_held_then_passed(&engine, &dir.path().join("w"), &rows, HOSTILE).await;
}

/// (d) The case the bound must not lose: two owner devices rotated at once,
/// and an honest row sealed under the second version-2 key reaches a reader
/// that holds only the first. It passes key-pending; the custody merge that
/// brings the second key is a reader gain, and the re-judge folds the row and
/// resolves its record.
#[tokio::test]
async fn a_concurrent_rotation_row_passes_key_pending_and_a_key_gain_refolds_it() {
    let server = MockServer::start().await;
    let mut held = FolderContentKeys::genesis(k(1), 1_000);
    held.rotate(k(0xA0), 2_000);
    let mut other = FolderContentKeys::genesis(k(1), 1_000);
    other.rotate(k(0xB0), 2_000);
    let (engine, dir, store) = keyed_scenario(&server, held.clone()).await;
    const LATE: &[u8] = b"sealed under the other device's version-2 key";
    let late = seed_honest(&store, &k(0xB0), LATE);
    let honest = seed_honest(&store, &k(0xA0), HONEST_BODY);
    let rows = [stamped(7, HOSTILE, late, 2), stamped(8, HONEST, honest, 2)];
    let watch = dir.path().join("w");
    // The trigger state as this reader last saw it.
    engine.note_reader_gain().unwrap();
    engine.db().set_head_rejudge_owed(false).unwrap();

    assert_held_then_passed(&engine, &watch, &rows, HOSTILE).await;
    drop(engine);

    let merged = reader(
        &server,
        &watch,
        &dir.path().join("sync.db"),
        Some(held.merge(&other)),
    );
    assert!(
        merged.note_reader_gain().unwrap(),
        "a same-version key arriving by custody merge is a reader gain"
    );
    let rejudge = merged
        .fold_passed_heads(rows.to_vec(), &[], 8)
        .await
        .expect("fold_passed_heads");
    assert!(rejudge.complete, "{rejudge:?}");
    assert_eq!(rejudge.folded, 1, "{rejudge:?}");
    assert_eq!(std::fs::read(watch.join(HOSTILE)).unwrap(), LATE);
    assert!(
        catchup_failures(&merged, HOSTILE).is_empty(),
        "the re-fold resolves the key-pending record"
    );
}

/// (e) A fetch failure is never key-pending: an outage must not skip the tail,
/// however long it lasts.
#[tokio::test]
async fn a_chunk_fetch_failure_stays_held_past_the_bound() {
    let server = MockServer::start().await;
    let (engine, dir, store, honest) = owner_scenario(&server).await;
    let root = owner_key().convergent_chunk_root();
    let missing = seed_random_chunk(&store, &root);
    store.chunks.lock().unwrap().clear();
    let rows = [row(7, HOSTILE, missing), row(8, HONEST, honest)];

    for at in [T0, T0 + hold(), T0 + 10 * hold()] {
        apply(&engine, &rows, at).await;
        assert_eq!(engine.db().get_anchor().unwrap(), 6, "held at {at}");
    }
    assert!(!dir.path().join("w").join(HONEST).exists());
    assert!(catchup_failures(&engine, HOSTILE).is_empty());
}

/// (f) A re-judge over a key-pending head that still does not open counts it
/// judged — else every pull would re-read the whole log for a hostile row —
/// and nothing re-owes it until something is gained.
#[tokio::test]
async fn a_rejudge_over_a_still_unopenable_key_pending_head_completes() {
    let server = MockServer::start().await;
    let (engine, dir, store, honest) = owner_scenario(&server).await;
    let hostile = seed_random_chunk(&store, &owner_key().convergent_chunk_root());
    let rows = [row(7, HOSTILE, hostile), row(8, HONEST, honest)];
    // Control: before it passed, the same head reaches the fold, fails
    // key-pending and leaves the pass owed.
    let before = engine
        .fold_passed_heads(vec![rows[0].clone()], &[], 7)
        .await
        .expect("fold_passed_heads");
    assert!(!before.complete, "{before:?}");
    assert_held_then_passed(&engine, &dir.path().join("w"), &rows, HOSTILE).await;
    engine.note_reader_gain().unwrap();
    engine.db().set_head_rejudge_owed(false).unwrap();
    let fetches = store.fetched_manifests.lock().unwrap().len();

    let rejudge = engine
        .fold_passed_heads(rows.to_vec(), &[], 8)
        .await
        .expect("fold_passed_heads");
    assert!(rejudge.complete, "{rejudge:?}");
    assert_eq!(rejudge.folded, 0);
    assert!(
        store.fetched_manifests.lock().unwrap().len() > fetches,
        "the head was read again, not skipped unread"
    );
    assert_eq!(
        catchup_failures(&engine, HOSTILE).len(),
        1,
        "still recorded"
    );
    assert!(
        !engine.note_reader_gain().unwrap(),
        "nothing gained, nothing owed"
    );
}

/// (f), the label arm: a sealed head whose label still opens under no root
/// is judged once it passed key-pending.
#[tokio::test]
async fn a_rejudge_over_a_still_unopenable_key_pending_label_completes() {
    let server = MockServer::start().await;
    let (engine, dir, _store, honest) = owner_scenario(&server).await;
    let label = unopenable_label_row(7);
    let recorded_as = label.path_hash.clone();
    let rows = [label, row(8, HONEST, honest)];
    // Control: before it passed, the same head leaves the pass owed.
    let before = engine
        .fold_passed_heads(vec![rows[0].clone()], &[], 7)
        .await
        .expect("fold_passed_heads");
    assert!(!before.complete, "{before:?}");
    assert_held_then_passed(&engine, &dir.path().join("w"), &rows, &recorded_as).await;

    let rejudge = engine
        .fold_passed_heads(rows.to_vec(), &[], 8)
        .await
        .expect("fold_passed_heads");
    assert!(rejudge.complete, "{rejudge:?}");
}
