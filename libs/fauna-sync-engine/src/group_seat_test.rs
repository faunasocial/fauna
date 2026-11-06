//! The group set's engine seat (`p2p-shared-set-build.md` § Build design — the
//! group set, decision 7): a `SyncEngine` keyed by the scope's generation ring
//! whose nest leg is **absent**.
//!
//! Two seats of one scope, no nest: a write at one is sealed under the ring's
//! tip, minted as an own-pending row stamped with the tip's generation id, its
//! manifest retained and its chunks indexed — then it settles, recording
//! nothing; the other seat ingests it as the fold hands it over
//! (`PeerPageOrigin::Group`), converged: no overlay row, the body landed and
//! stamped as recorded. Every nest a seat holds a client for is a tripwire
//! listener that fails the test if anything dials it.
//!
//! Tier 1 (`architecture/testing.md` § The four-tier taxonomy): in-process,
//! no nest, no driver.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fauna_core::blob_seal::SealStamp;
use fauna_core::crypto::GenerationKey;
use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderRef;
use fauna_core::group_content::GroupContentKeys;
use fauna_protocol::peer_row::PeerChangeRow;
use fauna_protocol::sync::SyncChange;

use crate::always_resident::ingest_peer_page_landing;
use crate::db::{OwnChangeRow, SyncState};
use crate::engine::SyncEngine;
use crate::peer_share_store::PeerPageOrigin;

const SCOPE: [u8; 32] = [0x5c; 32];
const TIP: [u8; 32] = [0x7a; 32];
const OLD: [u8; 32] = [0x7b; 32];

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

/// The scope's ring, as `group_set_keys` would answer it: two generations,
/// `TIP` the tip.
pub(crate) fn ring() -> GroupContentKeys {
    let tip_key = GenerationKey::from_bytes([0x11; 32]);
    let old_key = GenerationKey::from_bytes([0x22; 32]);
    GroupContentKeys::from_generation_keys(Some(TIP), [(&TIP, &tip_key), (&OLD, &old_key)])
}

/// One member's seat: the ring, the scope's ref, no nest leg.
fn group_seat(nest: &str, watch: &std::path::Path) -> SyncEngine {
    let mut engine = crate::download_file_bytes_test::build_test_engine(
        nest,
        watch.to_path_buf(),
        None,
        None,
        None,
        None,
    )
    .with_folder_ref(FolderRef::Group(SCOPE))
    .with_nest_leg_absent();
    engine.set_group_keys(Some(ring()));
    engine
}

/// The one pending own row for `path`.
fn pending_row(engine: &SyncEngine, path: &str) -> OwnChangeRow {
    let rows: Vec<_> = engine
        .db()
        .own_pending_changes(100)
        .unwrap()
        .into_iter()
        .filter(|r| r.path == path)
        .collect();
    assert_eq!(rows.len(), 1, "one pending row per path: {rows:?}");
    rows.into_iter().next().unwrap()
}

/// An in-memory blob store: what a member's serve answered for one body.
#[derive(Default)]
struct Served {
    manifests: HashMap<ContentHash, Vec<u8>>,
    chunks: HashMap<ContentHash, Vec<u8>>,
    /// Store keys asked for and not held — what the serve is asked next.
    missed: std::sync::Mutex<Vec<ContentHash>>,
}

#[async_trait::async_trait]
impl fauna_core::file_download::BlobFetcher for Served {
    async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
        self.manifests
            .get(hash)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no such manifest"))
    }
    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        _relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        for key in store_keys {
            match self.chunks.get(key) {
                Some(body) => out.push(body.clone()),
                None => {
                    self.missed.lock().unwrap().push(*key);
                    anyhow::bail!("no such chunk");
                }
            }
        }
        Ok(out)
    }
}

/// What `seat`'s own serve answers for `manifest` — its retained manifest and
/// the chunks its index re-derives — collected the way a puller asks: the
/// manifest, then each store key the open names.
async fn serve_of(seat: &SyncEngine, reader: &SyncEngine, manifest: ContentHash) -> Served {
    let mut served = Served::default();
    let bytes = seat
        .share_manifest_bytes(&manifest.digest())
        .await
        .unwrap()
        .expect("the write's manifest is retained");
    served.manifests.insert(manifest, bytes);
    // Ask the reader's open which store keys the manifest names; each miss is
    // the next chunk to ask the seat's serve for.
    loop {
        let attempt = fauna_core::file_download::download_file_bytes_by_manifest(
            &served,
            &reader.download_keys(),
            manifest,
            Some(SealStamp::Generation(TIP)),
            "probe",
        )
        .await;
        let missed: Vec<_> = served.missed.lock().unwrap().drain(..).collect();
        if attempt.is_ok() || missed.is_empty() {
            return served;
        }
        for key in missed {
            let body = seat
                .share_chunk_body(&key.digest())
                .await
                .unwrap()
                .expect("the write's chunks are indexed and served");
            served.chunks.insert(key, body);
        }
    }
}

/// The fold's row for a pending own row — the shape the share plane hands the
/// door after its walk merged the author's `fauna.group.file` entry.
fn folded(row: &OwnChangeRow) -> PeerChangeRow {
    PeerChangeRow {
        change: SyncChange {
            path: Some(row.path.clone()),
            path_hash: row.path_hash.clone(),
            manifest_hash: row.manifest_hash.clone(),
            size_bytes: row.size_bytes,
            change_type: row.change_type.clone(),
            created_at: row.created_at,
            content_generation: row.content_stamp.and_then(|s| match s {
                SealStamp::Generation(g) => Some(g),
                SealStamp::Version(_) => None,
            }),
            author_actor_id: Some(row.author_actor_id.clone()),
            device_id: Some(row.device_id.clone()),
            ..Default::default()
        },
        sequenced: false,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_group_seat_mints_its_write_and_records_nothing_on_any_nest() {
    let (nest, dialled) = nest_tripwire().await;
    let watch = tempfile::tempdir().unwrap();
    let seat = group_seat(&nest, watch.path());

    std::fs::write(watch.path().join("a.txt"), b"a group set's file").unwrap();
    let outcome = seat.upload_file("a.txt").await.unwrap();
    assert!(!outcome.recorded, "a group seat records nothing");

    // The pending own row, stamped with the tip's generation id and carrying
    // no path label (the path rides sealed in the plane entry, decision 2).
    let row = pending_row(&seat, "a.txt");
    assert_eq!(row.change_type, "create");
    assert_eq!(row.content_stamp, Some(SealStamp::Generation(TIP)));
    assert!(row.path_sealed.is_none());
    let manifest_hex = row
        .manifest_hash
        .clone()
        .expect("a create names its manifest");
    assert!(
        seat.db()
            .retained_manifest(&manifest_hex)
            .unwrap()
            .is_some(),
        "the manifest is retained for the serve"
    );

    // Settled, not re-driven: the row is `Synced` at the new bytes, and a
    // converge pass re-mints nothing.
    let entry = seat.db().get_entry("a.txt").unwrap().unwrap();
    assert_eq!(entry.state, SyncState::Synced);
    seat.reconcile().await.unwrap();
    seat.upload_pending(4).await.unwrap();
    assert_eq!(
        pending_row(&seat, "a.txt").manifest_hash,
        Some(manifest_hex)
    );

    // Every drive-level door answers without a call.
    assert_eq!(seat.pull_remote_changes().await.unwrap(), 0);
    seat.refresh_sync_mode().await;
    assert!(
        seat.applies_remote_deletes(),
        "a group seat syncs both ways"
    );
    seat.refresh_share_writer_roster().await;
    seat.refresh_seal_floor().await;
    assert!(!seat.stamp_sealed_set_name().await.unwrap());
    assert!(
        seat.open_lease_window().await.may_write(),
        "nothing governs a group seat"
    );

    // A delete is a pending own row too, replacing the create's.
    std::fs::remove_file(watch.path().join("a.txt")).unwrap();
    seat.handle_delete("a.txt").await.unwrap();
    let row = pending_row(&seat, "a.txt");
    assert_eq!(row.change_type, "delete");
    assert!(row.manifest_hash.is_none());

    // Let any stray dial land before the verdict.
    tokio::task::yield_now().await;
    assert!(
        !dialled.load(Ordering::SeqCst),
        "a group seat dialled a nest"
    );
}

#[tokio::test]
async fn a_write_at_one_seat_lands_converged_at_the_other() {
    let (nest, dialled) = nest_tripwire().await;
    let (watch_a, watch_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = group_seat(&nest, watch_a.path());
    let b = group_seat(&nest, watch_b.path());

    std::fs::write(watch_a.path().join("doc.txt"), b"written at A").unwrap();
    a.upload_file("doc.txt").await.unwrap();
    let row = pending_row(&a, "doc.txt");
    let manifest = ContentHash::from_digest_raw(
        fauna_core::hex32::decode(row.manifest_hash.as_deref().unwrap()).unwrap(),
    );
    let served = serve_of(&a, &b, manifest).await;

    let origin = PeerPageOrigin::Group { scope: SCOPE };
    let (report, cursor) = ingest_peer_page_landing(&b, &origin, vec![folded(&row)], &served, None)
        .await
        .unwrap();
    assert_eq!(report.materialized, 1, "{report:?}");
    assert_eq!(report.overlaid, 0, "a converged row writes no overlay");
    assert_eq!(cursor, 0, "a group origin keeps no pull cursor");
    assert_eq!(
        std::fs::read(watch_b.path().join("doc.txt")).unwrap(),
        b"written at A"
    );
    assert!(b.db().get_share_overlay("doc.txt").unwrap().is_none());
    let entry = b.db().get_entry("doc.txt").unwrap().unwrap();
    assert_eq!(
        entry.recorded_content_hash, entry.local_hash,
        "the landed body IS the head"
    );

    // The watcher's echo of that landing is no local edit: nothing to mint.
    b.upload_file("doc.txt").await.unwrap();
    assert!(b.db().own_pending_changes(100).unwrap().is_empty());

    // A's delete, folded, leaves B's disk — converged, not view-only.
    std::fs::remove_file(watch_a.path().join("doc.txt")).unwrap();
    a.handle_delete("doc.txt").await.unwrap();
    let delete = pending_row(&a, "doc.txt");
    let (report, _) = ingest_peer_page_landing(&b, &origin, vec![folded(&delete)], &served, None)
        .await
        .unwrap();
    assert_eq!(report.deleted, 1, "{report:?}");
    assert!(!watch_b.path().join("doc.txt").exists());

    tokio::task::yield_now().await;
    assert!(
        !dialled.load(Ordering::SeqCst),
        "a group seat dialled a nest"
    );
}

/// The agent's door (`EngineCommand::ShareIngest`, `ingest_share_page`) takes
/// the family from the engine: a page handed to a group set's engine lands
/// converged whatever actor the caller named.
#[tokio::test]
async fn the_share_ingest_door_lands_a_group_engines_page_converged() {
    let (nest, _dialled) = nest_tripwire().await;
    let (watch_a, watch_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = group_seat(&nest, watch_a.path());
    let b = group_seat(&nest, watch_b.path());
    std::fs::write(watch_a.path().join("s.txt"), b"through the door").unwrap();
    a.upload_file("s.txt").await.unwrap();
    let row = pending_row(&a, "s.txt");
    let manifest = ContentHash::from_digest_raw(
        fauna_core::hex32::decode(row.manifest_hash.as_deref().unwrap()).unwrap(),
    );
    let served = serve_of(&a, &b, manifest).await;

    // The spool the share plane fills, in the spool fetcher's own layout.
    let spool = tempfile::tempdir().unwrap();
    for (hash, bytes) in &served.manifests {
        let path = crate::peer_share_store::spool_manifest_path(spool.path(), hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    for (key, body) in &served.chunks {
        let path = crate::peer_share_store::spool_chunk_path(spool.path(), key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let encoded = fauna_core::encoding::canonical_encode(&folded(&row)).unwrap();
    let (report, _) = crate::always_resident::ingest_share_page(
        &b,
        &"ab".repeat(32),
        &[encoded],
        spool.path().to_path_buf(),
    )
    .await
    .unwrap();
    assert_eq!(report.materialized, 1, "{report:?}");
    assert_eq!(report.overlaid, 0);
    assert!(b.db().get_share_overlay("s.txt").unwrap().is_none());
}

/// A row the fold could not attribute — no author — is refused, never landed
/// as this account's.
#[tokio::test]
async fn a_group_row_naming_no_author_is_refused() {
    let (nest, _dialled) = nest_tripwire().await;
    let watch = tempfile::tempdir().unwrap();
    let b = group_seat(&nest, watch.path());
    let row = PeerChangeRow {
        change: SyncChange {
            path: Some("x.txt".into()),
            manifest_hash: Some("00".repeat(32)),
            change_type: "create".into(),
            content_generation: Some(TIP),
            ..Default::default()
        },
        ..Default::default()
    };
    let (report, _) = ingest_peer_page_landing(
        &b,
        &PeerPageOrigin::Group { scope: SCOPE },
        vec![row],
        &Served::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(report.refused, 1, "{report:?}");
    assert!(!watch.path().join("x.txt").exists());
}

/// The converged landing's on-demand arm (`group-set.md` § Build design — the
/// group set, decision 11 (b)): a member whose seat is an on-demand replica —
/// the engine over the kept root, the owned tree its one writer.
mod on_demand {
    use super::*;
    use crate::engine::OnDemandLanding;
    use crate::provider_face::owned_tree::OwnedTree;

    struct Replica {
        _dir: tempfile::TempDir,
        engine: SyncEngine,
        tree: OwnedTree,
    }

    impl Replica {
        fn new(nest: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (kept, cache) = (dir.path().join("kept"), dir.path().join("cache"));
            std::fs::create_dir_all(&kept).unwrap();
            std::fs::create_dir_all(&cache).unwrap();
            Replica {
                engine: group_seat(nest, &kept),
                tree: OwnedTree::new(kept, cache),
                _dir: dir,
            }
        }

        fn kept(&self, rel: &str) -> Option<Vec<u8>> {
            std::fs::read(self.tree.kept_root().join(rel)).ok()
        }

        fn cached(&self, rel: &str) -> bool {
            self.tree.cache_root().join(rel).exists()
        }

        /// One folded page through the on-demand landing, `free` the device's
        /// free space.
        async fn fold(
            &self,
            rows: Vec<PeerChangeRow>,
            served: &Served,
            free: Option<u64>,
        ) -> crate::peer_share_store::PeerIngestReport {
            let landing = OnDemandLanding {
                tree: &self.tree,
                free_space: &move || free,
            };
            let (report, cursor) = ingest_peer_page_landing(
                &self.engine,
                &PeerPageOrigin::Group { scope: SCOPE },
                rows,
                served,
                Some(&landing),
            )
            .await
            .unwrap();
            assert_eq!(cursor, 0, "a group origin keeps no pull cursor");
            report
        }
    }

    /// `seat`'s write of `path`: its pending row, and what its serve answers.
    async fn written(
        seat: &SyncEngine,
        watch: &std::path::Path,
        path: &str,
        bytes: &[u8],
        reader: &SyncEngine,
    ) -> (OwnChangeRow, Served) {
        std::fs::write(watch.join(path), bytes).unwrap();
        seat.upload_file(path).await.unwrap();
        let row = pending_row(seat, path);
        let manifest = ContentHash::from_digest_raw(
            fauna_core::hex32::decode(row.manifest_hash.as_deref().unwrap()).unwrap(),
        );
        let served = serve_of(seat, reader, manifest).await;
        (row, served)
    }

    #[tokio::test]
    async fn a_group_row_lands_in_the_kept_root_and_nothing_frees_it() {
        let (nest, dialled) = nest_tripwire().await;
        let watch_a = tempfile::tempdir().unwrap();
        let a = group_seat(&nest, watch_a.path());
        let b = Replica::new(&nest);

        let (row, served) =
            written(&a, watch_a.path(), "doc.txt", b"written at A", &b.engine).await;
        let report = b.fold(vec![folded(&row)], &served, None).await;
        assert_eq!(report.materialized, 1, "{report:?}");
        assert_eq!(report.overlaid, 0, "a converged row writes no overlay");
        assert_eq!(b.kept("doc.txt").as_deref(), Some(&b"written at A"[..]));
        assert!(
            b.engine
                .db()
                .get_share_overlay("doc.txt")
                .unwrap()
                .is_none()
        );
        let entry = b.engine.db().get_entry("doc.txt").unwrap().unwrap();
        assert_eq!(entry.state, SyncState::Synced);
        assert_eq!(
            entry.recorded_content_hash, entry.local_hash,
            "the landed body IS the head"
        );
        assert!(
            !b.engine.is_dehydration_safe("doc.txt"),
            "no nest can serve a group body back, so nothing may free it"
        );

        // The host's start sweep reads the landed head as recorded: nothing
        // is minted, and the body stays in the kept root.
        let sweep = b.tree.start_sweep(&b.engine).await.unwrap();
        assert!(sweep.pending.is_empty(), "{sweep:?}");
        assert!(b.engine.db().own_pending_changes(100).unwrap().is_empty());
        assert!(b.kept("doc.txt").is_some() && !b.cached("doc.txt"));

        // A's next edit lands over it: the kept body is the earlier landing,
        // not a write intent.
        let (row, served) = written(&a, watch_a.path(), "doc.txt", b"edited at A", &b.engine).await;
        let report = b.fold(vec![folded(&row)], &served, None).await;
        assert_eq!(report.materialized, 1, "{report:?}");
        assert_eq!(b.kept("doc.txt").as_deref(), Some(&b"edited at A"[..]));

        // A's delete leaves the kept root, converged.
        std::fs::remove_file(watch_a.path().join("doc.txt")).unwrap();
        a.handle_delete("doc.txt").await.unwrap();
        let report = b
            .fold(vec![folded(&pending_row(&a, "doc.txt"))], &served, None)
            .await;
        assert_eq!(report.deleted, 1, "{report:?}");
        assert!(b.kept("doc.txt").is_none());

        tokio::task::yield_now().await;
        assert!(
            !dialled.load(Ordering::SeqCst),
            "a group seat dialled a nest"
        );
    }

    /// Below the storage floor the row still lists — a placeholder — and the
    /// report says why; the next pass with room lands the body.
    #[tokio::test]
    async fn a_body_over_the_floor_stays_a_placeholder_until_there_is_room() {
        let (nest, _dialled) = nest_tripwire().await;
        let watch_a = tempfile::tempdir().unwrap();
        let a = group_seat(&nest, watch_a.path());
        let b = Replica::new(&nest);
        let (row, served) = written(
            &a,
            watch_a.path(),
            "big.bin",
            b"too big for the floor",
            &b.engine,
        )
        .await;

        let report = b.fold(vec![folded(&row)], &served, Some(0)).await;
        assert!(report.storage_limited, "{report:?}");
        assert_eq!(report.materialized, 0);
        assert!(b.kept("big.bin").is_none());
        let entry = b.engine.db().get_entry("big.bin").unwrap().unwrap();
        assert_eq!(entry.state, SyncState::Placeholder, "the file still lists");

        let report = b.fold(vec![folded(&row)], &served, None).await;
        assert_eq!(report.materialized, 1, "{report:?}");
        assert_eq!(
            b.kept("big.bin").as_deref(),
            Some(&b"too big for the floor"[..])
        );
    }

    /// A body in the kept root that is not the projection's head — a write
    /// the provider took and has not ingested — is never overwritten.
    #[tokio::test]
    async fn an_uningested_kept_body_is_a_write_intent_and_is_not_overwritten() {
        let (nest, _dialled) = nest_tripwire().await;
        let watch_a = tempfile::tempdir().unwrap();
        let a = group_seat(&nest, watch_a.path());
        let b = Replica::new(&nest);
        std::fs::write(b.tree.kept_root().join("n.txt"), b"typed at B").unwrap();
        let (row, served) = written(&a, watch_a.path(), "n.txt", b"from A", &b.engine).await;

        let report = b.fold(vec![folded(&row)], &served, None).await;
        assert_eq!(report.materialized, 0, "{report:?}");
        assert_eq!(b.kept("n.txt").as_deref(), Some(&b"typed at B"[..]));
    }
}

/// `build_engine`'s group arm: the ring read through the host's key reader
/// builds a seat with no nest leg — no registration, no folder list, no
/// custody load — and a ring that is absent or holds no tip builds nothing.
#[cfg(feature = "engine-lifecycle")]
mod build {
    use super::*;
    use crate::engine_lifecycle::{EngineCredential, EngineParams, build_engine};
    use fauna_core::identity::ActorKeypair;

    /// A host's key reader holding only `ring` for [`SCOPE`] — no folder
    /// custody at all, so a build that loaded it would fail.
    struct GroupRing(Option<GroupContentKeys>);

    #[async_trait::async_trait]
    impl fauna_client_folders::FolderKeyReader for GroupRing {
        async fn load(&self) -> anyhow::Result<fauna_core::data::FoldersConfig> {
            anyhow::bail!("a group seat's build reads no folder custody")
        }
        async fn group_set_keys(
            &self,
            scope_id: &[u8; 32],
        ) -> anyhow::Result<Option<GroupContentKeys>> {
            assert_eq!(*scope_id, SCOPE);
            Ok(self.0.clone())
        }
    }

    fn params(nest: &str, ring: Option<GroupContentKeys>, dir: &std::path::Path) -> EngineParams {
        let keypair = ActorKeypair::generate();
        let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
            nest.to_string(),
            ActorKeypair::from_secret(*keypair.secret_bytes()),
            Arc::new(fauna_nest_http::StaticBearer("test.bearer".to_string())),
            reqwest::Client::new(),
        ));
        let watch_dir = dir.join("watch");
        std::fs::create_dir_all(&watch_dir).unwrap();
        EngineParams {
            state_dir: dir.to_path_buf(),
            watch_dir,
            folder_ref: FolderRef::Group(SCOPE),
            device_id: [6u8; 32],
            // Set on purpose: a group arm registers nothing even when asked to.
            device_label: Some("a device".to_string()),
            auth,
            nest_rpc: fauna_client::NestClient::new(nest.to_string(), keypair),
            mls: None,
            credential: EngineCredential::BackupKey(fauna_core::crypto::BackupKey::from_bytes(
                [0x51; 32],
            )),
            progress_tx: None,
            predecessor_backup_keys: Vec::new(),
            predecessor_actor_ids: Vec::new(),
            learned_predecessors: Default::default(),
            access_gate: None,
            change_signer: None,
            folder_keys: Arc::new(GroupRing(ring)),
            reader_hosting: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_group_ref_with_its_ring_builds_a_seat_with_no_nest_leg() {
        let (nest, dialled) = nest_tripwire().await;
        let dir = tempfile::tempdir().unwrap();
        let built = build_engine(params(&nest, Some(ring()), dir.path()))
            .await
            .expect("a group set with a tipped ring builds");
        assert!(built.engine.nest_leg_absent());
        assert_eq!(built.engine.folder_row(), Some(FolderRef::Group(SCOPE)));
        assert_eq!(
            built.engine.seal_for_upload(b"x").unwrap().content_stamp,
            Some(SealStamp::Generation(TIP)),
            "the seat seals under the ring's tip"
        );
        tokio::task::yield_now().await;
        assert!(!dialled.load(Ordering::SeqCst), "the build dialled a nest");
    }

    #[tokio::test]
    async fn a_group_ref_with_no_ring_or_no_tip_builds_nothing() {
        let (nest, dialled) = nest_tripwire().await;
        let dir = tempfile::tempdir().unwrap();
        assert!(
            build_engine(params(&nest, None, dir.path()))
                .await
                .is_none(),
            "no group plane, no engine"
        );
        let key = GenerationKey::from_bytes([0x22; 32]);
        let tipless = GroupContentKeys::from_generation_keys(None, [(&OLD, &key)]);
        assert!(
            build_engine(params(&nest, Some(tipless), dir.path()))
                .await
                .is_none(),
            "no tip to seal under, no engine"
        );
        tokio::task::yield_now().await;
        assert!(
            !dialled.load(Ordering::SeqCst),
            "a refused build dialled a nest"
        );
    }
}
