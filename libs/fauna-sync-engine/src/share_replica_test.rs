//! The share plane's **on-demand landing** end to end at tier_1
//! (`p2p-shared-set-build.md` § *Phone peers — design*, decisions 1 and 5): a
//! real engine over a real state DB whose root is an owned tree's kept root,
//! the sealed fixtures and scripted fetcher of the resident twin
//! (`peer_share_ingest_test`), and the tree's own operations. No nest
//! anywhere on the landing side; the nest's later row is folded through the
//! populate fold, the only fold an on-demand replica runs.

use fauna_core::data::ContentHash;

use crate::db::SyncState;
use crate::engine::{OnDemandLanding, SyncEngine};
use crate::peer_share_ingest_test::{
    FixtureFetcher, PEER_ACTOR_HEX, encoded, holiday_bytes, ingest_engine, peer_row, sealed_blob,
    spool_blobs,
};
use crate::provider_face::owned_tree::OwnedTree;
use crate::share_landing::STORAGE_FLOOR_BYTES;

/// An on-demand replica: the engine's root is the tree's kept root.
struct Replica {
    _dir: tempfile::TempDir,
    tree: OwnedTree,
    engine: SyncEngine,
}

fn replica() -> Replica {
    let dir = tempfile::tempdir().unwrap();
    let tree = OwnedTree::new(dir.path().join("kept"), dir.path().join("cache"));
    std::fs::create_dir_all(tree.kept_root()).unwrap();
    std::fs::create_dir_all(tree.cache_root()).unwrap();
    let engine = ingest_engine(tree.kept_root().to_path_buf());
    engine
        .db()
        .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None, &[])
        .unwrap();
    Replica {
        _dir: dir,
        tree,
        engine,
    }
}

/// The peer's row for `path` as it serves an OFFLINE-authored write: signed,
/// and not sequenced — the nest has never seen it.
fn pending_row(path: &str, manifest: ContentHash) -> fauna_protocol::peer_share::PeerShareChange {
    let mut row = peer_row(0, path, manifest, "create");
    row.sequenced = false;
    row
}

fn roomy() -> Option<u64> {
    None
}

impl Replica {
    async fn ingest(
        &self,
        rows: &[fauna_protocol::peer_share::PeerShareChange],
        fetcher: &FixtureFetcher,
        free_space: &dyn Fn() -> Option<u64>,
    ) -> crate::peer_share_store::PeerIngestReport {
        self.engine
            .ingest_peer_share_rows_on_demand(
                rows,
                &PEER_ACTOR_HEX,
                fetcher,
                &OnDemandLanding {
                    tree: &self.tree,
                    free_space,
                },
            )
            .await
            .expect("ingest")
    }

    fn kept(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.tree.kept_root().join(rel)).ok()
    }

    fn cache(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.tree.cache_root().join(rel)).ok()
    }

    fn state(&self, rel: &str) -> Option<SyncState> {
        self.engine.db().get_entry(rel).unwrap().map(|e| e.state)
    }
}

/// The landing policy at the door: the un-sequenced row's body lands in the
/// KEPT root, with no dehydration proof and marked as a peer's body; the
/// sequenced row lands no body at all and lists as a placeholder.
#[tokio::test]
async fn an_unsequenced_body_lands_in_the_kept_root_and_a_sequenced_row_lands_none() {
    let r = replica();
    let draft = holiday_bytes();
    let draft_blob = sealed_blob(&draft);
    let recorded_blob = sealed_blob(b"recorded while the nest was reachable");
    let fetcher = FixtureFetcher::serving(&[&draft_blob, &recorded_blob]);

    let report = r
        .ingest(
            &[
                peer_row(4, "recorded.txt", recorded_blob.manifest_hash, "create"),
                pending_row("cabin/draft.mp4", draft_blob.manifest_hash),
            ],
            &fetcher,
            &roomy,
        )
        .await;
    assert_eq!(report.refused, 0, "report: {report:?}");
    assert_eq!(report.overlaid, 2, "every accepted row is recorded");
    assert_eq!(report.materialized, 1, "only the wanted body: {report:?}");

    assert_eq!(r.kept("cabin/draft.mp4").as_deref(), Some(&draft[..]));
    assert_eq!(r.cache("cabin/draft.mp4"), None);
    assert_eq!(r.state("cabin/draft.mp4"), Some(SyncState::Synced));
    assert!(
        !r.engine.is_dehydration_safe("cabin/draft.mp4"),
        "the nest does not hold these bytes — the proof is withheld"
    );
    assert!(
        r.engine
            .holds_provisional_peer_body("cabin/draft.mp4")
            .unwrap(),
        "a peer-landed body is told apart from a write intent"
    );

    assert_eq!(
        r.kept("recorded.txt"),
        None,
        "a sequenced row lands no body"
    );
    assert_eq!(r.cache("recorded.txt"), None);
    assert_eq!(r.state("recorded.txt"), Some(SyncState::Placeholder));
    let entry = r.engine.db().get_entry("recorded.txt").unwrap().unwrap();
    assert_eq!(entry.manifest_hash, Some(recorded_blob.manifest_hash));
    assert!(
        !r.engine
            .db()
            .get_share_overlay("recorded.txt")
            .unwrap()
            .expect("the overlay row still lands")
            .materialized
    );
    assert!(
        !r.engine
            .holds_provisional_peer_body("recorded.txt")
            .unwrap(),
        "a placeholder holds no body"
    );
}

/// The judge's changed arm, at the door: a placeholder holds no bytes, so a
/// wanted body lands over it — where the resident judge would skip.
#[tokio::test]
async fn a_wanted_body_lands_over_a_placeholder() {
    let r = replica();
    let old = sealed_blob(b"the head the nest recorded last week");
    let draft = b"rewritten in the cabin".to_vec();
    let draft_blob = sealed_blob(&draft);
    r.engine
        .db()
        .upsert_entry(
            "notes.txt",
            None,
            None,
            Some(old.manifest_hash),
            SyncState::Placeholder,
            0,
            0,
            36,
            3,
            Some(fauna_core::blob_seal::SealStamp::Version(1)),
        )
        .unwrap();

    let report = r
        .ingest(
            &[pending_row("notes.txt", draft_blob.manifest_hash)],
            &FixtureFetcher::serving(&[&draft_blob]),
            &roomy,
        )
        .await;
    assert_eq!(report.materialized, 1, "report: {report:?}");
    assert_eq!(r.kept("notes.txt").as_deref(), Some(&draft[..]));
    let entry = r.engine.db().get_entry("notes.txt").unwrap().unwrap();
    assert_eq!(entry.state, SyncState::Synced);
    assert_eq!(entry.manifest_hash, Some(draft_blob.manifest_hash));
}

/// A body in the kept root is a local write intent — an edit the nest has
/// not recorded — and a peer's row never overwrites it. The body an EARLIER
/// peer row landed there is not one, and the peer's next write replaces it.
#[tokio::test]
async fn a_kept_root_write_intent_is_never_overwritten_but_a_peers_own_body_is() {
    let r = replica();
    std::fs::write(r.tree.kept_root().join("mine.txt"), b"my unrecorded edit").unwrap();
    let theirs = sealed_blob(b"the peer's version of the same path");
    let first = sealed_blob(b"the peer's first draft");
    let second_bytes = b"the peer's second draft".to_vec();
    let second = sealed_blob(&second_bytes);
    let fetcher = FixtureFetcher::serving(&[&theirs, &first, &second]);

    let report = r
        .ingest(
            &[
                pending_row("mine.txt", theirs.manifest_hash),
                pending_row("theirs.txt", first.manifest_hash),
            ],
            &fetcher,
            &roomy,
        )
        .await;
    assert_eq!(report.materialized, 1, "report: {report:?}");
    assert_eq!(
        report.skipped,
        [(
            "mine.txt".to_string(),
            "a kept-root body is a local write intent"
        )]
    );
    assert_eq!(
        r.kept("mine.txt").as_deref(),
        Some(&b"my unrecorded edit"[..])
    );

    let report = r
        .ingest(
            &[pending_row("theirs.txt", second.manifest_hash)],
            &fetcher,
            &roomy,
        )
        .await;
    assert_eq!(report.materialized, 1, "report: {report:?}");
    assert_eq!(r.kept("theirs.txt").as_deref(), Some(&second_bytes[..]));
}

/// The storage floor: a wanted body that would take the device's free space
/// under the floor is not landed — its row stays a placeholder and the report
/// says why — and a later pass with room lands it.
#[tokio::test]
async fn the_storage_floor_leaves_a_placeholder_and_a_later_pass_lands_the_body() {
    let r = replica();
    let draft = b"too big for a phone that is nearly full".to_vec();
    let blob = sealed_blob(&draft);
    let fetcher = FixtureFetcher::serving(&[&blob]);
    let rows = [pending_row("big.bin", blob.manifest_hash)];

    let full = || Some(STORAGE_FLOOR_BYTES);
    let report = r.ingest(&rows, &fetcher, &full).await;
    assert_eq!(report.materialized, 0, "report: {report:?}");
    assert!(report.storage_limited);
    assert_eq!(report.skipped, [("big.bin".to_string(), "storage floor")]);
    assert_eq!(r.kept("big.bin"), None);
    assert_eq!(
        r.state("big.bin"),
        Some(SyncState::Placeholder),
        "the row lists"
    );

    let report = r.ingest(&rows, &fetcher, &roomy).await;
    assert_eq!(report.materialized, 1, "report: {report:?}");
    assert!(!report.storage_limited);
    assert_eq!(r.kept("big.bin").as_deref(), Some(&draft[..]));
    assert_eq!(r.state("big.bin"), Some(SyncState::Synced));
}

/// **A pending row the nest has answered lands nothing — on an on-demand root
/// too.** A sibling yet to hear the nest still serves its pending row, and an
/// on-demand replica WANTS an un-sequenced body, so left alone the row's stale
/// bytes would land in the kept root: over a settled delete the file comes
/// back, over a newer head history is written over it. The resident arm's
/// rule holds here: the row is history, no overlay row names it and it is not
/// relayed onward.
#[tokio::test]
async fn an_on_demand_root_lands_no_pending_row_over_a_settled_delete() {
    let r = replica();
    let rel = "cabin/draft.mp4";
    let blob = sealed_blob(&holiday_bytes());
    // The nest's delete of exactly this head has applied here.
    r.engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(blob.manifest_hash),
            SyncState::Deleted,
            0,
            0,
            0,
            2,
            Some(fauna_core::blob_seal::SealStamp::Version(1)),
        )
        .unwrap();

    let report = r
        .ingest(
            &[pending_row(rel, blob.manifest_hash)],
            &FixtureFetcher::serving(&[&blob]),
            &roomy,
        )
        .await;
    assert_eq!(report.materialized, 0, "report: {report:?}");
    assert_eq!(
        report.skipped,
        [(rel.to_string(), "pending row the ledger already holds")]
    );
    assert_eq!(r.kept(rel), None, "a settled delete stays settled");
    assert_eq!(r.state(rel), Some(SyncState::Deleted));
    assert!(r.engine.db().get_share_overlay(rel).unwrap().is_none());
    assert!(r.engine.db().relayed_pending_changes(4).unwrap().is_empty());
}

/// The same rule's ledger half: the pending row's bytes are ones the ledger
/// holds at or below the path's frontier, so the nest has recorded them and a
/// newer head since — the placeholder for that head stays as it is.
#[tokio::test]
async fn an_on_demand_root_lands_no_pending_row_the_ledger_holds() {
    let r = replica();
    let rel = "notes.txt";
    let stale = b"the first draft, recorded at seq 4".to_vec();
    let stale_blob = sealed_blob(&stale);
    let head = sealed_blob(b"the head the nest recorded at seq 5");
    r.engine.causal().hold(rel, 4, &stale);
    r.engine.causal().advance_frontier(rel, 5);
    r.engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(head.manifest_hash),
            SyncState::Placeholder,
            0,
            0,
            35,
            2,
            Some(fauna_core::blob_seal::SealStamp::Version(1)),
        )
        .unwrap();

    let report = r
        .ingest(
            &[pending_row(rel, stale_blob.manifest_hash)],
            &FixtureFetcher::serving(&[&stale_blob]),
            &roomy,
        )
        .await;
    assert_eq!(report.materialized, 0, "report: {report:?}");
    assert_eq!(
        report.skipped,
        [(rel.to_string(), "pending row the ledger already holds")]
    );
    assert_eq!(r.kept(rel), None, "no history over the head");
    let entry = r.engine.db().get_entry(rel).unwrap().unwrap();
    assert_eq!(entry.state, SyncState::Placeholder);
    assert_eq!(entry.manifest_hash, Some(head.manifest_hash));
    assert!(r.engine.db().get_share_overlay(rel).unwrap().is_none());
}

/// The reconcile on the populate fold, CONFIRM half: the nest's row for the
/// path names the manifest the peer's body landed under — the proof is
/// stamped, the overlay retires, and the body is demoted from the kept root
/// to the cache root through the engine's own dehydration gate.
#[tokio::test]
async fn the_populate_fold_confirms_a_landed_body_and_it_is_demoted_to_the_cache_root() {
    let r = replica();
    let draft = holiday_bytes();
    let blob = sealed_blob(&draft);
    let rel = "cabin/draft.mp4";
    r.ingest(
        &[pending_row(rel, blob.manifest_hash)],
        &FixtureFetcher::serving(&[&blob]),
        &roomy,
    )
    .await;
    assert!(!r.engine.is_dehydration_safe(rel), "withheld pre-confirm");

    // The author reached the nest; the phone's next fold carries the row.
    let nest_row = peer_row(9, rel, blob.manifest_hash, "create").change;
    let fold = r.engine.fold_changes_for_test(&[nest_row]).expect("fold");
    assert_eq!(fold.overlay.confirmed, [rel]);
    assert!(fold.overlay.superseded.is_empty());
    assert!(fold.stale_hydrated.is_empty(), "same head — nothing stale");
    assert!(r.engine.db().get_share_overlay(rel).unwrap().is_none());
    assert!(
        r.engine.is_dehydration_safe(rel),
        "the nest provably holds these bytes now"
    );
    assert!(!r.engine.holds_provisional_peer_body(rel).unwrap());

    r.tree
        .settle_peer_bodies(&r.engine, &fold.overlay)
        .expect("settle");
    assert_eq!(r.kept(rel), None, "demoted out of the kept root");
    assert_eq!(r.cache(rel).as_deref(), Some(&draft[..]));
    assert_eq!(
        r.tree.lookup_body(&r.engine, rel).unwrap(),
        Some(r.tree.cache_root().join(rel)),
        "and still the file's body"
    );
}

/// The reconcile on the populate fold, SUPERSEDE half: the nest recorded
/// another head for the path. The landed body is dropped — never left in the
/// kept root to be uploaded as a conflict — and the nest's row stands.
#[tokio::test]
async fn the_populate_fold_supersedes_a_landed_body_and_the_nests_row_stands() {
    let r = replica();
    let blob = sealed_blob(&holiday_bytes());
    let rel = "cabin/draft.mp4";
    r.ingest(
        &[pending_row(rel, blob.manifest_hash)],
        &FixtureFetcher::serving(&[&blob]),
        &roomy,
    )
    .await;

    let winner = sealed_blob(b"the re-cut everyone preferred");
    let nest_row = peer_row(9, rel, winner.manifest_hash, "create").change;
    let fold = r.engine.fold_changes_for_test(&[nest_row]).expect("fold");
    assert!(fold.overlay.confirmed.is_empty());
    assert_eq!(fold.overlay.superseded.len(), 1);
    assert_eq!(fold.overlay.superseded[0].path, rel);
    assert_eq!(fold.stale_hydrated.len(), 1, "the head moved under the row");

    r.tree
        .settle_peer_bodies(&r.engine, &fold.overlay)
        .expect("settle");
    r.engine
        .apply_refresh_fold(&fold, None)
        .await
        .expect("apply");
    assert_eq!(r.kept(rel), None, "the landed body yielded");
    let entry = r.engine.db().get_entry(rel).unwrap().unwrap();
    assert_eq!(entry.state, SyncState::Placeholder);
    assert_eq!(entry.manifest_hash, Some(winner.manifest_hash));
    assert!(
        r.engine.db().own_changes_since(0, 10).unwrap().is_empty(),
        "and it was never recorded as this replica's own change"
    );
}

/// The door itself ([`OwnedTree::share_ingest`]) over a spool, as the host's
/// worker runs it: a pull cut before the body crossed leaves the row a
/// placeholder; the next pass lands it; an un-sequenced row never moves the
/// cursor, and a sequenced one does although it lands no body.
#[tokio::test]
async fn the_door_resumes_an_interrupted_pull_and_keeps_the_cursor_rule() {
    let r = replica();
    let draft = b"authored in the cabin, with the nest unreachable".to_vec();
    let blob = sealed_blob(&draft);
    let recorded = sealed_blob(b"recorded while the nest was reachable");
    let rows = encoded(&[
        peer_row(6, "recorded.txt", recorded.manifest_hash, "create"),
        pending_row("cabin-draft.txt", blob.manifest_hash),
    ]);

    // Pass 1: the connection dropped before the draft's bodies crossed.
    let empty_spool = tempfile::tempdir().unwrap();
    let (report, cursor) = r
        .tree
        .share_ingest(
            &r.engine,
            &PEER_ACTOR_HEX,
            &rows,
            empty_spool.path().to_path_buf(),
        )
        .await
        .expect("pass 1");
    assert_eq!(report.materialized, 0, "report: {report:?}");
    assert_eq!(
        cursor, 6,
        "the sequenced row landed (no body is owed for it)"
    );
    assert_eq!(r.state("cabin-draft.txt"), Some(SyncState::Placeholder));

    // Pass 2: the pump re-plans the placeholder's body and spools it.
    let spool = tempfile::tempdir().unwrap();
    spool_blobs(spool.path(), &[&blob]);
    let (report, cursor) = r
        .tree
        .share_ingest(
            &r.engine,
            &PEER_ACTOR_HEX,
            &rows[1..],
            spool.path().to_path_buf(),
        )
        .await
        .expect("pass 2");
    assert_eq!(report.materialized, 1, "report: {report:?}");
    assert_eq!(cursor, 6, "an un-sequenced row moves no cursor");
    assert_eq!(r.kept("cabin-draft.txt").as_deref(), Some(&draft[..]));
}

/// iOS's replica ([`StagingTree`]): the File Provider extension's staging
/// root is the kept root, and its OS owns the cache
/// (`p2p-shared-set-build.md` § *Phone peers — design*, decision 3).
mod staging {
    use super::*;
    use crate::always_resident::share_ingest_on_demand;
    use crate::peer_share_ingest_test::Blob;
    use crate::provider_face::owned_tree::StagingTree;

    struct Staged {
        _dir: tempfile::TempDir,
        tree: StagingTree,
        engine: SyncEngine,
    }

    fn staged() -> Staged {
        let dir = tempfile::tempdir().unwrap();
        let tree = StagingTree::new(dir.path().join("staging"));
        std::fs::create_dir_all(tree.staging_root()).unwrap();
        let engine = ingest_engine(tree.staging_root().to_path_buf());
        engine
            .db()
            .cache_share_writer_roster(&[(PEER_ACTOR_HEX.to_string(), true)], &[], None, &[])
            .unwrap();
        Staged {
            _dir: dir,
            tree,
            engine,
        }
    }

    impl Staged {
        async fn door(
            &self,
            rows: &[fauna_protocol::peer_share::PeerShareChange],
            blobs: &[&Blob],
        ) {
            let spool = tempfile::tempdir().unwrap();
            spool_blobs(spool.path(), blobs);
            share_ingest_on_demand(
                &self.engine,
                &self.tree,
                &PEER_ACTOR_HEX,
                &encoded(rows),
                spool.path().to_path_buf(),
            )
            .await
            .expect("door");
        }

        fn staged(&self, rel: &str) -> Option<Vec<u8>> {
            std::fs::read(self.tree.staging_root().join(rel)).ok()
        }

        fn state(&self, rel: &str) -> Option<SyncState> {
            self.engine.db().get_entry(rel).unwrap().map(|e| e.state)
        }
    }

    /// The door over the staging root: the un-sequenced body lands there,
    /// `fetchContents`' kept-root lookup answers with it, and the engine
    /// never uploads it as this device's write; a sequenced row lands none.
    #[tokio::test]
    async fn the_door_lands_the_unsequenced_body_in_staging_and_the_kept_lookup_serves_it() {
        let s = staged();
        let draft = holiday_bytes();
        let blob = sealed_blob(&draft);
        let recorded = sealed_blob(b"recorded while the nest was reachable");
        s.door(
            &[
                peer_row(4, "recorded.txt", recorded.manifest_hash, "create"),
                pending_row("cabin/draft.mp4", blob.manifest_hash),
            ],
            &[&blob, &recorded],
        )
        .await;

        assert_eq!(s.staged("cabin/draft.mp4").as_deref(), Some(&draft[..]));
        assert_eq!(
            s.tree.kept_body(&s.engine, "cabin/draft.mp4").unwrap(),
            Some(s.tree.staging_root().join("cabin/draft.mp4")),
            "fetchContents answers from the kept root before the nest"
        );
        assert!(
            s.engine
                .holds_provisional_peer_body("cabin/draft.mp4")
                .unwrap(),
            "the engine's upload door refuses it as a write intent"
        );
        assert_eq!(
            s.staged("recorded.txt"),
            None,
            "a sequenced row lands no body"
        );
        assert_eq!(s.state("recorded.txt"), Some(SyncState::Placeholder));
        assert_eq!(s.tree.kept_body(&s.engine, "recorded.txt").unwrap(), None);
    }

    /// A staged copy that is neither a peer's unconfirmed body nor the
    /// recorded head is never handed to the OS as the file.
    #[tokio::test]
    async fn the_kept_lookup_never_serves_a_staged_copy_the_head_moved_past() {
        let s = staged();
        let recorded = sealed_blob(b"the head the nest recorded");
        s.door(
            &[peer_row(4, "notes.txt", recorded.manifest_hash, "create")],
            &[&recorded],
        )
        .await;
        std::fs::write(
            s.tree.staging_root().join("notes.txt"),
            b"an older staged copy",
        )
        .unwrap();
        assert_eq!(s.tree.kept_body(&s.engine, "notes.txt").unwrap(), None);
    }

    /// Confirm: the nest recorded exactly the landed bytes — demotion is
    /// deletion (the row a placeholder, the staged copy gone); the OS keeps
    /// its own copy or fetches the nest's.
    #[tokio::test]
    async fn a_confirmed_body_is_demoted_by_deleting_it_from_staging() {
        let s = staged();
        let blob = sealed_blob(&holiday_bytes());
        let rel = "cabin/draft.mp4";
        s.door(&[pending_row(rel, blob.manifest_hash)], &[&blob])
            .await;

        let nest_row = peer_row(9, rel, blob.manifest_hash, "create").change;
        let fold = s.engine.fold_changes_for_test(&[nest_row]).expect("fold");
        assert_eq!(fold.overlay.confirmed, [rel]);
        s.tree
            .settle_peer_bodies(&s.engine, &fold.overlay)
            .expect("settle");
        assert_eq!(s.staged(rel), None, "deleted from staging");
        assert_eq!(s.state(rel), Some(SyncState::Placeholder));
        assert_eq!(s.tree.kept_body(&s.engine, rel).unwrap(), None);
    }

    /// Supersede: the nest recorded another head — the landed body is
    /// dropped, and never recorded as this replica's change.
    #[tokio::test]
    async fn a_superseded_body_is_dropped_from_staging() {
        let s = staged();
        let blob = sealed_blob(&holiday_bytes());
        let rel = "cabin/draft.mp4";
        s.door(&[pending_row(rel, blob.manifest_hash)], &[&blob])
            .await;

        let winner = sealed_blob(b"the re-cut everyone preferred");
        let nest_row = peer_row(9, rel, winner.manifest_hash, "create").change;
        let fold = s.engine.fold_changes_for_test(&[nest_row]).expect("fold");
        assert_eq!(fold.overlay.superseded.len(), 1);
        s.tree
            .settle_peer_bodies(&s.engine, &fold.overlay)
            .expect("settle");
        assert_eq!(s.staged(rel), None, "the landed body yielded");
        assert!(s.engine.db().own_changes_since(0, 10).unwrap().is_empty());
    }
}
