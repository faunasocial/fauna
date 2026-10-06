//! The serve-side BYTE half at tier_1 (slice E —
//! [`SyncEngine::share_manifest_bytes`] / [`SyncEngine::share_chunk_body`];
//! `p2p.md` § Cross-user shared-set transfer): a real engine over a real
//! state DB, the retained-manifest read, and the range-read + per-chunk
//! re-derivation proven **byte-identical** to the upload seal's artifacts.
//! No nest anywhere: offline serveability is the point.

use fauna_core::data::ContentHash;

use crate::db::{SyncDb, SyncState};
use crate::engine::SyncEngine;
use crate::peer_share_ingest_test::{
    CONTENT_KEY, ingest_engine_with_db, serving_peer, sign_as_serving_peer,
};

/// Well past the 8 MiB single-chunk threshold, with varying content so
/// FastCDC cuts real content-defined boundaries — a single-chunk fixture
/// would leave the range arithmetic untested (the sub-budget-fixture trap the
/// serve/pull build record names).
fn multi_chunk_bytes() -> Vec<u8> {
    (0..20 * 1024 * 1024u64)
        .map(|i| ((i.wrapping_mul(31) ^ (i / 251)) % 251) as u8)
        .collect()
}

struct ServeRig {
    _dir: tempfile::TempDir,
    engine: SyncEngine,
    sealed: crate::seal::SealedBlob,
}

/// A bound engine whose watch dir holds `holiday.mp4`, with the file's sealed
/// manifest retained in the state DB exactly as the upload-site epilogue
/// writes it. `retain` / `entry` select which serve source the test exercises.
fn serve_rig(bytes: &[u8], retain_manifest: bool, seed_entry: bool) -> ServeRig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("holiday.mp4"), bytes).unwrap();

    let sealed = crate::seal::seal_blob(bytes, Some((CONTENT_KEY, Some(1)))).unwrap();
    let db = SyncDb::open_in_memory().unwrap();
    if retain_manifest {
        db.retain_manifest(
            &hex::encode(sealed.manifest_hash.digest()),
            &sealed.manifest_bytes,
            "holiday.mp4",
            Some(1),
        )
        .unwrap();
    }
    if seed_entry {
        db.upsert_entry(
            "holiday.mp4",
            Some(ContentHash::of_raw(bytes)),
            None,
            Some(sealed.manifest_hash),
            SyncState::Synced,
            0,
            0,
            bytes.len() as i64,
            1,
            Some(1),
        )
        .unwrap();
    }
    let engine = ingest_engine_with_db(dir.path().to_path_buf(), db);
    ServeRig {
        _dir: dir,
        engine,
        sealed,
    }
}

/// The round trip the whole byte half exists for: the retained manifest
/// serves verbatim, and every chunk re-derives from a plaintext RANGE read
/// into exactly the body the upload sealed — same store key, same bytes — so
/// a puller's per-body verification passes against this replica's serve.
#[tokio::test]
async fn a_retained_manifest_serves_and_every_chunk_rederives_byte_identically() {
    let bytes = multi_chunk_bytes();
    let rig = serve_rig(&bytes, true, false);
    assert!(
        rig.sealed.manifest.chunk_hashes.len() >= 2,
        "fixture must be multi-chunk to test the range arithmetic (got {})",
        rig.sealed.manifest.chunk_hashes.len()
    );

    let served = rig
        .engine
        .share_manifest_bytes(&rig.sealed.manifest_hash.digest())
        .await
        .unwrap()
        .expect("retained manifest serves");
    assert_eq!(served, rig.sealed.manifest_bytes);

    for (store_key, body) in &rig.sealed.chunks {
        let got = rig
            .engine
            .share_chunk_body(&store_key.digest())
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("chunk {} serves", hex::encode(store_key.digest())));
        assert_eq!(&got, body, "re-derived body is byte-identical");
    }
}

/// Chunk serving is manifest-anchored: a store key asked before its manifest
/// answers `None` (the shared download walk always fetches the manifest
/// first, so an honest puller never hits this), and answers after it.
#[tokio::test]
async fn a_chunk_request_before_its_manifest_answers_none() {
    let bytes = multi_chunk_bytes();
    let rig = serve_rig(&bytes, true, false);
    let first_key = rig.sealed.chunks[0].0.digest();

    assert_eq!(
        rig.engine.share_chunk_body(&first_key).await.unwrap(),
        None,
        "no manifest served yet — nothing anchors the chunk"
    );
    rig.engine
        .share_manifest_bytes(&rig.sealed.manifest_hash.digest())
        .await
        .unwrap()
        .expect("manifest serves");
    assert!(
        rig.engine
            .share_chunk_body(&first_key)
            .await
            .unwrap()
            .is_some(),
        "anchored now"
    );
}

/// A file that drifted after its manifest was retained must refuse chunks —
/// the range read no longer hashes to the manifest's plaintext anchor, and
/// serving the drifted bytes would only fail the puller's verification with a
/// confusing mismatch at the far end.
#[tokio::test]
async fn a_drifted_file_refuses_chunks_rather_than_serving_wrong_bytes() {
    let bytes = multi_chunk_bytes();
    let rig = serve_rig(&bytes, true, false);
    rig.engine
        .share_manifest_bytes(&rig.sealed.manifest_hash.digest())
        .await
        .unwrap()
        .expect("manifest serves");

    // Same length, different content — every range read hash-mismatches.
    let drifted: Vec<u8> = bytes.iter().map(|b| b.wrapping_add(1)).collect();
    std::fs::write(rig._dir.path().join("holiday.mp4"), &drifted).unwrap();

    assert_eq!(
        rig.engine
            .share_chunk_body(&rig.sealed.chunks[0].0.digest())
            .await
            .unwrap(),
        None
    );
}

/// An unknown manifest is the ordinary multi-source "cannot produce it"
/// answer, never an error.
#[tokio::test]
async fn an_unknown_manifest_answers_none() {
    let rig = serve_rig(b"tiny", false, false);
    assert_eq!(
        rig.engine.share_manifest_bytes(&[0xAB; 32]).await.unwrap(),
        None
    );
}

/// The resident loop's ShareIngest arm end-to-end — the seam the agent's
/// pipe server drives on an out-of-process-agent app: an accepted page
/// (canonical-encoded rows) plus a caller-populated spool reaches the loop's
/// OWN engine, the row overlays, the bytes materialize from the spool, and
/// the per-peer pull cursor comes back advanced to the page's highest
/// sequenced seq.
#[tokio::test]
async fn the_resident_loop_ingests_a_spooled_share_page_and_advances_the_cursor() {
    let peer_hex = serving_peer().actor_id().to_hex();
    let dir = tempfile::tempdir().unwrap();
    let spool = tempfile::tempdir().unwrap();

    let bytes: Vec<u8> = (0..60_000u32).map(|i| (i % 233) as u8).collect();
    let sealed = crate::seal::seal_blob(&bytes, Some((CONTENT_KEY, Some(1)))).unwrap();
    std::fs::create_dir_all(spool.path().join("manifests")).unwrap();
    std::fs::create_dir_all(spool.path().join("chunks")).unwrap();
    std::fs::write(
        crate::peer_share_store::spool_manifest_path(spool.path(), &sealed.manifest_hash),
        &sealed.manifest_bytes,
    )
    .unwrap();
    for (key, body) in &sealed.chunks {
        std::fs::write(
            crate::peer_share_store::spool_chunk_path(spool.path(), key),
            body,
        )
        .unwrap();
    }

    // The peer must be a cached WRITER for its rows to be accepted (the
    // fail-closed consult) — seeded before the engine takes the DB.
    let db = SyncDb::open_in_memory().unwrap();
    db.cache_share_writer_roster(&[(peer_hex.clone(), true)], &[], None)
        .unwrap();
    let engine = ingest_engine_with_db(dir.path().to_path_buf(), db);

    let row = fauna_protocol::peer_share::PeerShareChange {
        change: fauna_protocol::sync::SyncChange {
            seq: 41,
            // The signed hash of the plaintext path below — the judge checks
            // one against the other (ruling (2)), offline as on the nest pull.
            path_hash: hex::encode(fauna_core::sync::path_hash("from-peer.bin")),
            manifest_hash: Some(hex::encode(sealed.manifest_hash.digest())),
            size_bytes: bytes.len() as i64,
            change_type: "create".to_string(),
            created_at: 1_700_000_000_000,
            path: Some("from-peer.bin".to_string()),
            device_id: Some("d1".repeat(32)),
            content_key_version: Some(1),
            author_actor_id: Some(peer_hex.clone()),
            ..Default::default()
        },
        sequenced: true,
        ..Default::default()
    };
    let mut row = row;
    sign_as_serving_peer(&mut row.change);
    let encoded = fauna_core::encoding::canonical_encode(&row).unwrap();

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(2);
    // `SyncEngine` is `!Sync` (its `SyncDb` wraps a raw `rusqlite::Connection`);
    // this loop future is deliberately `!Send` and raced in-task, never spawned.
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = std::sync::Arc::new(engine);
    let loop_fut = crate::always_resident::run_watch_loop(
        engine,
        dir.path().to_path_buf(),
        "shared-set".to_string(),
        std::time::Duration::from_secs(3600),
        None,
        Some(cmd_rx),
    );
    tokio::pin!(loop_fut);

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(crate::always_resident::EngineCommand::ShareIngest {
            proven_actor_hex: peer_hex.clone(),
            rows: vec![encoded.to_vec()],
            spool_dir: spool.path().to_path_buf(),
            reply: reply_tx,
        })
        .await
        .expect("capacity-2 channel buffers the command before the loop runs");
    let (report, cursor) = tokio::select! {
        _ = &mut loop_fut => panic!("the watch loop must not exit"),
        got = tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx) => got
            .expect("the loop must answer the command well inside the ceiling")
            .expect("reply channel alive")
            .expect("ingest succeeds"),
    };
    assert_eq!(
        report.overlaid, 1,
        "the row landed in the overlay: {report:?}"
    );
    assert_eq!(report.materialized, 1, "the spooled bytes materialized");
    assert_eq!(cursor, 41, "cursor advanced to the page's sequenced seq");
    assert_eq!(
        std::fs::read(dir.path().join("from-peer.bin")).unwrap(),
        bytes,
        "the peer's file landed byte-identical"
    );
}

/// Content recorded before manifest retention landed re-derives from disk
/// under the RECORDED generation, serves, and backfills the retention — the
/// second fetch answers from the table even with the file gone.
#[tokio::test]
async fn a_pre_retention_head_rederives_from_disk_and_backfills() {
    let bytes = multi_chunk_bytes();
    let rig = serve_rig(&bytes, false, true);

    let served = rig
        .engine
        .share_manifest_bytes(&rig.sealed.manifest_hash.digest())
        .await
        .unwrap()
        .expect("legacy fallback re-derives from the entry's head");
    assert_eq!(served, rig.sealed.manifest_bytes);

    // The fallback retained what it derived: with the file gone, the second
    // fetch still answers (table hit), while a re-derivation would fail.
    std::fs::remove_file(rig._dir.path().join("holiday.mp4")).unwrap();
    assert!(
        rig.engine
            .share_manifest_bytes(&rig.sealed.manifest_hash.digest())
            .await
            .unwrap()
            .is_some(),
        "backfilled retention answers without the file"
    );
}
