//! An offline placeholder delete propagates — the engine's half of the ruling
//! (`docs/goal/behavior/delete-propagation.md` § *The floor on an on-demand
//! root* → **An offline placeholder delete propagates — the sweep sees what the
//! watcher would have seen**, decisions (a), (b), (d) and (f)).
//!
//! The delete universe is the rows with evidence: `Synced` ∪ **seen**
//! `Placeholder` (∪ `LocallyDeleted`, a seen placeholder whose delete is still
//! owed). A *never-seen* placeholder's absence is evidence of nothing — lazy
//! population leaves most of them off the disk by design — so it is neither
//! counted by the floor nor deletable by the scan. A seen placeholder found gone
//! becomes `LocallyDeleted` in the same pass: never listed back onto the disk,
//! retried by every later `reconcile`, tombstoned only on the nest's ack.
//!
//! Two engine shapes carry the two record outcomes: a **folderless** engine has
//! nothing to record, so its `handle_delete` tombstones at once (the ack-landed
//! shape); an engine with a folder over an unreachable nest FAILS every record
//! (the nest-down shape). The nest-down-then-up story runs over a real
//! `NestClient` with only the socket mocked.
//!
//! The windows host's half — marking what cfapi put on the disk, the boot sweep
//! before the root connects, the mark hygiene — is pinned in
//! `fauna-sync-agent` (`bridge.rs` tests + `cfapi_live_integration.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, NestClient, PushBroker};
use fauna_core::data::ContentHash;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::sync::{SyncChange, SyncChangeRecordReply};
use fauna_protocol::{Frame, Reply, RpcError, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};

use crate::db::SyncState;
use crate::engine::SyncEngine;
use crate::enumerate::PlaceholderLister;
use crate::provider_face::{ProviderEngine, RowPresence};
use crate::pull_remote_changes_test::{
    seed_tracked_file, test_engine, test_engine_folderless_with_progress,
    test_engine_with_nest_client,
};

/// A deterministic per-path manifest digest — the head a fold would anchor to.
fn head_digest(rel: &str) -> [u8; 32] {
    let mut d = [0u8; 32];
    for (i, b) in rel.bytes().enumerate() {
        d[i % 32] ^= b;
    }
    d
}

/// A folded cloud-only row — what `record_placeholders_from_changes` leaves:
/// a manifest anchor, no local identity, NOT seen (nothing is on the disk yet).
fn seed_placeholder(engine: &SyncEngine, rel: &str) {
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(ContentHash::from_digest_raw(head_digest(rel))),
            SyncState::Placeholder,
            0,
            1_700_000_000,
            5,
            1,
            None,
        )
        .unwrap();
}

/// [`seed_placeholder`], then marked seen — the engine put it on the disk once.
fn seed_seen_placeholder(engine: &SyncEngine, rel: &str) {
    seed_placeholder(engine, rel);
    engine.db().mark_seen(&[rel]).unwrap();
}

fn state(engine: &SyncEngine, rel: &str) -> Option<SyncState> {
    engine.db().get_entry(rel).unwrap().map(|e| e.state)
}

fn seen(engine: &SyncEngine, rel: &str) -> bool {
    engine.db().get_entry(rel).unwrap().unwrap().seen_on_disk
}

fn folderless(dir: &std::path::Path) -> SyncEngine {
    test_engine_folderless_with_progress(dir.to_path_buf()).0
}

// ─────────────────────────────────────────────────────────────────────
// (b) the universe
// ─────────────────────────────────────────────────────────────────────

/// **Never-seen is never evidence** — the lazy-population invariant. Two folded
/// placeholders were never put on this disk (their directory was never browsed);
/// a scan that does not find them records nothing, holds nothing, and leaves the
/// rows exactly as the fold wrote them. A floor that counted them would read
/// every never-browsed subtree as a wholesale vanish.
#[tokio::test]
async fn a_never_seen_placeholder_absent_from_the_scan_is_evidence_of_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_placeholder(&engine, "a.txt");
    seed_placeholder(&engine, "sub/b.txt");

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.deleted_files, 0, "nothing was ever on the disk");
    assert_eq!(stats.deletes_held, 0, "and nothing is a vanish");
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Placeholder));
    assert_eq!(state(&engine, "sub/b.txt"), Some(SyncState::Placeholder));
}

/// A **seen** placeholder the scan does not find is a delete — the offline twin
/// of the live watcher's `Removed` event. One of two rows with evidence is gone
/// (a hydrated sibling is present), so the floor stays quiet and the per-file
/// path records it; the folderless engine's record lands at once, so the row is
/// tombstoned.
#[tokio::test]
async fn a_seen_placeholder_absent_from_the_scan_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"still here");
    seed_seen_placeholder(&engine, "gone.txt");
    seed_placeholder(&engine, "never-listed.txt");

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(
        stats.deletes_held, 0,
        "one of two with evidence is not totality"
    );
    assert_eq!(
        stats.deleted_files, 1,
        "the seen placeholder's delete propagates"
    );
    assert_eq!(state(&engine, "gone.txt"), Some(SyncState::Deleted));
    assert_eq!(
        state(&engine, "never-listed.txt"),
        Some(SyncState::Placeholder),
        "the never-seen sibling is untouched"
    );
}

/// The floor keeps its shape and threshold; only its universe widened. Every
/// row with evidence gone at once — here two seen placeholders and nothing
/// hydrated, which counted nothing before the ruling — HOLDS, and the rows go
/// `LocallyDeleted` in the same pass so a re-armed population cannot list them
/// back and silently disarm the hold. The user's confirm (`apply_held_deletes`)
/// then applies exactly the held set.
#[tokio::test]
async fn every_seen_row_absent_holds_and_the_confirm_applies_it() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_seen_placeholder(&engine, "a.txt");
    seed_seen_placeholder(&engine, "sub/b.txt");
    seed_placeholder(&engine, "never-listed.txt");

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.deletes_held, 2, "both seen rows vanished at once");
    assert_eq!(stats.deleted_files, 0, "a hold records nothing");
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::LocallyDeleted));
    assert_eq!(state(&engine, "sub/b.txt"), Some(SyncState::LocallyDeleted));
    let listed: Vec<String> = engine
        .list_placeholder_rows()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.rel)
        .collect();
    assert_eq!(
        listed,
        vec!["never-listed.txt".to_string()],
        "a held, gone placeholder is never listed back onto the disk"
    );

    // Still held on the next pass — `LocallyDeleted` stays in the universe.
    let again = engine.reconcile().await.unwrap();
    assert_eq!(again.deletes_held, 2, "the hold is re-derived, not lost");

    let applied = engine.apply_held_deletes().await.unwrap();
    assert!(applied.floor_was_active);
    assert_eq!(applied.applied, 2);
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Deleted));
    assert_eq!(state(&engine, "sub/b.txt"), Some(SyncState::Deleted));
    assert_eq!(
        state(&engine, "never-listed.txt"),
        Some(SyncState::Placeholder)
    );
}

/// A seen placeholder is a row with evidence ONLY while the scan could look.
/// Under an unreadable directory it is withheld exactly like a `Synced` row —
/// the one shared derivation both delete verbs go through.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_prefix_withholds_a_seen_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"here");
    seed_seen_placeholder(&engine, "sub/gone.txt");
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    let stats = crate::mass_delete_floor_test::while_unreadable(&sub, || engine.reconcile())
        .await
        .unwrap();
    assert_eq!(stats.deleted_files, 0, "unreadable is not absent");
    assert_eq!(stats.deletes_skipped_unreadable, 1);
    assert_eq!(state(&engine, "sub/gone.txt"), Some(SyncState::Placeholder));
}

// ─────────────────────────────────────────────────────────────────────
// (a) the mark: observation and dehydration
// ─────────────────────────────────────────────────────────────────────

/// The self-heal: a placeholder the scan observes present-and-flagged is marked
/// seen, whoever put it there — which is what makes a lost mark write cost one
/// pass, never a delete. (The OS's `OFFLINE` bit is the half an ordinary process
/// may set, so this needs no sync root; `placeholder.rs` pins the predicate.)
#[cfg(windows)]
#[tokio::test]
async fn a_placeholder_the_scan_observes_is_marked_seen() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_placeholder(&engine, "a.txt");
    let f = dir.path().join("a.txt");
    std::fs::write(&f, b"bytes the OS will say are elsewhere").unwrap();
    set_offline(&f);
    assert!(!seen(&engine, "a.txt"), "precondition: folded, never seen");

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.placeholder_files, 1);
    assert!(seen(&engine, "a.txt"), "observed on the disk → seen");

    // …and so its later absence IS evidence.
    std::fs::remove_file(&f).unwrap();
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"here");
    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.deleted_files, 1);
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Deleted));
}

/// A `LocallyDeleted` row whose placeholder is observed on the disk again (the
/// old directory restored while the agent was down) was never deleted after all:
/// it goes back to a seen `Placeholder`, and no delete is recorded for it.
#[cfg(windows)]
#[tokio::test]
async fn a_locally_deleted_placeholder_observed_again_is_restored() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_seen_placeholder(&engine, "a.txt");
    engine
        .db()
        .update_state("a.txt", SyncState::LocallyDeleted)
        .unwrap();
    let f = dir.path().join("a.txt");
    std::fs::write(&f, b"it came back").unwrap();
    set_offline(&f);

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.deleted_files, 0);
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Placeholder));
    assert!(seen(&engine, "a.txt"));
}

#[cfg(windows)]
fn set_offline(path: &std::path::Path) {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileAttributesW(path: *const u16, attrs: u32) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path living until after the call.
    let rc = unsafe { SetFileAttributesW(wide.as_ptr(), 0x0000_1000) };
    assert_ne!(rc, 0, "SetFileAttributesW(OFFLINE) failed");
}

/// A dehydrate frees a hydrated file's bytes IN PLACE — the placeholder is on the
/// disk by the engine's own hand, so the row is seen whatever route it took to
/// `Synced` (a file uploaded from this disk was never transferred as a
/// placeholder, and must still count once freed).
#[tokio::test]
async fn a_dehydrated_row_is_seen() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "a.txt", b"uploaded from here");
    assert!(!seen(&engine, "a.txt"));

    engine.mark_placeholder("a.txt").unwrap();
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Placeholder));
    assert!(seen(&engine, "a.txt"), "freed in place — still on the disk");
}

// ─────────────────────────────────────────────────────────────────────
// (d) LocallyDeleted: durable, never listed, never present
// ─────────────────────────────────────────────────────────────────────

/// The nest-down shape, on the watcher's path: a placeholder deleted while the
/// agent runs goes through `handle_delete`, whose record FAILS (unreachable
/// nest). Record-first stands — nothing is tombstoned — but the row is no longer
/// a `Placeholder` that a re-armed population would list straight back: it is
/// `LocallyDeleted`, owed to the nest, excluded from every present-reading.
#[tokio::test]
async fn a_placeholder_delete_whose_record_fails_is_locally_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    seed_seen_placeholder(&engine, "a.txt");

    let outcome = engine.handle_delete("a.txt").await.unwrap();
    assert!(!outcome.recorded, "the nest is unreachable");
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::LocallyDeleted));
    assert!(
        engine.list_placeholder_rows().await.unwrap().is_empty(),
        "never listed back"
    );
    assert!(
        engine.provider_rows().await.unwrap().is_empty(),
        "never enumerated by a replicated provider either"
    );
    assert!(matches!(
        engine.row_presence("a.txt").unwrap(),
        RowPresence::Absent
    ));
    assert_eq!(
        engine.db().tracked_totals().unwrap(),
        (0, 0),
        "not a tracked file"
    );
}

/// (f) — the fold's two arms over an owed delete. A remote delete settles it
/// (the nest already holds the delete; nothing is owed). A remote EDIT that
/// moved the head after the offline delete wins — no data is lost: the row is
/// re-pointed as a fresh, **never-seen** placeholder, so its absence from this
/// disk is not read as the old delete all over again.
#[tokio::test]
async fn the_fold_settles_or_resurrects_an_owed_delete() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    for rel in [
        "deleted-remotely.txt",
        "edited-remotely.txt",
        "unchanged.txt",
    ] {
        seed_seen_placeholder(&engine, rel);
        engine
            .db()
            .update_state(rel, SyncState::LocallyDeleted)
            .unwrap();
    }
    let head = |rel: &str| hex::encode(head_digest(rel));
    let moved = hex::encode([0x42u8; 32]);
    let change = |seq, path: &str, manifest: Option<String>| SyncChange {
        seq,
        path_hash: format!("h-{path}"),
        manifest_hash: manifest,
        size_bytes: 5,
        change_type: "modify".into(),
        created_at: 1_700_000_000_000,
        path: Some(path.into()),
        device_id: Some("another-device".into()),
        ..Default::default()
    };
    let fold = engine
        .record_placeholders_from_changes(&[
            change(1, "deleted-remotely.txt", None),
            change(2, "edited-remotely.txt", Some(moved)),
            change(3, "unchanged.txt", Some(head("unchanged.txt"))),
        ])
        .unwrap();
    let created: Vec<&str> = fold.created.iter().map(|r| r.rel.as_str()).collect();
    assert_eq!(
        created,
        vec!["edited-remotely.txt"],
        "a resurrected row is not on this disk, and its directory may already be listed \
         — it is materialized eagerly like any other create (decision (f))"
    );

    assert_eq!(
        state(&engine, "deleted-remotely.txt"),
        None,
        "a remote delete settles an owed one"
    );
    assert_eq!(
        state(&engine, "edited-remotely.txt"),
        Some(SyncState::Placeholder),
        "the newer remote edit wins over the offline delete"
    );
    assert!(
        !seen(&engine, "edited-remotely.txt"),
        "resurrected as never-seen — not on this disk"
    );
    assert_eq!(
        state(&engine, "unchanged.txt"),
        Some(SyncState::LocallyDeleted),
        "an unchanged head leaves the owed delete owed"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The nest down at boot, then up: over a real NestClient
// ─────────────────────────────────────────────────────────────────────

/// A mocked socket that answers `fauna.sync.changes.record` with a genuine reply
/// while `up`, with an error while down, and every other kind with an error.
async fn serve_records(
    mut server: fauna_client::testing::ServerSide,
    up: Arc<AtomicBool>,
    records: Arc<AtomicI64>,
) {
    while let Some(bytes) = server.rx_from_client.recv().await {
        let Frame::Request(req) = decode_frame(&bytes).expect("decodable frame") else {
            continue;
        };
        let ok = req.kind == "fauna.sync.changes.record" && up.load(Ordering::SeqCst);
        let payload = if ok {
            let seq = records.fetch_add(1, Ordering::SeqCst) + 1;
            let bytes = fauna_core::encoding::canonical_encode(&SyncChangeRecordReply {
                seq,
                extra: Default::default(),
            })
            .unwrap();
            fauna_core::encoding::canonical_decode(&bytes).unwrap()
        } else {
            fauna_core::encoding::canonical_decode(
                &fauna_core::encoding::canonical_encode(&RpcError::new(
                    "fauna.test.nest_down",
                    "error.test.nest_down",
                ))
                .unwrap(),
            )
            .unwrap()
        };
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
}

/// **The commonest restart — the nest unreachable at boot.** A seen placeholder
/// deleted while the agent was down: the boot sweep finds it gone, its record
/// fails, and it is `LocallyDeleted` — so the root's re-armed population cannot
/// list it back (the silent revert decision (d) closes). When the nest is back,
/// the next pass records it, and only then is it tombstoned.
#[tokio::test]
async fn a_delete_owed_while_the_nest_is_down_lands_on_the_next_pass() {
    let dir = tempfile::tempdir().unwrap();
    let auth = Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret([9u8; 32]),
    ));
    let client = NestClient::with_auth(Arc::clone(&auth));
    let (slot, state_tx) = client.supervisor_channels_for_test();
    let (adapter, server) = mpsc_pair();
    let up = Arc::new(AtomicBool::new(false));
    let records = Arc::new(AtomicI64::new(0));
    let server_task = tokio::spawn(serve_records(server, up.clone(), records.clone()));
    let channel = Arc::new(MockClientChannel::new(
        connect_queue([Ok(adapter)]),
        Arc::clone(&auth),
        PushBroker::new(16),
    ));
    let supervisor = tokio::spawn(async move {
        let _ = run_supervisor(Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: state_tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        })
        .await;
    });
    let engine = test_engine_with_nest_client(dir.path().to_path_buf(), "docs", client);
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"still here");
    seed_seen_placeholder(&engine, "gone.txt");

    // Boot, nest down.
    let stats = tokio::time::timeout(Duration::from_secs(10), engine.reconcile())
        .await
        .expect("reconcile must not hang")
        .unwrap();
    assert_eq!(stats.deleted_files, 0, "the record failed");
    assert_eq!(state(&engine, "gone.txt"), Some(SyncState::LocallyDeleted));
    assert!(
        engine
            .list_placeholder_rows()
            .await
            .unwrap()
            .iter()
            .all(|r| r.rel != "gone.txt"),
        "the post-restart browse must not re-list it"
    );

    // The nest comes back: the next pass records it, and only now tombstones.
    up.store(true, Ordering::SeqCst);
    let stats = tokio::time::timeout(Duration::from_secs(10), engine.reconcile())
        .await
        .expect("reconcile must not hang")
        .unwrap();
    assert_eq!(stats.deleted_files, 1, "the owed delete lands");
    assert_eq!(records.load(Ordering::SeqCst), 1, "exactly one record");
    assert_eq!(state(&engine, "gone.txt"), Some(SyncState::Deleted));
    assert_eq!(state(&engine, "kept.txt"), Some(SyncState::Synced));

    supervisor.abort();
    server_task.abort();
}
