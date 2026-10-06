//! The peer-share ingest end to end at tier_1 (B2.3 —
//! [`SyncEngine::ingest_peer_share_rows`]; `p2p-shared-set-build.md` § *Build design — the row
//! half*): a real engine over a real state DB, a sealed fixture (a share-leg
//! fixture must be sealed — the store-key/body identity the transfer boundary
//! verifies only holds there), and a scripted per-set fetcher standing in for
//! `PeerShareBlobFetcher`. No nest anywhere: the whole point of the
//! provisional plane is that it runs without one.

use std::collections::HashMap;
use std::sync::Arc;

use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderContentKeys;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};

use crate::adaptive::AdaptiveConcurrency;
use crate::db::{SyncDb, SyncState};
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

// ── The set's M2 content key, generation 1 ───────────────────────────────────

pub(crate) const CONTENT_KEY: [u8; 32] = [0x7E; 32];
/// The serving peer — a real key, because since the writer-signed change
/// records switch flipped every row it serves as its own is signed by it.
pub(crate) fn serving_peer() -> ActorKeypair {
    ActorKeypair::from_secret([0x0D; 32])
}
pub(crate) static PEER_ACTOR_HEX: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| serving_peer().actor_id().to_hex());

/// Sign `change` as the serving peer's own row under the set's nonce — what
/// the peer's replica recorded (its `device_id` must be a 32-byte hex id).
pub(crate) fn sign_as_serving_peer(change: &mut fauna_protocol::sync::SyncChange) {
    change.author_actor_id = Some(PEER_ACTOR_HEX.to_string());
    fauna_protocol::sync_writer_sig::ChangeSigner::direct(&serving_peer())
        .sign_row(change, SET_NONCE)
        .expect("sign");
}

/// Strip the writer signature — a row as an unsigned writer would serve it.
fn unsigned(
    mut row: fauna_protocol::peer_share::PeerShareChange,
) -> fauna_protocol::peer_share::PeerShareChange {
    row.change.signature = None;
    row.change.signer_key = None;
    row
}

/// An engine bound to a shared set (content keys present) whose clients are
/// deliberately unconnected — ingest needs neither. Shared with the serve-side
/// twin (`peer_share_serve_test`), which needs exactly this engine shape.
pub(crate) fn ingest_engine(watch_dir: std::path::PathBuf) -> SyncEngine {
    ingest_engine_with_db(watch_dir, SyncDb::open_in_memory().unwrap())
}

/// [`ingest_engine`] over a caller-seeded state DB — the serve-side tests
/// retain manifests / entries before the engine takes the DB by value.
///
/// Bound under the set's nonce and owner — the binding every engine host
/// installs from custody — so a signed peer row verifies.
pub(crate) fn ingest_engine_with_db(watch_dir: std::path::PathBuf, db: SyncDb) -> SyncEngine {
    ingest_engine_at_with_db("http://127.0.0.1:9", watch_dir, db)
}

/// [`ingest_engine`] pointed at a real (mock) nest — the reconcile tests
/// drive `apply_remote_changes`, whose download arm fetches manifests and
/// chunks from the nest's HTTP routes.
fn ingest_engine_at(url: &str, watch_dir: std::path::PathBuf) -> SyncEngine {
    ingest_engine_at_with_db(url, watch_dir, SyncDb::open_in_memory().unwrap())
}

fn ingest_engine_at_with_db(url: &str, watch_dir: std::path::PathBuf, db: SyncDb) -> SyncEngine {
    let kp = ActorKeypair::generate();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        url.to_string(),
        kp,
        bearer,
        reqwest::Client::new(),
    ));
    let engine = SyncEngine::new(
        watch_dir,
        db,
        SyncClient::new(auth, &[0u8; 32]),
        Some("__test".to_string()),
        [0u8; 32],
        None,                 // mls
        None,                 // epoch_secret
        None,                 // backup_key — a shared set's chunks seal under the content key
        Some(vec![0x11; 32]), // bound marker
        Some(FolderContentKeys::genesis(CONTENT_KEY, 1_760_000_000)),
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        fauna_client::NestClient::new(url.to_string(), ActorKeypair::generate()),
        crate::config::SyncMode::Sync,
    );
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(set_owner().actor_id().0),
        ..Default::default()
    });
    engine
}

/// Mount `blobs`' manifests and chunk bodies on a mock nest — the shared
/// [`MockNest`] byte plane, seeded from fixtures rather than populated by a
/// real upload, which is all this leg needs (it only ever GETs).
async fn mount_nest_blobs(server: &wiremock::MockServer, blobs: &[&Blob]) {
    let nest = MockNest::new();
    for b in blobs {
        nest.store()
            .seed_manifest(b.manifest_hash, b.manifest_bytes.clone());
        for (k, body) in &b.chunks {
            nest.store().seed_chunk(*k, body.clone());
        }
    }
    nest.mount(server).await;
}

/// Seal `bytes` the way a shared set's chunks rest (chunk → frame → encrypt
/// under the content key → re-key by ciphertext hash) — the
/// `share_leg_over_the_channel.rs` fixture, reproduced for the same reason it
/// documents: a plaintext fixture tests a corpus this leg never sees.
pub(crate) struct Blob {
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) manifest_hash: ContentHash,
    pub(crate) chunks: Vec<(ContentHash, Vec<u8>)>,
}

pub(crate) fn sealed_blob(bytes: &[u8]) -> Blob {
    let mut manifest = fauna_core::chunker::chunk_file(bytes);
    let plaintext = fauna_core::chunker::extract_chunks(bytes, &manifest);
    // Through the one seal door — the exact bytes the sharing engine stores.
    let stored: Vec<(ContentHash, Vec<u8>)> =
        fauna_core::chunk_seal::seal_chunk_bodies(&plaintext, &CONTENT_KEY)
            .expect("seal the chunks under the set's content key");
    manifest.stored_hashes = Some(stored.iter().map(|(k, _)| *k).collect());
    let manifest_bytes = fauna_core::encoding::canonical_encode(
        &manifest
            .wire_form(Some(&CONTENT_KEY))
            .expect("seal the manifest's hashes"),
    )
    .expect("encode manifest");
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    Blob {
        manifest_bytes,
        manifest_hash,
        chunks: stored,
    }
}

/// The scripted per-set fetcher — `PeerShareBlobFetcher`'s stand-in, serving
/// the fixture's manifests and chunk bodies by address.
pub(crate) struct FixtureFetcher {
    manifests: HashMap<[u8; 32], Vec<u8>>,
    chunks: HashMap<[u8; 32], Vec<u8>>,
}

impl FixtureFetcher {
    pub(crate) fn serving(blobs: &[&Blob]) -> Self {
        let mut manifests = HashMap::new();
        let mut chunks = HashMap::new();
        for b in blobs {
            manifests.insert(b.manifest_hash.digest(), b.manifest_bytes.clone());
            for (k, body) in &b.chunks {
                chunks.insert(k.digest(), body.clone());
            }
        }
        Self { manifests, chunks }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_core::file_download::BlobFetcher for FixtureFetcher {
    async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
        self.manifests
            .get(&hash.digest())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("fixture holds no manifest {hash:?}"))
    }
    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        _relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        store_keys
            .iter()
            .map(|k| {
                self.chunks
                    .get(&k.digest())
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("fixture holds no chunk {k:?}"))
            })
            .collect()
    }
}

/// A sequenced peer row for `path`, authored, signed and stamped by the peer.
pub(crate) fn peer_row(
    seq: i64,
    path: &str,
    manifest_hash: ContentHash,
    change_type: &str,
) -> fauna_protocol::peer_share::PeerShareChange {
    let mut row = fauna_protocol::peer_share::PeerShareChange {
        change: fauna_protocol::sync::SyncChange {
            seq,
            path_hash: hex::encode(fauna_core::sync::path_hash(path)),
            manifest_hash: (change_type != "delete").then(|| hex::encode(manifest_hash.digest())),
            size_bytes: 300_000,
            change_type: change_type.to_string(),
            created_at: 1_760_000_111_000,
            path: Some(path.to_string()),
            device_id: Some("d7".repeat(32)),
            content_key_version: (change_type != "delete").then_some(1),
            ..Default::default()
        },
        sequenced: true,
        ..Default::default()
    };
    sign_as_serving_peer(&mut row.change);
    row
}

pub(crate) fn holiday_bytes() -> Vec<u8> {
    b"the spouses' holiday clip, frame after frame. "
        .iter()
        .cycle()
        .take(300_000)
        .copied()
        .collect()
}

/// The success criterion's first arm: a peer-ingested provisional create
/// MATERIALIZES — bytes on disk, entry clean-synced (so the scan never
/// re-uploads peer content as this replica's own authorship) — and the
/// dehydration proof is WITHHELD: the nest does not hold these bytes, so the
/// row must read dehydration-unsafe until the nest confirms.
#[tokio::test]
async fn a_provisional_create_materializes_without_the_dehydration_proof() {
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine(watch.path().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();

    let bytes = holiday_bytes();
    let blob = sealed_blob(&bytes);
    let fetcher = FixtureFetcher::serving(&[&blob]);
    let rel = "clips/holiday.mp4";

    let report = engine
        .ingest_peer_share_rows(
            &[peer_row(4, rel, blob.manifest_hash, "create")],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest");
    assert_eq!(report.materialized, 1, "report: {report:?}");
    assert_eq!(report.refused, 0);

    let on_disk = std::fs::read(watch.path().join(rel)).expect("materialized file");
    assert_eq!(on_disk, bytes, "the plaintext came through the sealed walk");

    let entry = engine.db().get_entry(rel).unwrap().expect("entry exists");
    assert_eq!(entry.state, SyncState::Synced, "scan-inert commit");
    assert_eq!(entry.local_hash, Some(ContentHash::of_raw(&bytes)));
    assert_eq!(entry.manifest_hash, Some(blob.manifest_hash));
    assert!(
        !engine.is_dehydration_safe(rel),
        "the dehydration proof must be WITHHELD — the nest does not hold these bytes"
    );

    let overlay = engine
        .db()
        .get_share_overlay(rel)
        .unwrap()
        .expect("overlay row");
    assert!(overlay.materialized);
    assert_eq!(
        overlay.content_hash.as_deref(),
        Some(hex::encode(ContentHash::of_raw(&bytes).digest()).as_str())
    );

    // No nest-log accounting moved: the anchor is untouched by provisional rows.
    assert_eq!(engine.db().get_anchor().unwrap(), 0);
}

/// The engine's own provenance door: without a cached writer role for the
/// serving peer, every row is refused HERE — whatever the pump checked.
#[tokio::test]
async fn a_peer_without_a_cached_writer_role_is_refused_at_the_engine_door() {
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine(watch.path().to_path_buf());
    // No roster cached at all — the fail-closed default.

    let blob = sealed_blob(&holiday_bytes());
    let fetcher = FixtureFetcher::serving(&[&blob]);

    let report = engine
        .ingest_peer_share_rows(
            &[peer_row(
                4,
                "clips/holiday.mp4",
                blob.manifest_hash,
                "create",
            )],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest");
    assert_eq!(report.refused, 1);
    assert_eq!(report.overlaid, 0, "a refused row lands nowhere");
    assert!(
        engine
            .db()
            .get_share_overlay("clips/holiday.mp4")
            .unwrap()
            .is_none()
    );
    assert!(!watch.path().join("clips/holiday.mp4").exists());
}

/// The success criterion's third arm: a provisional DELETE is view-only —
/// the overlay records it, and the local file plus its entry stay untouched
/// (local bytes die only on nest-confirmed deletes).
#[tokio::test]
async fn a_provisional_delete_leaves_the_local_file_on_disk() {
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine(watch.path().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();

    // Materialize a create first, so there is something to "delete".
    let bytes = holiday_bytes();
    let blob = sealed_blob(&bytes);
    let fetcher = FixtureFetcher::serving(&[&blob]);
    let rel = "clips/holiday.mp4";
    engine
        .ingest_peer_share_rows(
            &[peer_row(4, rel, blob.manifest_hash, "create")],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest create");

    let report = engine
        .ingest_peer_share_rows(
            &[peer_row(9, rel, blob.manifest_hash, "delete")],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest delete");
    assert_eq!(report.overlaid, 1);

    assert!(
        watch.path().join(rel).exists(),
        "a provisional delete never destroys local bytes"
    );
    assert!(
        engine.db().get_entry(rel).unwrap().is_some(),
        "the entry stands — only a nest-confirmed delete retires it"
    );
    let overlay = engine
        .db()
        .get_share_overlay(rel)
        .unwrap()
        .expect("overlay row");
    assert_eq!(overlay.change_type, "delete");
    assert_eq!(
        overlay.seq, 9,
        "latest-per-path: the delete superseded the create"
    );
}

/// The misattribution/data-loss door: a path with local novelty (modified
/// state, or disk drifted from tracked state) is NEVER overwritten — the
/// overlay row lands unmaterialized and the report names the skip.
#[tokio::test]
async fn local_novelty_is_never_overwritten() {
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine(watch.path().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();

    let rel = "docs/draft.txt";
    let user_bytes = b"the user's unsynced draft".to_vec();
    std::fs::create_dir_all(watch.path().join("docs")).unwrap();
    std::fs::write(watch.path().join(rel), &user_bytes).unwrap();
    // Untracked-but-present is the sharpest arm: no entry, bytes on disk.

    let blob = sealed_blob(&holiday_bytes());
    let fetcher = FixtureFetcher::serving(&[&blob]);
    let report = engine
        .ingest_peer_share_rows(
            &[peer_row(4, rel, blob.manifest_hash, "create")],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest");

    assert_eq!(report.materialized, 0);
    assert_eq!(
        report.skipped,
        vec![(rel.to_string(), "untracked local file present")]
    );
    assert_eq!(
        std::fs::read(watch.path().join(rel)).unwrap(),
        user_bytes,
        "the user's bytes survived"
    );
    let overlay = engine
        .db()
        .get_share_overlay(rel)
        .unwrap()
        .expect("overlay row lands anyway");
    assert!(!overlay.materialized);
}

// ── B2.4 — the reconcile: retire-on-arrival ──────────────────────────────────

/// The success criterion's second arm, CONFIRM half: after a provisional
/// create materialized, the nest's sequenced row for the SAME manifest
/// arrives — the overlay annotation retires, the withheld dehydration proof
/// is stamped (the nest now provably holds what disk holds), and nothing
/// forks: same head, same bytes, no conflict.
#[tokio::test]
async fn the_nests_sequenced_row_confirms_and_stamps_the_withheld_proof() {
    let server = wiremock::MockServer::start().await;
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine_at(&server.uri(), watch.path().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();

    let bytes = holiday_bytes();
    let blob = sealed_blob(&bytes);
    let rel = "clips/holiday.mp4";
    let fetcher = FixtureFetcher::serving(&[&blob]);
    engine
        .ingest_peer_share_rows(
            &[peer_row(4, rel, blob.manifest_hash, "create")],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest create");
    assert!(
        !engine.is_dehydration_safe(rel),
        "proof withheld pre-confirm"
    );

    // The author's row reaches the nest and comes back sequenced; the nest
    // now serves the same manifest and chunks.
    mount_nest_blobs(&server, &[&blob]).await;
    let nest_row = peer_row(4, rel, blob.manifest_hash, "create").change;
    let batch = engine
        .apply_remote_changes(&[nest_row], 0)
        .await
        .expect("apply the nest's sequenced row");
    assert!(!batch.deferred, "nothing deferred in a one-row batch");

    assert!(
        engine.db().get_share_overlay(rel).unwrap().is_none(),
        "the provisional annotation retired on arrival"
    );
    assert!(
        engine.is_dehydration_safe(rel),
        "CONFIRM stamps the withheld proof — the nest provably holds these bytes now"
    );
    let entry = engine.db().get_entry(rel).unwrap().expect("entry stands");
    assert_eq!(
        entry.manifest_hash,
        Some(blob.manifest_hash),
        "same head — no fork"
    );
    assert_eq!(
        std::fs::read(watch.path().join(rel)).unwrap(),
        bytes,
        "same bytes — no fork"
    );
    assert!(
        !engine
            .db()
            .has_unresolved_conflict_for_path(rel)
            .unwrap_or(true),
        "no conflict minted"
    );
}

/// The success criterion's second arm, SUPERSEDE half: the nest sequenced a
/// DIFFERENT winner for the overlaid path. The nest row applies by the
/// ordinary rules (the materialized state reads clean-at-base, so the new
/// head fast-forwards over it), the overlay annotation retires, and the
/// materialized copy is neither uploaded nor retained as a conflict loser.
#[tokio::test]
async fn the_nests_different_winner_supersedes_without_forking() {
    let server = wiremock::MockServer::start().await;
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine_at(&server.uri(), watch.path().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();

    let provisional_bytes = holiday_bytes();
    let provisional = sealed_blob(&provisional_bytes);
    let rel = "clips/holiday.mp4";
    let fetcher = FixtureFetcher::serving(&[&provisional]);
    engine
        .ingest_peer_share_rows(
            &[peer_row(4, rel, provisional.manifest_hash, "create")],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest create");

    // A different edit won at the nest.
    let winner_bytes: Vec<u8> = b"the re-cut everyone preferred. "
        .iter()
        .cycle()
        .take(200_000)
        .copied()
        .collect();
    let winner = sealed_blob(&winner_bytes);
    mount_nest_blobs(&server, &[&winner]).await;
    engine
        .apply_remote_changes(
            &[peer_row(9, rel, winner.manifest_hash, "create").change],
            0,
        )
        .await
        .expect("apply the winner");

    assert!(
        engine.db().get_share_overlay(rel).unwrap().is_none(),
        "the provisional annotation retired on arrival"
    );
    let entry = engine.db().get_entry(rel).unwrap().expect("entry stands");
    assert_eq!(
        entry.manifest_hash,
        Some(winner.manifest_hash),
        "the nest's winner is the head"
    );
    assert_eq!(
        std::fs::read(watch.path().join(rel)).unwrap(),
        winner_bytes,
        "the winner's bytes are on disk — the materialized copy yielded"
    );
    assert!(
        !engine
            .db()
            .has_unresolved_conflict_for_path(rel)
            .unwrap_or(true),
        "the materialized copy was never this replica's authorship — no conflict, no loser retention"
    );
    assert!(
        engine.db().own_changes_since(0, 10).unwrap().is_empty(),
        "and it was never recorded as this replica's own change"
    );
}

/// A NEST-confirmed delete is the one that touches bytes: the ordinary delete
/// arm removes the file, and the reconcile retires the overlay annotation in
/// the same batch.
#[tokio::test]
async fn a_nest_confirmed_delete_removes_bytes_and_retires_the_overlay() {
    let server = wiremock::MockServer::start().await;
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine_at(&server.uri(), watch.path().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();

    let bytes = holiday_bytes();
    let blob = sealed_blob(&bytes);
    let rel = "clips/holiday.mp4";
    let fetcher = FixtureFetcher::serving(&[&blob]);
    engine
        .ingest_peer_share_rows(
            &[
                peer_row(4, rel, blob.manifest_hash, "create"),
                peer_row(5, rel, blob.manifest_hash, "delete"),
            ],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest create + provisional delete");
    assert!(
        watch.path().join(rel).exists(),
        "provisionally deleted, bytes still here"
    );

    engine
        .apply_remote_changes(&[peer_row(6, rel, blob.manifest_hash, "delete").change], 0)
        .await
        .expect("apply the nest-confirmed delete");

    assert!(
        !watch.path().join(rel).exists(),
        "a NEST-confirmed delete is what removes local bytes"
    );
    assert!(
        engine.db().get_share_overlay(rel).unwrap().is_none(),
        "the provisional annotation retired with it"
    );
}

/// Spool `blobs`' manifests and chunk bodies the way the pump does, so
/// [`crate::always_resident::ingest_share_page`] (the real door) reads them.
pub(crate) fn spool_blobs(spool: &std::path::Path, blobs: &[&Blob]) {
    std::fs::create_dir_all(spool.join("manifests")).unwrap();
    std::fs::create_dir_all(spool.join("chunks")).unwrap();
    for b in blobs {
        std::fs::write(
            crate::peer_share_store::spool_manifest_path(spool, &b.manifest_hash),
            &b.manifest_bytes,
        )
        .unwrap();
        for (k, body) in &b.chunks {
            std::fs::write(crate::peer_share_store::spool_chunk_path(spool, k), body).unwrap();
        }
    }
}

pub(crate) fn encoded(rows: &[fauna_protocol::peer_share::PeerShareChange]) -> Vec<Vec<u8>> {
    rows.iter()
        .map(|r| fauna_core::encoding::canonical_encode(r).unwrap().to_vec())
        .collect()
}

/// A transfer cut part-way leaves a SEQUENCED row whose bytes never arrived,
/// and the pull cursor must stay below it. The cursor is what the pump asks
/// the peer for next (`since`), so a cursor carried past that row means the
/// peer never serves it again. With the nest down (the scenario the plane
/// exists for) the row would then never arrive at all, which is the opposite
/// of *"an interrupted transfer picks up where it stopped"* (`p2p.md`
/// § Cross-user shared-set transfer). Rows beyond it that DID arrive are
/// re-served on the next pass and judged current, so nothing that arrived is
/// fetched again.
#[tokio::test]
async fn a_row_whose_bytes_did_not_arrive_holds_the_pull_cursor_below_it() {
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine(watch.path().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();

    let first = sealed_blob(b"the first letter, which arrived");
    let cut = sealed_blob(b"the second letter, cut off in the post");
    let third = sealed_blob(b"the third letter, which also arrived");
    let rows = [
        peer_row(5, "letters/one.txt", first.manifest_hash, "create"),
        peer_row(6, "letters/two.txt", cut.manifest_hash, "create"),
        peer_row(7, "letters/three.txt", third.manifest_hash, "create"),
    ];

    // Pass 1: the connection dropped before `two`'s bodies crossed, so the
    // spool holds only `one` and `three`.
    let spool_1 = tempfile::tempdir().unwrap();
    spool_blobs(spool_1.path(), &[&first, &third]);
    let (report, cursor) = crate::always_resident::ingest_share_page(
        &engine,
        &PEER_ACTOR_HEX,
        &encoded(&rows),
        spool_1.path().to_path_buf(),
    )
    .await
    .expect("pass 1 ingest");
    assert_eq!(report.materialized, 2, "report: {report:?}");
    assert_eq!(
        cursor, 5,
        "the cursor must stop BELOW the row whose bytes did not arrive (seq 6), \
         or the peer never serves it again: {report:?}"
    );
    assert!(!watch.path().join("letters/two.txt").exists());

    // Pass 2: the peer serves from the held cursor, so `two` and `three` come
    // again; only `two`'s bodies are spooled, the way the pump's plan skips
    // what is already current.
    let spool_2 = tempfile::tempdir().unwrap();
    spool_blobs(spool_2.path(), &[&cut]);
    let (report, cursor) = crate::always_resident::ingest_share_page(
        &engine,
        &PEER_ACTOR_HEX,
        &encoded(&rows[1..]),
        spool_2.path().to_path_buf(),
    )
    .await
    .expect("pass 2 ingest");
    assert_eq!(
        report.materialized, 1,
        "only the row that was cut: {report:?}"
    );
    assert_eq!(
        report.already_current, 1,
        "`three` arrived in pass 1: {report:?}"
    );
    assert_eq!(
        cursor, 7,
        "the whole page landed, so the cursor moves past it"
    );
    assert_eq!(
        std::fs::read(watch.path().join("letters/two.txt")).unwrap(),
        b"the second letter, cut off in the post"
    );
}

/// A row skipped for a reason no retry can change (here an unsafe path) must
/// NOT hold the cursor: holding it would re-serve the same refused row on
/// every pass for ever.
#[tokio::test]
async fn a_row_refused_for_good_does_not_hold_the_pull_cursor() {
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine(watch.path().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();

    let blob = sealed_blob(b"a body the path guard never lets land");
    let rows = [peer_row(9, "../outside.txt", blob.manifest_hash, "create")];
    let spool = tempfile::tempdir().unwrap();
    spool_blobs(spool.path(), &[&blob]);
    let (report, cursor) = crate::always_resident::ingest_share_page(
        &engine,
        &PEER_ACTOR_HEX,
        &encoded(&rows),
        spool.path().to_path_buf(),
    )
    .await
    .expect("ingest");
    assert_eq!(report.skipped.len(), 1, "report: {report:?}");
    assert_eq!(cursor, 9, "a permanent refusal moves the cursor on");
}

// ── The relayed-row lift (writer-signed change records, ruling (3)) ─────────

const SET_NONCE: [u8; 32] = [0x4E; 32];

/// The set's owner, a writer member, and a member holding no writer role.
fn set_owner() -> ActorKeypair {
    ActorKeypair::from_secret([0x0A; 32])
}
fn writer_member() -> ActorKeypair {
    ActorKeypair::from_secret([0x0B; 32])
}
fn read_only_member() -> ActorKeypair {
    ActorKeypair::from_secret([0x0C; 32])
}

/// `peer_row` re-attributed to `writer` and signed by it under `nonce` — the
/// row as the writer's own replica recorded it, now held by someone else.
fn signed_row(
    writer: &ActorKeypair,
    nonce: [u8; 32],
    seq: i64,
    path: &str,
    manifest_hash: ContentHash,
) -> fauna_protocol::peer_share::PeerShareChange {
    let mut row = peer_row(seq, path, manifest_hash, "create");
    row.change.author_actor_id = Some(writer.actor_id().to_hex());
    // A real device id — the signed statement carries it as 32 bytes.
    row.change.device_id = Some("d7".repeat(32));
    fauna_protocol::sync_writer_sig::ChangeSigner::direct(writer)
        .sign_row(&mut row.change, nonce)
        .expect("sign");
    row
}

/// An ingest engine that reads the set under its nonce and owner — the
/// binding every engine host installs from custody.
fn bound_ingest_engine(watch_dir: std::path::PathBuf, db: SyncDb) -> SyncEngine {
    let engine = ingest_engine_with_db(watch_dir, db);
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(set_owner().actor_id().0),
        ..Default::default()
    });
    engine
}

/// The lift, end to end over one N-member set: the peer B serves a row of a
/// THIRD writer it relays; this replica verifies it self-contained (the
/// signed actor is a cached writer — the reader never read its roster
/// offline), materializes it attributed to the WRITER, and keeps it for relay
/// onward; a second replica then pulls it from this one's share store and
/// verifies it too. Every member's rows cross, not only the relayer's.
#[tokio::test]
async fn a_third_writers_signed_row_relays_across_two_hops() {
    let writer = writer_member();
    let writer_hex = writer.actor_id().to_hex();
    let bytes = holiday_bytes();
    let blob = sealed_blob(&bytes);
    let fetcher = FixtureFetcher::serving(&[&blob]);
    let rel = "clips/holiday.mp4";
    let row = signed_row(&writer, SET_NONCE, 4, rel, blob.manifest_hash);

    // Hop 1: B → this replica (A), on an on-disk DB the share store reopens.
    let watch_a = tempfile::tempdir().unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("sync.db");
    let a = bound_ingest_engine(
        watch_a.path().to_path_buf(),
        SyncDb::open(&db_path).unwrap(),
    );
    a.db()
        .cache_share_writer_roster(
            &[
                (PEER_ACTOR_HEX.to_string(), true),
                (writer_hex.clone(), true),
            ],
            &[],
            None,
        )
        .unwrap();
    let report = a
        .ingest_peer_share_rows(std::slice::from_ref(&row), &PEER_ACTOR_HEX, &fetcher)
        .await
        .expect("ingest");
    assert_eq!(report.refused, 0, "report: {report:?}");
    assert_eq!(report.materialized, 1, "report: {report:?}");
    assert_eq!(
        report.channel_proven, 0,
        "a verified row is not a channel-proof one"
    );
    assert_eq!(std::fs::read(watch_a.path().join(rel)).unwrap(), bytes);
    assert_eq!(
        a.db()
            .get_share_overlay(rel)
            .unwrap()
            .unwrap()
            .proven_author,
        writer_hex,
        "attributed to the SIGNED writer, not the serving peer"
    );

    // A's share store now serves the writer's row — byte-exact, still signed.
    let store =
        crate::peer_share_store::SyncDbShareStore::new([0x5E; 32], SyncDb::open(&db_path).unwrap());
    let served = fauna_peer_share::server::ShareStore::changes_since(&store, &[0x5E; 32], 0, 100)
        .await
        .expect("serve");
    assert_eq!(served.len(), 1);
    assert!(!served[0].locally_authored, "held for relay, not A's own");
    assert_eq!(served[0].change, row.change, "relayed byte-exact");
    assert!(fauna_peer_share::serves_held_row(
        &served[0],
        &a.owner_actor_id_hex()
    ));

    // Hop 2: A → C. C knows A as a writer and the WRITER too — never asks
    // whether A wrote it.
    let watch_c = tempfile::tempdir().unwrap();
    let c = bound_ingest_engine(
        watch_c.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
    );
    c.db()
        .cache_share_writer_roster(&[(writer_hex.clone(), true)], &[], None)
        .unwrap();
    let hop2 = fauna_protocol::peer_share::PeerShareChange {
        change: served[0].change.clone(),
        sequenced: served[0].sequenced,
        signer_cert: served[0].signer_cert.clone(),
        ..Default::default()
    };
    let report = c
        .ingest_peer_share_rows(&[hop2], &a.owner_actor_id_hex(), &fetcher)
        .await
        .expect("ingest");
    assert_eq!(report.materialized, 1, "report: {report:?}");
    assert_eq!(std::fs::read(watch_c.path().join(rel)).unwrap(), bytes);
}

/// What the lift keeps refusing: a relayed row with no signature (nothing can
/// vouch for it — the serving peer's own writer role vouches for no one
/// else's row), a row a read-only member fabricates under its own key (its
/// chain holds; the roster, consulted for the SIGNED actor, does not), and a
/// writer's row bound to another set's nonce.
#[tokio::test]
async fn unsigned_relays_fabrications_and_foreign_set_rows_are_refused_at_the_engine_door() {
    let watch = tempfile::tempdir().unwrap();
    let engine = bound_ingest_engine(
        watch.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
    );
    let writer = writer_member();
    engine
        .db()
        .cache_share_writer_roster(
            &[
                (PEER_ACTOR_HEX.to_string(), true),
                (writer.actor_id().to_hex(), true),
            ],
            &[],
            None,
        )
        .unwrap();
    let blob = sealed_blob(&holiday_bytes());
    let fetcher = FixtureFetcher::serving(&[&blob]);

    let mut unsigned_relay = unsigned(peer_row(4, "a.mp4", blob.manifest_hash, "create"));
    unsigned_relay.change.author_actor_id = Some(writer.actor_id().to_hex());
    let fabricated = signed_row(
        &read_only_member(),
        SET_NONCE,
        5,
        "b.mp4",
        blob.manifest_hash,
    );
    let foreign_set = signed_row(&writer, [0x99; 32], 6, "c.mp4", blob.manifest_hash);

    let report = engine
        .ingest_peer_share_rows(
            &[unsigned_relay, fabricated, foreign_set],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest");
    assert_eq!(report.refused, 3, "report: {report:?}");
    assert_eq!(report.overlaid, 0, "a refused row lands nowhere");
    assert!(engine.db().relayed_pending_changes(10).unwrap().is_empty());
    assert!(engine.db().relayed_changes_since(0, 10).unwrap().is_empty());
}

/// Every writer signs: the serving peer's own unsigned row is refused by the
/// same judge as the nest pull's — even from a cached writer.
#[tokio::test]
async fn an_own_unsigned_row_is_refused_at_the_engine_door() {
    let blob = sealed_blob(&holiday_bytes());
    let fetcher = FixtureFetcher::serving(&[&blob]);
    let row = unsigned(peer_row(
        4,
        "clips/holiday.mp4",
        blob.manifest_hash,
        "create",
    ));
    let watch = tempfile::tempdir().unwrap();
    let engine = bound_ingest_engine(
        watch.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
    );
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None)
        .unwrap();
    let report = engine
        .ingest_peer_share_rows(std::slice::from_ref(&row), &PEER_ACTOR_HEX, &fetcher)
        .await
        .expect("ingest");
    assert_eq!(
        (report.channel_proven, report.refused, report.overlaid),
        (0, 1, 0),
        "report: {report:?}"
    );
}

/// **The cut on the peer leg** (`writer-signed-change-records.md` ruling
/// (11)(c)/(i)): a member whose reader never read its roster — offline since
/// launch — judges a peer-served row over the share leg's cached roster, and
/// takes exactly the nest pull's verdict. The owner's predecessor is stored
/// beside the owner, never as a writer of its own, so a row it signed under
/// the live nonce the owner minted is refused (a plant: the nonce was minted
/// after it retired), one it signed under a retired nonce is history (never
/// folded), and a member's row under that retired nonce is current — a cut
/// touches nothing a member signed.
#[tokio::test]
async fn a_member_offline_judges_a_predecessors_peer_row_as_the_nest_pull_does() {
    use crate::peer_share_store::tests::{ScriptedRoster, member, roster_reply, succession_link};
    const RETIRED_NONCE: [u8; 32] = [0x3E; 32];
    let (owner, old_owner, writer) = (
        set_owner(),
        ActorKeypair::from_secret([0x1A; 32]),
        writer_member(),
    );
    let blob = sealed_blob(&holiday_bytes());
    let fetcher = FixtureFetcher::serving(&[&blob]);
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine_with_db(
        watch.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
    );
    // A member's binding: the owner is someone else, the live nonce is the
    // one the owner minted at the cut, the lineage holds the nonce it retired.
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(owner.actor_id().0),
        live_minted_by: Some(owner.actor_id().0),
        retired_set_nonces: vec![(RETIRED_NONCE, Some(old_owner.actor_id().0))],
        ..Default::default()
    });
    // The share leg's one roster read, while the nest was still reachable:
    // the owner's row carries the statement that proves its predecessor.
    let mut owner_row = member(&owner.actor_id().to_hex(), "owner", None);
    owner_row.succession_statements = vec![succession_link(&old_owner, &owner, &owner)];
    crate::peer_share_store::refresh_writer_roster_via(
        engine.db(),
        "photos",
        ScriptedRoster(Ok(roster_reply(vec![
            owner_row,
            member(&PEER_ACTOR_HEX, "member", Some("writer")),
            member(&writer.actor_id().to_hex(), "member", Some("writer")),
        ]))),
    )
    .await;

    let planted = engine
        .ingest_peer_share_rows(
            &[signed_row(
                &old_owner,
                SET_NONCE,
                4,
                "planted.mp4",
                blob.manifest_hash,
            )],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest");

    let history = engine
        .ingest_peer_share_rows(
            &[signed_row(
                &old_owner,
                RETIRED_NONCE,
                5,
                "history.mp4",
                blob.manifest_hash,
            )],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest");

    let members = engine
        .ingest_peer_share_rows(
            &[signed_row(
                &writer,
                RETIRED_NONCE,
                6,
                "members.mp4",
                blob.manifest_hash,
            )],
            &PEER_ACTOR_HEX,
            &fetcher,
        )
        .await
        .expect("ingest");
    // (refused, overlaid) per row, judged together so one failure shows all
    // three verdicts: the plant refused, the history row refused as history,
    // the member's row landed.
    assert_eq!(
        [
            (planted.refused, planted.overlaid),
            (history.refused, history.overlaid),
            (members.refused, members.overlaid),
        ],
        [(1, 0), (1, 0), (0, 1)],
        "planted: {planted:?}\nhistory: {history:?}\nmembers: {members:?}"
    );
    assert_eq!(members.materialized, 1, "members: {members:?}");
}

/// **The marker-less reader at the peer door** (`writer-signed-change-records.md`
/// ruling (11)(c)/(i)): a member's host that holds no marker — its binding
/// names no owner — judges a peer-served row offline over the cached roster,
/// whose owner row is the owner it reads on the nest pull. So the cut's arms
/// fire here too: the owner's predecessor under the live nonce the owner
/// minted is a plant and refused, under a retired nonce it is history, and a
/// member's row under that retired nonce is current. Before the cache stored
/// the owner marker the reader held no owner offline and all three landed.
#[tokio::test]
async fn a_markerless_member_offline_takes_its_owner_off_the_cached_roster() {
    use crate::peer_share_store::tests::{ScriptedRoster, member, roster_reply, succession_link};
    const RETIRED_NONCE: [u8; 32] = [0x3E; 32];
    let (owner, old_owner, writer) = (
        set_owner(),
        ActorKeypair::from_secret([0x1A; 32]),
        writer_member(),
    );
    let blob = sealed_blob(&holiday_bytes());
    let fetcher = FixtureFetcher::serving(&[&blob]);
    let watch = tempfile::tempdir().unwrap();
    let engine = ingest_engine_with_db(
        watch.path().to_path_buf(),
        SyncDb::open_in_memory().unwrap(),
    );
    // No owner: the host holds no marker.
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: None,
        live_minted_by: Some(owner.actor_id().0),
        retired_set_nonces: vec![(RETIRED_NONCE, Some(old_owner.actor_id().0))],
        ..Default::default()
    });
    let mut owner_row = member(&owner.actor_id().to_hex(), "owner", None);
    owner_row.succession_statements = vec![succession_link(&old_owner, &owner, &owner)];
    crate::peer_share_store::refresh_writer_roster_via(
        engine.db(),
        "photos",
        ScriptedRoster(Ok(roster_reply(vec![
            owner_row,
            member(&PEER_ACTOR_HEX, "member", Some("writer")),
            member(&writer.actor_id().to_hex(), "member", Some("writer")),
        ]))),
    )
    .await;
    assert_eq!(
        crate::peer_share_store::cached_writer_roster(engine.db()).owner,
        Some(owner.actor_id().0),
        "the cache stores the read's owner marker"
    );

    let mut reports = Vec::new();
    for (signer, nonce, seq, path) in [
        (&old_owner, SET_NONCE, 4, "planted.mp4"),
        (&old_owner, RETIRED_NONCE, 5, "history.mp4"),
        (&writer, RETIRED_NONCE, 6, "members.mp4"),
        (&owner, SET_NONCE, 7, "owners.mp4"),
    ] {
        let report = engine
            .ingest_peer_share_rows(
                &[signed_row(signer, nonce, seq, path, blob.manifest_hash)],
                &PEER_ACTOR_HEX,
                &fetcher,
            )
            .await
            .expect("ingest");
        reports.push((report.refused, report.overlaid));
    }
    assert_eq!(
        reports,
        [(1, 0), (1, 0), (0, 1), (0, 1)],
        "(refused, overlaid) for the plant, the history row, the member's row, the owner's row"
    );
}
