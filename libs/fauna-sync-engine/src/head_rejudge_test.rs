//! **The roster re-read, the skip note and the head re-judge** —
//! `mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
//! (8)(e) and (8)(f): a row that verifies and whose signed actor resolves to
//! no writer on this reader, now, is a record the reader cannot attribute
//! *yet*. Before it is refused the roster is read once more; refused, it notes
//! a skip on its path; and when what the reader admits by gains something the
//! engine judges again what its cursor already passed — by heads, up to its
//! own cursor, whole before it folds anything, never by replay.
//!
//! Tier_1: the control plane is a nest double on a mocked socket
//! (`cross_nest_reader_roster_test`'s wiring) that serves the set's log in
//! pages and its writer roster; the byte plane is `test_support::MockNest`.

use std::sync::{Arc, Mutex};

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::file_download::PredecessorSealKey;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_core::path_crypto::LabelRoot;
use fauna_protocol::RpcError;
use fauna_protocol::folders::{ActorMembersListReply, FolderActorMember};
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::sync_row_verify::{ROSTER_NOT_SHARED, ReaderBinding};
use fauna_protocol::sync_writer_sig::{ChangeSigner, SignedChange};
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::cross_nest_reader_roster_test::{stand_up_ws_answering, value_of};
use crate::db::{SyncDb, SyncState};
use crate::download_file_bytes_test::{store_sealed_fixture, test_sync_client};
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::test_support::{BlobStore, MockNest};
use crate::transfer::TransferPool;

const FOLDER: &str = "holiday";
const SET_NONCE: [u8; 32] = [7; 32];
const OTHER_NONCE: [u8; 32] = [8; 32];
/// No byte plane: a test whose rows carry no bytes to fetch.
const NO_BLOB_PLANE: &str = "http://127.0.0.1:9";

/// This seat's account — the set's owner in every test here.
fn owner() -> ActorKeypair {
    ActorKeypair::from_secret([0x51; 32])
}

fn predecessor() -> ActorKeypair {
    ActorKeypair::from_secret([0x52; 32])
}

fn writer() -> ActorKeypair {
    ActorKeypair::from_secret([0x53; 32])
}

fn owner_key() -> BackupKey {
    BackupKey::from_bytes([0x61; 32])
}

fn predecessor_key() -> BackupKey {
    BackupKey::from_bytes([0x62; 32])
}

/// The nest double: the set's log (served `seq > since`, oldest first, at most
/// `page` rows a reply), its writer roster (`None` = an owner-only set,
/// `not_shared`), and what was asked of it.
#[derive(Default)]
struct NestDouble {
    log: Mutex<Vec<SyncChange>>,
    /// Rows per `changes.list` reply; 0 = the whole log in one.
    page: Mutex<usize>,
    roster: Mutex<Option<ActorMembersListReply>>,
    /// A roster read fails outright (the nest is unreachable for it).
    roster_fails: Mutex<bool>,
    /// The `changes.list` call (0-based, counted over the double's life) from
    /// which every such call fails.
    list_fails_from: Mutex<Option<usize>>,
    /// The `since` of every `changes.list` request, in order.
    list_since: Mutex<Vec<i64>>,
    roster_reads: Mutex<usize>,
    /// The `path` of every `changes.record` asked of the double (each one
    /// answered with an error: nothing lands).
    records: Mutex<Vec<String>>,
}

impl NestDouble {
    fn serving(log: Vec<SyncChange>) -> Arc<Self> {
        let nest = Self::default();
        *nest.log.lock().unwrap() = log;
        Arc::new(nest)
    }

    /// How many log reads started from the beginning — a head re-judge pass
    /// (the ordinary pull of a seat whose cursor is past 0 never asks for it).
    fn passes(&self) -> usize {
        self.list_since
            .lock()
            .unwrap()
            .iter()
            .filter(|since| **since == 0)
            .count()
    }

    fn roster_reads(&self) -> usize {
        *self.roster_reads.lock().unwrap()
    }

    fn set_roster(&self, writers: &[&ActorKeypair]) {
        let mut members = vec![member(&owner(), "owner", None)];
        members.extend(writers.iter().map(|w| member(w, "member", Some("writer"))));
        *self.roster.lock().unwrap() = Some(ActorMembersListReply {
            members,
            ..Default::default()
        });
    }

    fn answer(&self, kind: &str, payload: &fauna_protocol::Value) -> (bool, fauna_protocol::Value) {
        let error = |code: &str| (false, value_of(&RpcError::new(code, "error.test")));
        match kind {
            "fauna.sync.changes.list" => {
                let bytes = fauna_core::encoding::canonical_encode(payload).unwrap();
                let request: SyncChangesListRequest =
                    fauna_protocol::decode_strict(&bytes).expect("a well-formed request");
                let mut asked = self.list_since.lock().unwrap();
                let call = asked.len();
                asked.push(request.since);
                if self
                    .list_fails_from
                    .lock()
                    .unwrap()
                    .is_some_and(|from| call >= from)
                {
                    return error("fauna.test.unavailable");
                }
                let mut changes: Vec<SyncChange> = self
                    .log
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|c| c.seq > request.since)
                    .cloned()
                    .collect();
                changes.sort_by_key(|c| c.seq);
                let page = *self.page.lock().unwrap();
                if page > 0 {
                    changes.truncate(page);
                }
                (
                    true,
                    value_of(&SyncChangesListReply {
                        changes,
                        ..Default::default()
                    }),
                )
            }
            "fauna.sync.changes.record" => {
                let bytes = fauna_core::encoding::canonical_encode(payload).unwrap();
                let request: fauna_protocol::sync::SyncChangeRecordRequest =
                    fauna_protocol::decode_strict(&bytes).expect("a well-formed record");
                self.records.lock().unwrap().push(request.path);
                error("fauna.test.unavailable")
            }
            "fauna.folders.members.list_actors" => {
                *self.roster_reads.lock().unwrap() += 1;
                if *self.roster_fails.lock().unwrap() {
                    return error("fauna.test.unavailable");
                }
                match self.roster.lock().unwrap().clone() {
                    Some(reply) => (true, value_of(&reply)),
                    None => error(ROSTER_NOT_SHARED),
                }
            }
            _ => error("fauna.test.unexpected_kind"),
        }
    }
}

fn member(actor: &ActorKeypair, role: &str, access: Option<&str>) -> FolderActorMember {
    FolderActorMember {
        actor_id: actor.actor_id().to_hex(),
        role: role.into(),
        access: access.map(str::to_string),
        ..Default::default()
    }
}

/// The owner's engine on `watch` over `db`, its control plane answered by
/// `nest` and its byte plane at `blob_uri`, with the binding
/// `engine_lifecycle` installs for an owned set of an account that succeeded
/// from `predecessors` (their roots held, paired with their ids).
fn engine_on(
    nest: &Arc<NestDouble>,
    blob_uri: &str,
    watch: &std::path::Path,
    db: SyncDb,
    predecessors: &[(&ActorKeypair, BackupKey)],
) -> SyncEngine {
    let answering = Arc::clone(nest);
    let (nest_client, _server, _supervisor) =
        stand_up_ws_answering(owner(), move |kind, payload| {
            answering.answer(kind, payload)
        });
    let mut engine = SyncEngine::new(
        watch.to_path_buf(),
        db,
        test_sync_client(blob_uri),
        Some(FOLDER.to_string()),
        [0u8; 32],
        None, // mls
        None, // epoch_secret
        Some(owner_key().into()),
        None, // mls_group_id
        None, // content_keys
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        nest_client,
        crate::config::SyncMode::Sync,
    );
    engine.set_predecessor_backup_keys(PredecessorSealKey::chain(
        predecessors
            .iter()
            .map(|(actor, key)| (actor.actor_id(), key.clone())),
    ));
    engine.set_reader_binding(binding(
        &predecessors.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
    ));
    engine
}

fn binding(predecessors: &[&ActorKeypair]) -> ReaderBinding {
    ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(owner().actor_id().0),
        account: Some(owner().actor_id().0),
        account_predecessors: predecessors.iter().map(|p| p.actor_id().0).collect(),
        ..Default::default()
    }
}

/// A peer device's row for `path` at `seq`, signed directly by `signer` under
/// `nonce`, its label sealed under `label_key`'s owner root: a create/modify
/// naming `manifest`, or a delete (`None`).
fn row_under(
    signer: &ActorKeypair,
    nonce: [u8; 32],
    seq: i64,
    path: &str,
    manifest: Option<ContentHash>,
    label_key: &BackupKey,
) -> SyncChange {
    let mut row = SyncChange {
        seq,
        path_hash: hex::encode(fauna_core::sync::path_hash(path)),
        manifest_hash: manifest.map(|m| hex::encode(m.digest())),
        size_bytes: 10,
        change_type: if manifest.is_some() {
            "create"
        } else {
            "delete"
        }
        .into(),
        created_at: 1_700_000_000_000,
        device_id: Some("ee".repeat(32)),
        author_actor_id: Some(signer.actor_id().to_hex()),
        path_sealed: Some(
            fauna_core::label_custody::seal_path(&LabelRoot::owner_of(label_key), path)
                .unwrap()
                .into(),
        ),
        derived_through: Some(seq - 1),
        ..Default::default()
    };
    let key = ChangeSigner::direct(signer);
    let statement = SignedChange::for_row(&row, nonce).unwrap();
    row.signature = Some(fauna_protocol::ByteBuf::from(
        key.sign_statement(&statement).to_vec(),
    ));
    row.signer_key = Some(fauna_protocol::ByteBuf::from(key.signer_key().to_vec()));
    row
}

/// [`row_under`] for the owner's own row under the set's nonce.
fn owners_row(seq: i64, path: &str, manifest: Option<ContentHash>) -> SyncChange {
    row_under(&owner(), SET_NONCE, seq, path, manifest, &owner_key())
}

/// `path`'s wire key — what a skip is noted under.
fn hash_of(path: &str) -> String {
    hex::encode(fauna_core::sync::path_hash(path))
}

fn body(fill: u8) -> Vec<u8> {
    (0..4_000u32).map(|i| (i % 251) as u8 ^ fill).collect()
}

/// `body(fill)` on the byte plane, sealed under `key`'s chunk root.
fn stored(store: &BlobStore, fill: u8, key: &BackupKey) -> ContentHash {
    store_sealed_fixture(store, &body(fill), &key.convergent_chunk_root())
}

/// This device holds `content` at `path`, synced at `manifest` — a settled
/// row whose recorded identity is what the disk hashes to, i.e. exactly the
/// state in which the delete arm unlinks.
fn hold_synced(
    engine: &SyncEngine,
    watch: &std::path::Path,
    path: &str,
    content: &[u8],
    manifest: ContentHash,
) {
    std::fs::write(watch.join(path), content).unwrap();
    let file_hash = fauna_core::chunker_stream::content_hash_streaming(&watch.join(path)).unwrap();
    engine
        .db()
        .upsert_entry(
            path,
            Some(file_hash),
            Some(file_hash),
            Some(manifest),
            SyncState::Synced,
            1,
            1,
            content.len() as i64,
            1,
            None,
        )
        .unwrap();
}

// ─────────────────────────────────────────────────────────────────────
// (8)(e) — the roster is read again before a writer is refused
// ─────────────────────────────────────────────────────────────────────

/// A newly granted writer's first row, met with a roster read before the
/// grant, is admitted after ONE re-read for the batch — and a re-read that
/// fails changes nothing: the last roster stands, and the refusal against it.
#[tokio::test]
async fn a_newly_granted_writers_row_is_admitted_after_one_roster_re_read() {
    let nest = NestDouble::serving(vec![]);
    nest.set_roster(&[]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    engine.refresh_reader_roster().await;
    assert_eq!(
        nest.roster_reads(),
        1,
        "the roster as read before the grant"
    );

    // The grant lands on the nest; this reader's roster predates it.
    nest.set_roster(&[&writer()]);
    let rows = vec![
        row_under(&writer(), SET_NONCE, 5, "a.txt", None, &owner_key()),
        row_under(&writer(), SET_NONCE, 6, "b.txt", None, &owner_key()),
    ];
    let (kept, held) = engine.verify_served_rows(rows, &[]).await;
    assert_eq!(held, None);
    assert_eq!(
        kept.iter().map(|c| c.seq).collect::<Vec<_>>(),
        vec![5, 6],
        "admitted against the roster as re-read"
    );
    assert_eq!(
        nest.roster_reads(),
        2,
        "one re-read for the batch, not one per row"
    );

    // A stranger's row: the re-read fails, the last roster stands, refused.
    *nest.roster_fails.lock().unwrap() = true;
    let stranger = ActorKeypair::from_secret([0x5f; 32]);
    let (kept, held) = engine
        .verify_served_rows(
            vec![row_under(
                &stranger,
                SET_NONCE,
                7,
                "c.txt",
                None,
                &owner_key(),
            )],
            &[],
        )
        .await;
    assert_eq!(
        held, None,
        "refused, never held: offline, the last roster stands"
    );
    assert!(kept.is_empty());
    assert_eq!(nest.roster_reads(), 3);
    // ...and the writer the last successful read named is still one.
    let (kept, _) = engine
        .verify_served_rows(
            vec![row_under(
                &writer(),
                SET_NONCE,
                8,
                "a.txt",
                None,
                &owner_key(),
            )],
            &[],
        )
        .await;
    assert_eq!(kept.len(), 1);
    assert_eq!(nest.roster_reads(), 3, "an admitted batch reads nothing");
}

// ─────────────────────────────────────────────────────────────────────
// (8)(f) — the skip note
// ─────────────────────────────────────────────────────────────────────

/// A row refused for want of a writer notes the skip on its path: a local
/// edit of that path made afterwards does not claim to derive from the head
/// this device never read. A row that fails cryptographically is not a record
/// and notes nothing.
#[tokio::test]
async fn a_row_refused_for_want_of_a_writer_notes_the_skip_on_its_path() {
    let nest = NestDouble::serving(vec![]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    let stranger = ActorKeypair::from_secret([0x5f; 32]);
    let (kept, held) = engine
        .verify_served_rows(
            vec![
                row_under(&stranger, SET_NONCE, 7, "unread.txt", None, &owner_key()),
                row_under(&owner(), OTHER_NONCE, 8, "forged.txt", None, &owner_key()),
            ],
            &[],
        )
        .await;
    assert_eq!(
        (kept.len(), held),
        (0, None),
        "both refused, the cursor passes them"
    );

    // The cursor is past both; an edit stamps the honest anchor.
    assert_eq!(
        engine.causal().honest_anchor("unread.txt", 20),
        6,
        "an edit of the skipped path claims nothing at or past the head it never read"
    );
    assert_eq!(
        engine.causal().honest_anchor("forged.txt", 20),
        20,
        "a row that fails cryptographically is not a record: no skip"
    );
    assert_eq!(engine.causal().honest_anchor("elsewhere.txt", 20), 20);
}

// ─────────────────────────────────────────────────────────────────────
// (8)(f) — the head re-judge
// ─────────────────────────────────────────────────────────────────────

/// **The heal.** The cursor passes a predecessor-signed head while this
/// reader has not proven the predecessor (refused, skip noted); the reader
/// then gains the predecessor; the next pull's pass folds the head and the
/// file materialises — opened under the predecessor's own root.
#[tokio::test]
async fn a_passed_predecessor_signed_head_is_folded_once_the_predecessor_is_admitted() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let inherited = stored(&store, 1, &predecessor_key());
    let path = "inherited.bin";
    let nest = NestDouble::serving(vec![row_under(
        &predecessor(),
        SET_NONCE,
        5,
        path,
        Some(inherited),
        &predecessor_key(),
    )]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        &server.uri(),
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[(&predecessor(), predecessor_key())],
    );
    // The root is held, the link is not proven yet.
    engine.set_reader_binding(binding(&[]));

    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(engine.db().get_anchor().unwrap(), 5, "the cursor passed it");
    assert!(!watch.path().join(path).exists(), "refused: nothing folded");
    assert_eq!(
        engine.causal().honest_anchor(path, 5),
        4,
        "the skip is noted"
    );

    engine.set_reader_binding(binding(&[&predecessor()]));
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(
        std::fs::read(watch.path().join(path)).expect("the head materialises"),
        body(1)
    );
    assert!(
        !engine.db().head_rejudge_owed().unwrap(),
        "the pass completed"
    );
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        5,
        "the cursor never moved"
    );
    assert_eq!(
        engine.causal().honest_anchor(path, 5),
        5,
        "the frontier reached the head: its skip released"
    );
}

/// The pass folds nothing around the per-signer root bound (ruling (8)(c)):
/// a predecessor-signed head naming bytes the CURRENT root sealed goes
/// through the verify step's own door, so its manifest is opened under the
/// predecessor's roots alone — a noted skip, recorded once however often the
/// pass meets it, never a hold and never a materialised file.
#[tokio::test]
async fn the_pass_folds_no_predecessor_signed_head_around_the_signer_bound() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let planted = stored(&store, 4, &owner_key());
    let path = "after-the-ceremony.bin";
    let nest = NestDouble::serving(vec![row_under(
        &predecessor(),
        SET_NONCE,
        5,
        path,
        Some(planted),
        &predecessor_key(),
    )]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        &server.uri(),
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[(&predecessor(), predecessor_key())],
    );
    engine.db().set_anchor(5).unwrap();

    for _ in 0..2 {
        let pass = engine.rejudge_passed_heads(FOLDER).await.expect("pass");
        assert_eq!(
            (pass.complete, pass.folded),
            (true, 0),
            "a noted skip, never a hold"
        );
    }
    assert!(
        !watch.path().join(path).exists(),
        "a predecessor's signature must not open a current-root manifest"
    );
    assert_eq!(
        engine.causal().honest_anchor(path, 5),
        4,
        "the skip is noted"
    );
    let recorded = engine
        .db()
        .list_unresolved_conflicts()
        .unwrap()
        .into_iter()
        .filter(|(_, row_path, kind, _, _)| kind == "catchup_failed" && row_path == path)
        .count();
    assert_eq!(recorded, 1, "recorded once, not once per pass");
}

/// **The review's first unlink scenario.** Log `create@10`, `delete@20`,
/// `create@30`, the page boundary between 20 and 30, on a device holding the
/// file at 30 — the pass unlinks nothing. The log is read whole before
/// anything folds, so the path's head is the create, never the delete a page
/// ahead of it. Pinned in the one state where the whole read alone holds: the
/// delete was refused when first served (its skip still on record) and the
/// path's frontier file is lost — either would refuse the delete on its own.
#[tokio::test]
async fn a_superseded_delete_a_page_ahead_of_its_create_unlinks_nothing() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let (v10, v30) = (
        stored(&store, 1, &owner_key()),
        stored(&store, 3, &owner_key()),
    );
    let path = "kept.bin";
    let nest = NestDouble::serving(vec![
        owners_row(10, path, Some(v10)),
        owners_row(20, path, None),
        owners_row(30, path, Some(v30)),
    ]);
    *nest.page.lock().unwrap() = 2;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        &server.uri(),
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    hold_synced(&engine, watch.path(), path, &body(3), v30);
    engine
        .causal()
        .note_permanent_skip(Some(&hash_of(path)), 20);
    engine.db().set_anchor(30).unwrap();

    let pass = engine.rejudge_passed_heads(FOLDER).await.expect("pass");
    assert!(pass.complete);
    assert_eq!(pass.folded, 0);
    assert_eq!(
        *nest.list_since.lock().unwrap(),
        vec![0, 20],
        "both pages read"
    );
    assert_eq!(
        std::fs::read(watch.path().join(path)).expect("the live file is untouched"),
        body(3)
    );
    assert_eq!(
        engine.db().get_entry(path).unwrap().unwrap().state,
        SyncState::Synced
    );
}

/// **The review's second unlink scenario.** The cursor at 20 with a head at
/// 30 above it — the pass does not fold it (nor anything it would have to
/// read past its cursor for), and the ordinary pull then takes `delete@25`,
/// `create@30` in order.
#[tokio::test]
async fn a_head_above_the_cursor_is_left_to_the_ordinary_pull() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let (v10, v30) = (
        stored(&store, 1, &owner_key()),
        stored(&store, 3, &owner_key()),
    );
    let path = "kept.bin";
    let nest = NestDouble::serving(vec![
        owners_row(10, path, Some(v10)),
        owners_row(25, path, None),
        owners_row(30, path, Some(v30)),
    ]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        &server.uri(),
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    hold_synced(&engine, watch.path(), path, &body(1), v10);
    engine.causal().advance_frontier(path, 10);
    engine.db().set_anchor(20).unwrap();

    let pass = engine.rejudge_passed_heads(FOLDER).await.expect("pass");
    assert_eq!((pass.complete, pass.folded), (true, 0));
    assert!(
        !store.manifest_was_fetched(v30),
        "a head above the cursor is not the pass's"
    );
    assert_eq!(std::fs::read(watch.path().join(path)).unwrap(), body(1));
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        20,
        "the cursor never moved"
    );

    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(engine.db().get_anchor().unwrap(), 30);
    assert!(
        store.manifest_was_fetched(v30),
        "the ordinary pull delivers the create"
    );
    assert!(
        watch.path().join(path).exists(),
        "the delete below it in the batch unlinked nothing"
    );
}

/// A head at or below the path's EDIT-frontier is a duplicate too: a delete
/// this device skipped (noted) is not folded over the device's own later edit
/// of the path — recorded and acked at 25, its echo still above the cursor.
/// The path's frontier still reads 10; only the edit-frontier knows.
#[tokio::test]
async fn a_noted_delete_head_below_this_devices_own_later_edit_unlinks_nothing() {
    let path = "edited.bin";
    let nest = NestDouble::serving(vec![
        owners_row(10, path, Some(ContentHash::of_raw(b"m10"))),
        owners_row(20, path, None),
    ]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    hold_synced(
        &engine,
        watch.path(),
        path,
        b"edited here",
        ContentHash::of_raw(b"m25"),
    );
    engine.causal().advance_frontier(path, 10);
    engine
        .causal()
        .note_permanent_skip(Some(&hash_of(path)), 20);
    engine.causal().advance_edit_frontier(path, 25);
    engine.db().set_anchor(22).unwrap();

    let pass = engine.rejudge_passed_heads(FOLDER).await.expect("pass");
    assert_eq!((pass.complete, pass.folded), (true, 0));
    assert_eq!(
        std::fs::read(watch.path().join(path)).expect("the edited file is untouched"),
        b"edited here"
    );
}

/// A delete this device already READ is never folded again. The delete arm
/// advances no frontier, so an applied delete reads as above the path's
/// frontier for ever — and a file re-created since is `Synced` on disk before
/// its record is acked, so neither frontier knows it yet. What the pass asks
/// instead is this device's own word that it never read the row: no skip
/// noted, no fold.
#[tokio::test]
async fn a_delete_this_device_already_read_is_never_folded_again() {
    let path = "recreated.bin";
    let nest = NestDouble::serving(vec![
        owners_row(10, path, Some(ContentHash::of_raw(b"m10"))),
        owners_row(20, path, None),
    ]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    // create@10 pulled, delete@20 applied, then re-created here: uploaded,
    // the row settled, the record not acked.
    hold_synced(
        &engine,
        watch.path(),
        path,
        b"re-created",
        ContentHash::of_raw(b"m10"),
    );
    engine.causal().advance_frontier(path, 10);
    engine.db().set_anchor(20).unwrap();

    let pass = engine.rejudge_passed_heads(FOLDER).await.expect("pass");
    assert_eq!((pass.complete, pass.folded), (true, 0));
    assert_eq!(
        std::fs::read(watch.path().join(path)).expect("the re-created file is untouched"),
        b"re-created"
    );
}

/// The heal, for a delete: a predecessor-signed delete this device refused
/// (skip noted) is folded once the predecessor is admitted — the ordinary
/// delete arm unlinks the settled file — and then never again: the note is
/// released, so a file re-created at the path survives the next pass.
#[tokio::test]
async fn a_skipped_delete_head_is_folded_once_its_signer_is_admitted_and_only_once() {
    let path = "retired.bin";
    let nest = NestDouble::serving(vec![
        owners_row(10, path, Some(ContentHash::of_raw(b"m10"))),
        row_under(
            &predecessor(),
            SET_NONCE,
            20,
            path,
            None,
            &predecessor_key(),
        ),
    ]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[(&predecessor(), predecessor_key())],
    );
    engine.set_reader_binding(binding(&[]));
    hold_synced(
        &engine,
        watch.path(),
        path,
        b"synced at 10",
        ContentHash::of_raw(b"m10"),
    );
    engine.causal().advance_frontier(path, 10);
    engine.db().set_anchor(10).unwrap();

    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        20,
        "the cursor passed it"
    );
    assert!(watch.path().join(path).exists(), "refused: nothing applied");
    assert!(engine.causal().skip_noted(&hash_of(path), 20));

    engine.set_reader_binding(binding(&[&predecessor()]));
    engine.pull_remote_changes().await.expect("pull");
    assert!(
        !watch.path().join(path).exists(),
        "the admitted delete is applied"
    );
    assert!(
        !engine.causal().skip_noted(&hash_of(path), 20),
        "read now: its skip is released"
    );
    assert_eq!(engine.causal().honest_anchor(path, 20), 20);

    hold_synced(
        &engine,
        watch.path(),
        path,
        b"re-created",
        ContentHash::of_raw(b"m10"),
    );
    let pass = engine.rejudge_passed_heads(FOLDER).await.expect("pass");
    assert_eq!((pass.complete, pass.folded), (true, 0));
    assert!(watch.path().join(path).exists(), "never folded twice");
}

/// A retention row is never taken as a path's head: with one above the
/// path's content row, the pass folds the content row.
#[tokio::test]
async fn a_retention_row_is_never_a_paths_head() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let (head, loser) = (
        stored(&store, 1, &owner_key()),
        stored(&store, 2, &owner_key()),
    );
    let path = "contested.bin";
    let mut retention = owners_row(12, path, Some(loser));
    retention.is_retention = Some(true);
    retention.signature = None;
    retention.signer_key = None;
    let nest = NestDouble::serving(vec![owners_row(10, path, Some(head)), retention]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        &server.uri(),
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    engine.db().set_anchor(15).unwrap();

    let pass = engine.rejudge_passed_heads(FOLDER).await.expect("pass");
    assert_eq!((pass.complete, pass.folded), (true, 1));
    assert!(!store.manifest_was_fetched(loser));
    assert_eq!(
        std::fs::read(watch.path().join(path)).expect("the content row folds"),
        body(1)
    );
}

// ─────────────────────────────────────────────────────────────────────
// (3) — an item class routes a row away from the file reader
// ─────────────────────────────────────────────────────────────────────

/// A lying nest's forged, unsigned row labelled `state-entry` — carrying a
/// path and a manifest, on a folder feed the honest nest never puts one on —
/// is consumed by no file reader: the pull drops it, keeps the rows around
/// it, and the cursor passes it (nothing is held).
#[tokio::test]
async fn a_forged_item_class_row_is_no_file_row() {
    let nest = NestDouble::serving(vec![]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    for class in [
        fauna_protocol::account_state::ItemClass::StateEntry,
        fauna_protocol::account_state::ItemClass::RecordCid,
    ] {
        let mut forged = owners_row(6, "planted.bin", Some(ContentHash::of_raw(b"x")));
        forged.signature = None;
        forged.signer_key = None;
        forged.change_type = "modify".into();
        forged.item_class = Some(class.as_wire().into());
        let rows = vec![
            owners_row(5, "a.txt", None),
            forged,
            owners_row(7, "b.txt", None),
        ];
        let (kept, held) = engine.verify_served_rows(rows, &[]).await;
        assert_eq!(held, None, "{class:?}: the cursor passes it");
        assert_eq!(
            kept.iter().map(|c| c.seq).collect::<Vec<_>>(),
            vec![5, 7],
            "{class:?}: dropped, its neighbours kept"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Custody (g) — the re-record leg places by a path it checked
// ─────────────────────────────────────────────────────────────────────

/// This device's own head on the public plane (no `path_sealed`), signed
/// under `nonce` at `signed_path` and served with the plaintext
/// `served_path` — the one field of the row the statement does not cover.
fn own_plaintext_head(
    seq: i64,
    signed_path: &str,
    served_path: &str,
    nonce: [u8; 32],
) -> SyncChange {
    let mut row = SyncChange {
        seq,
        path_hash: hash_of(signed_path),
        path: Some(served_path.into()),
        manifest_hash: Some(hex::encode(ContentHash::of_raw(b"mine").digest())),
        size_bytes: 10,
        change_type: "create".into(),
        created_at: 1_700_000_000_000,
        device_id: Some(hex::encode([0u8; 32])),
        author_actor_id: Some(owner().actor_id().to_hex()),
        derived_through: Some(seq - 1),
        ..Default::default()
    };
    let key = ChangeSigner::direct(&owner());
    let statement = SignedChange::for_row(&row, nonce).unwrap();
    row.signature = Some(fauna_protocol::ByteBuf::from(
        key.sign_statement(&statement).to_vec(),
    ));
    row.signer_key = Some(fauna_protocol::ByteBuf::from(key.signer_key().to_vec()));
    row
}

/// The re-record leg fetches unjudged, so it checks a plaintext `path`
/// against the signed `path_hash` itself: a head this device signed under a
/// retired nonce is re-recorded at its own path, and one the nest serves
/// with a path of its choosing is not re-recorded anywhere.
#[tokio::test]
async fn the_re_record_leg_re_records_no_head_at_a_path_the_nest_chose() {
    let nest = NestDouble::serving(vec![
        own_plaintext_head(5, "honest.txt", "honest.txt", OTHER_NONCE),
        own_plaintext_head(6, "mine.txt", "planted.txt", OTHER_NONCE),
    ]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[],
    );
    engine.set_change_signer(
        Some(Arc::new(ChangeSigner::direct(&owner()))),
        Some(SET_NONCE),
    );
    engine.set_retired_set_nonces(vec![OTHER_NONCE]);

    engine
        .rerecord_under_live_nonce()
        .await
        .expect("the leg runs");
    assert_eq!(
        *nest.records.lock().unwrap(),
        vec!["honest.txt".to_string()],
        "the honest head is re-recorded at its own path; the planted one nowhere"
    );
}

// ─────────────────────────────────────────────────────────────────────
// (8)(f) — the trigger
// ─────────────────────────────────────────────────────────────────────

/// A seat on one set across restarts: a file-backed engine db and one watch
/// directory, a fresh engine (and in-memory roster) per "start".
struct Seat {
    watch: tempfile::TempDir,
    state: tempfile::TempDir,
}

impl Seat {
    fn new() -> Self {
        Self {
            watch: tempfile::tempdir().unwrap(),
            state: tempfile::tempdir().unwrap(),
        }
    }

    fn start(&self, nest: &Arc<NestDouble>) -> SyncEngine {
        let db = SyncDb::open(self.state.path().join("sync.db")).unwrap();
        engine_on(nest, NO_BLOB_PLANE, self.watch.path(), db, &[])
    }
}

/// Once per set on the first run of the build, then never again while what
/// the reader admits by is unchanged — across a restart too, where the roster
/// (in memory only) is unread at first and then read back the same.
#[tokio::test]
async fn a_restart_with_an_unchanged_roster_runs_no_pass() {
    let nest = NestDouble::serving(vec![]);
    nest.set_roster(&[&writer()]);
    let seat = Seat::new();

    let engine = seat.start(&nest);
    engine.db().set_anchor(7).unwrap();
    engine.refresh_reader_roster().await;
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 1, "the first run of the build owes one pass");
    assert!(!engine.db().head_rejudge_owed().unwrap());
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 1, "nothing gained: no pass");
    drop(engine);

    let engine = seat.start(&nest);
    engine.pull_remote_changes().await.expect("pull");
    engine.refresh_reader_roster().await;
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(
        nest.passes(),
        1,
        "a restart gains nothing it had already — before the roster is read back, and after"
    );
}

/// A pass that fails part-way stays owed and runs again at the next start.
#[tokio::test]
async fn a_pass_interrupted_part_way_runs_again_at_the_next_start() {
    let nest = NestDouble::serving(vec![
        owners_row(3, "a.txt", None),
        owners_row(6, "b.txt", None),
    ]);
    *nest.page.lock().unwrap() = 1;
    // The pass's second page fails (and the ordinary pull behind it).
    *nest.list_fails_from.lock().unwrap() = Some(1);
    let seat = Seat::new();

    let engine = seat.start(&nest);
    engine.db().set_anchor(7).unwrap();
    let _ = engine.pull_remote_changes().await;
    assert!(
        engine.db().head_rejudge_owed().unwrap(),
        "interrupted: still owed"
    );
    assert_eq!(nest.passes(), 1);
    drop(engine);

    *nest.list_fails_from.lock().unwrap() = None;
    let engine = seat.start(&nest);
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 2, "the owed pass ran again");
    assert!(!engine.db().head_rejudge_owed().unwrap(), "and completed");
}

/// The trigger is a GAIN: a writer removed owes nothing; the same writer
/// granted again does.
#[tokio::test]
async fn the_trigger_fires_on_a_gain_and_never_on_a_loss() {
    let nest = NestDouble::serving(vec![]);
    nest.set_roster(&[&writer()]);
    let seat = Seat::new();
    let engine = seat.start(&nest);
    engine.db().set_anchor(7).unwrap();
    engine.refresh_reader_roster().await;
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 1, "the first run");

    nest.set_roster(&[]);
    engine.refresh_reader_roster().await;
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 1, "a writer removed: no pass");
    assert!(!engine.db().head_rejudge_owed().unwrap());

    nest.set_roster(&[&writer()]);
    engine.refresh_reader_roster().await;
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 2, "a writer granted: one pass");
}

// ─────────────────────────────────────────────────────────────────────
// Ruling (11) — the succession cut: three verdicts where there were two
// (`writer-signed-change-records.md` ruling (11)(c))
// ─────────────────────────────────────────────────────────────────────

/// The owner's binding after a cut: the live nonce [`SET_NONCE`] minted by
/// `live_minter` (`None`: none recorded), [`OTHER_NONCE`] retired into the
/// lineage, minted by the predecessor, and the predecessor as the owner's
/// chain.
fn cut_binding(live_minter: Option<&ActorKeypair>) -> ReaderBinding {
    ReaderBinding {
        live_minted_by: live_minter.map(|k| k.actor_id().0),
        retired_set_nonces: vec![(OTHER_NONCE, Some(predecessor().actor_id().0))],
        owner_chain: vec![predecessor().actor_id().0],
        ..binding(&[&predecessor()])
    }
}

/// (i) A predecessor-signed row under the LIVE nonce, which the successor
/// minted, is refused — the nonce postdates the predecessor's retirement, so
/// the row is a plant — where before the cut it was admitted.
#[tokio::test]
async fn i_a_predecessor_signed_row_under_a_successor_minted_live_nonce_is_refused() {
    let nest = NestDouble::serving(vec![]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[(&predecessor(), predecessor_key())],
    );
    let row = row_under(
        &predecessor(),
        SET_NONCE,
        5,
        "plant.bin",
        None,
        &predecessor_key(),
    );
    engine.set_reader_binding(cut_binding(Some(&owner())));
    let (kept, held) = engine.verify_served_rows(vec![row.clone()], &[]).await;
    assert_eq!((kept.len(), held), (0, None), "refused, never held");
    // (iv) the pre-cut shape — no minter recorded — admits it (ruling (8)).
    engine.set_reader_binding(cut_binding(None));
    let (kept, _) = engine.verify_served_rows(vec![row], &[]).await;
    assert_eq!(kept.len(), 1, "the un-cut inheritance");
}

/// (ii′) A predecessor-signed row under a live nonce the predecessor itself
/// minted is current: only a minter that is a strict successor refuses.
#[tokio::test]
async fn ii_prime_a_predecessor_signed_row_under_its_own_live_nonce_is_admitted() {
    let nest = NestDouble::serving(vec![]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[(&predecessor(), predecessor_key())],
    );
    engine.set_reader_binding(cut_binding(Some(&predecessor())));
    let row = row_under(
        &predecessor(),
        SET_NONCE,
        5,
        "kept.bin",
        None,
        &predecessor_key(),
    );
    let (kept, _) = engine.verify_served_rows(vec![row], &[]).await;
    assert_eq!(kept.len(), 1);
}

/// (ii) A predecessor-signed row under a RETIRED nonce is history: folded by
/// nothing, its skip noted on the path — and a history DELETE head is never
/// folded by the head re-judge, whatever skip note it left.
#[tokio::test]
async fn ii_a_history_row_folds_nothing_and_a_history_delete_head_is_never_folded() {
    let path = "kept.bin";
    let nest = NestDouble::serving(vec![
        owners_row(10, path, Some(ContentHash::of_raw(b"m10"))),
        row_under(
            &predecessor(),
            OTHER_NONCE,
            20,
            path,
            None,
            &predecessor_key(),
        ),
    ]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[(&predecessor(), predecessor_key())],
    );
    engine.set_reader_binding(cut_binding(Some(&owner())));
    hold_synced(
        &engine,
        watch.path(),
        path,
        b"synced at 10",
        ContentHash::of_raw(b"m10"),
    );
    engine.causal().advance_frontier(path, 10);
    engine.db().set_anchor(10).unwrap();

    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        20,
        "the cursor passed it"
    );
    assert!(watch.path().join(path).exists(), "history: nothing applied");
    assert!(
        engine.causal().skip_noted(&hash_of(path), 20),
        "a passed history head is noted, so the honest anchor accounts for it"
    );

    let pass = engine.rejudge_passed_heads(FOLDER).await.expect("pass");
    assert_eq!(pass.folded, 0, "the re-judge folds current verdicts only");
    assert!(
        watch.path().join(path).exists(),
        "a history delete's skip note is never the licence to fold it"
    );
}

/// (iii) A member writer's row under a retired nonce stays current: a cut
/// touches nothing a member signed.
#[tokio::test]
async fn iii_a_members_row_under_a_retired_nonce_stays_verified() {
    let nest = NestDouble::serving(vec![]);
    nest.set_roster(&[&writer()]);
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(
        &nest,
        NO_BLOB_PLANE,
        watch.path(),
        SyncDb::open_in_memory().unwrap(),
        &[(&predecessor(), predecessor_key())],
    );
    engine.set_reader_binding(cut_binding(Some(&owner())));
    let row = row_under(&writer(), OTHER_NONCE, 5, "theirs.bin", None, &owner_key());
    let (kept, held) = engine.verify_served_rows(vec![row], &[]).await;
    assert_eq!((kept.len(), held), (1, None));
    assert_eq!(
        kept[0].author_actor_id.as_deref(),
        Some(writer().actor_id().to_hex().as_str())
    );
}

/// The `nonce:` gain token: a cut reaching the binding — a new live nonce,
/// the old one retired into the lineage — owes one head re-judge pass.
#[tokio::test]
async fn a_cut_reaching_the_binding_owes_one_head_rejudge_pass() {
    let nest = NestDouble::serving(vec![]);
    let seat = Seat::new();
    let engine = seat.start(&nest);
    engine.db().set_anchor(7).unwrap();
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 1, "the first run");
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 1, "nothing gained");

    engine.set_reader_binding(ReaderBinding {
        set_nonce: Some([9; 32]),
        retired_set_nonces: vec![(SET_NONCE, None)],
        ..binding(&[])
    });
    engine.pull_remote_changes().await.expect("pull");
    assert_eq!(nest.passes(), 2, "a nonce gained: one pass");
}
