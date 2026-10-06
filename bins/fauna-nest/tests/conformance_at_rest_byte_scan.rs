//! tier_3: the **at-rest byte-scan proof** — after the production writers
//! run and the next boot's plaintext scrub executes, the raw
//! `nest.db` file holds **zero bytes** of any user-chosen name, path, label,
//! tag or detail, except the one still-gated `folders.name` plane (honest
//! red flag) and the ARMED `retention_policy` knobs (nest-parsed, not a
//! label).
//!
//! Proof obligation (`docs/goal/behavior/file-sync.md` § Sealed names & paths →
//! *Migration*, implementing the 2026-07-29 paths-are-content ruling in
//! `docs/goal/architecture/encryption-at-rest.md` § Carve-outs): the contract
//! step scrubs plaintext *where a sealed sibling exists*. Column-level pins for
//! that live on `scrub_plaintext_where_sealed`'s own unit tests; this file
//! greps **BYTES, not columns** — a per-column SELECT proves only the columns
//! the inventory remembered, while the byte scan catches the surface the
//! inventory forgot (S7's screen-out lesson, applied at rest). Hence:
//! file-backed `CacheDb`, `VACUUM` (the freelist keeps old row images), then a
//! substring scan over every file resting in the DB directory.
//!
//! What runs, in production shape:
//! - **Born-sealed writers** (the post-D2 wire): `fauna.sync.changes.record`
//!   with `path_sealed`, `fauna.sync.conflicts.report` with
//!   `ConflictLabels::seal`, `fauna.sync.register` re-registered keyed (the
//!   boot-time upsert D1 relies on for device-label convergence).
//! - **Keyless-writer shapes that legally still rest plaintext** (each a
//!   ratified class, each converged by the stamp-at-bind pattern + the boot
//!   scrub): `fauna.folders.update` with plaintext include/exclude and no
//!   seals (the create-gesture class), plus raw-seeded dual-write custody /
//!   import rows.
//! - **The D1 backfill pass**, via the real shared client
//!   (`FoldersClient::backfill_sealed_fields`) — the keyed stamp that
//!   licenses the boot scrub to clear the keyless plaintext above.
//! - **The boot scrub itself** (`run_scrub_plaintext` in `run_migrations`),
//!   exercised by re-opening the DB: it clears every plaintext sibling whose
//!   sealed twin now rests, and a boot that cleared anything `VACUUM`s, so no
//!   freed page keeps the old bytes.
//!
//! Asserted PRESENT after everything: `folders.name` (apps still address
//!   folders by name — the S5b hash arm has no production sender) and `folders.retention_policy`
//!   (the ARMED auto-prune ruling: the nest parses its numeric knobs
//!   server-side — `encryption-at-rest.md` § Carve-outs). A green run states,
//!   precisely: *everything the sealed planes claim stops resting has stopped
//!   resting, everything they defer is still visibly there, and the write path
//!   itself never rests it at all.*
//!
//! Out of scope, deliberately — and ⚠ the exclusion is **named per tenant**,
//! not per directory, since a screen-out is a claim about *every* occupant of
//! the region it screens (the lesson: a second tenant of
//! the blob store inherited the first tenant's exemption):
//! - **Client-sealed user content bytes** — chunks and manifests, covered by
//!   the content plane's own sealing proof; here they are generic bytes in a
//!   separate blob tempdir. This is the only tenant the exemption licenses.
//! - **The daemon's cache dir** — its artifact pin lives in
//!   `bins/fauna-sync/tests/at_rest_cache_artifacts.rs`, since the dep edge
//!   runs fauna-sync → fauna-nest.
//! - **The nest's own self-backup blobs are covered by their OWN proof
//!   below** (`the_self_backup_store_rotates_out_scrubbed_plaintext`):
//!   The window is ratified-and-bounded — plaintext
//!   a hot copy captured before the boot scrub cleared it rests in at most 24
//!   hot-copy blobs until count-based rotation replaces them. The rotation
//!   proof drives `backup_database` 24× synchronously (count-based ⇒
//!   latency-independent by construction) and asserts the scrubbed marker
//!   leaves the store while an ordinary content blob survives.
//!
//! Run: `cargo test -p fauna-nest --test conformance_at_rest_byte_scan
//! --features test-hooks`.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::label_custody::{self, LabelCustody};
use fauna_core::path_crypto::{LabelField, LabelRoot, seal_convergent};
use fauna_nest::blob_store::{BlobStoreBackend, DiskBlobStore};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::filesync::SnapshotCreateFolderRequest;
use fauna_protocol::folders::FolderUpdateRequest;
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
use fauna_protocol::{decode_strict as decode, encode_canonical};

// ── Per-family markers ────────────────────────────────────────────────────────
// One distinctive fragment per plane, never a substring of another plane's, so
// each absence/presence assert isolates exactly one surface family.

/// `folders.name`, set A — created keyless (rests), then blanked by the
/// update that stamps its seal (schema 114): absent after the boot.
const SET_A: &str = "divorce-filings-2026";
/// `folders.name`, set B — same.
const SET_B: &str = "estate-inventory-2026";
/// `sync_changes.path` (set A) — never rests (the write flip). The house
/// marker path.
const PATH_A: &str = "2026/eviction_notice.pdf";
const FRAG_PATH_A: &str = "eviction_notice";
/// Set B's file path — never rests in `sync_changes` NOR in the
/// (plaintext-column-less) `snapshot_files`.
const PATH_B: &str = "records/appraisal_villa.pdf";
const FRAG_PATH_B: &str = "appraisal_villa";
/// `folders.include_paths` — never rests: a keyless (unsealed) list is
/// refused at the wire.
const FRAG_INCLUDE: &str = "alimony-ledgers";
/// `folders.exclude_paths` — same.
const FRAG_EXCLUDE: &str = "scratch-drafts-private";
/// `folders.retention_policy` (a `keep_tags` value) — RESTS (the ARMED
/// ruling: nest-parsed knobs; a real `keep_tags` vocabulary, if ever built,
/// gets `keep_tag_hashes` + the sealed display — its slice, not this one).
const FRAG_RETENTION: &str = "custody-hearing-2026";
/// `sync_devices.label` — never rests for a user-chosen label (the write
/// flip rests `''`, the NOT NULL sentinel, keyed or not).
const DEVICE_LABEL: &str = "attic-workstation-keys";
const FRAG_LABEL: &str = "attic-workstation";
/// A snapshot tag — never rests (the nest keeps only its hash and seal).
const SNAP_TAG: &str = "insurance-claim-photos";
const FRAG_TAG: &str = "insurance-claim";
/// `sync_conflicts.path` — never rests (reports rest `''`).
const CONFLICT_PATH: &str = "2026/affidavit_draft.docx";
const FRAG_CONFLICT_PATH: &str = "affidavit_draft";
/// `sync_conflicts.details` — scrubbed. Deliberately OVERFLOW-SIZED (~15KB,
/// vs. the 4KB page): a big TEXT value lands on overflow pages, and an
/// `UPDATE … = NULL` frees those pages to the freelist with their content
/// intact (`secure_delete` is OFF — SQLite's default; `CacheDb::open` sets no
/// pragma). This is the plane that makes the `VACUUM` below load-bearing: the
/// small in-page values happen to be overwritten by the cell rewrite, but
/// freelist pages keep their bytes until VACUUM discards them — verified by
/// mutation (dropping the VACUUM reddens exactly this marker's absence
/// assert).
const FRAG_CONFLICT_DETAILS: &str = "raced on the same paragraph";

fn conflict_details() -> String {
    (0..400)
        .map(|i| format!("{FRAG_CONFLICT_DETAILS} copy {i:03}"))
        .collect::<Vec<_>>()
        .join("; ")
}
/// `backup_custody.path` — dual-write seed, scrubbed at boot. (A retained
/// generation's `path` has no sealed sibling and no scrub: the writer carries
/// only the plaintext the exempt classes rest, so a sealed set's generation
/// rests it NULL from the start.)
const FRAG_CUSTODY: &str = "deed_scan_originals";
/// `import_sessions.source_descriptor` — scrubbed; the per-source lock keys
/// on `source_hash`.
const FRAG_IMPORT: &str = "lawfirm-familia";

const OWNER_SECRET: [u8; 32] = [0xb1; 32];
/// The owner's real keypair: every record below is signed by it under the
/// set's stored nonce ([`common::SET_NONCE`]), exactly as a writer engine
/// signs — an unsigned record is refused `signature_required`.
fn actor_kp() -> fauna_core::identity::ActorKeypair {
    common::signing_actor(0x71)
}

/// The owner's actor id (its key's public half).
fn actor() -> [u8; 32] {
    actor_kp().actor_id().0
}
const DEVICE: [u8; 32] = [0x0a; 32];

fn owner_key() -> BackupKey {
    BackupKey::from_bytes(OWNER_SECRET)
}

fn owner_root() -> LabelRoot {
    LabelRoot::owner_of(&owner_key())
}

/// Seal `path` the way the keyed engine's funnel does — convergent, salted by
/// the path's own hash, tagged `SyncChangePath` (the one path-plane tag).
fn owner_seal(path: &str) -> Vec<u8> {
    seal_convergent(
        &owner_root(),
        &fauna_core::sync::path_hash(path),
        LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

/// An `RpcRequester` dispatching straight into the registered handlers
/// (`conformance_path_sealing.rs`'s shape), so the shared production clients
/// run against real replies with no transport in between.
struct HandlerRequester {
    state: Arc<AppState>,
    actor: [u8; 32],
}

impl fauna_protocol::RpcRequester for HandlerRequester {
    type Error = String;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, String>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = encode_canonical(&payload).map_err(|e| e.to_string())?;
        let out = dispatch(&self.state, self.actor, kind, Bytes::from(bytes.to_vec())).await;
        decode(&out).map_err(|e| e.to_string())
    }
}

async fn dispatch(state: &Arc<AppState>, actor: [u8; 32], kind: &str, payload: Bytes) -> Bytes {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = state.rpc_router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state.clone(), actor, payload)
        .await
        .unwrap_or_else(|e| panic!("{kind} handler ok, got {e:?}"))
}

async fn record(
    state: &Arc<AppState>,
    folder: &str,
    device_hex: &str,
    path: &str,
    manifest_hash: ContentHash,
) -> i64 {
    let req = SyncChangeRecordRequest {
        folder: folder.to_string(),
        device_id: device_hex.to_string(),
        path: path.to_string(),
        manifest_hash: Some(hex::encode(manifest_hash.digest())),
        size_bytes: 0,
        change_type: "create".to_string(),
        path_sealed: Some(fauna_protocol::ByteBuf::from(owner_seal(path))),
        ..Default::default()
    };
    let req = common::signed_record(req, &actor_kp());
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply: SyncChangeRecordReply =
        decode(&dispatch(state, actor(), "fauna.sync.changes.record", payload).await).unwrap();
    reply.seq
}

/// Every regular file resting in the DB directory, concatenated — the journal
/// (or a WAL, should the pragma ever change) is part of "at rest" too.
fn resting_bytes(dir: &std::path::Path) -> Vec<u8> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    let mut all = Vec::new();
    for p in names {
        all.extend(std::fs::read(&p).unwrap());
    }
    all
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    !n.is_empty() && haystack.windows(n.len()).any(|w| w == n)
}

#[tokio::test]
async fn no_sealed_plane_plaintext_ever_rests() {
    // ── A real on-disk DB (`nat_mode_api.rs`'s harness shape).
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir.path().join("nest.db");
    let db = Arc::new(CacheDb::open(&db_path).unwrap());
    let blob_tmp = tempfile::tempdir().unwrap();
    let store = DiskBlobStore::new(blob_tmp.path()).unwrap();

    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::filesync_handlers::register_filesync_handlers(&mut b);
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            ..AppState::for_test(db.clone())
        })
    };
    let requester = || HandlerRequester {
        state: Arc::clone(&state),
        actor: actor(),
    };
    let device_hex = hex::encode(DEVICE);

    // Both sets are born with `name_sealed = NULL` — the shape a custody-less
    // client's create leaves (the keyed `create_set` seals at birth) — so the
    // D1 backfill stamps it.
    // Each is born under the client-minted set nonce the records sign under.
    let with_nonce = || fauna_nest::db::FolderOptions {
        set_nonce: Some(common::SET_NONCE.to_vec()),
        ..Default::default()
    };
    let _fs_a = db
        .create_folder_with_options(SET_A, &actor(), with_nonce())
        .await
        .unwrap();
    let fs_b = db
        .create_folder_with_options(SET_B, &actor(), with_nonce())
        .await
        .unwrap();

    // ── Production writers. ──────────────────────────────────────────────────
    // Device label, twice: the keyless shape then the keyed re-register.
    // BOTH rest `''` now — a user-chosen label never rests plaintext again.
    let sync_client = fauna_client_sync::SyncClient::new(requester());
    sync_client
        .register(&device_hex, DEVICE_LABEL, None)
        .await
        .unwrap();
    let label_sealed = label_custody::seal_device_label(&owner_root(), &DEVICE, DEVICE_LABEL)
        .unwrap()
        .expect("a user-chosen label seals");
    sync_client
        .register(&device_hex, DEVICE_LABEL, Some(label_sealed))
        .await
        .unwrap();

    // The path plane: sealed records rest hash + seal, plaintext NULL.
    let m_a = common::put_manifest(&store, &db, b"generic body bytes A").await;
    record(&state, SET_A, &device_hex, PATH_A, m_a).await;
    let m_b = common::put_manifest(&store, &db, b"generic body bytes B").await;
    record(&state, SET_B, &device_hex, PATH_B, m_b).await;

    // A conflict, labels minted through the S6-a client funnel — rests the
    // scrub sentinel + the sealed pair, never the plaintext.
    let details = conflict_details();
    let labels =
        fauna_client_sync::ConflictLabels::seal(Some(&owner_root()), CONFLICT_PATH, Some(&details))
            .unwrap();
    sync_client
        .conflicts_report(
            SET_A,
            device_hex.clone(),
            CONFLICT_PATH,
            "divergent",
            Some(details),
            Vec::new(),
            labels,
        )
        .await
        .unwrap();

    // Selective-sync lists in the KEYLESS shape are refused at the wire (paths
    // are content): the write-path proof below then finds neither fragment.
    let keyless_paths = FolderUpdateRequest {
        name: SET_A.to_string(),
        include_paths: Some(vec![format!("Documents/{FRAG_INCLUDE}")]),
        exclude_paths: Some(vec![format!("Documents/{FRAG_EXCLUDE}")]),
        ..Default::default()
    };
    common::seed_dispatch_actor(&state.db, &actor()).await;
    let refused = (state
        .rpc_router
        .kind_meta("fauna.folders.update")
        .expect("kind registered")
        .handler)(
        state.clone(),
        actor(),
        Bytes::from(encode_canonical(&keyless_paths).unwrap().to_vec()),
    )
    .await
    .expect_err("an unsealed path list is refused");
    assert_eq!(refused.code, "fauna.folders.invalid_request");

    // Retention through the update wire in the KEYLESS shape — the ARMED plane
    // that legally rests plaintext (the nest parses its knobs).
    dispatch(
        &state,
        actor(),
        "fauna.folders.update",
        Bytes::from(
            encode_canonical(&FolderUpdateRequest {
                name: SET_A.to_string(),
                retention_policy: Some(format!("{{\"keep_tags\":[\"{FRAG_RETENTION}\"]}}")),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await;

    // A snapshot with plaintext tags on the wire and no seal: the nest
    // computes `tag_hashes` from the wire plaintext and rests nothing else.
    dispatch(
        &state,
        actor(),
        "fauna.filesync.snapshot.create_folder",
        Bytes::from(
            encode_canonical(&SnapshotCreateFolderRequest {
                folder: SET_B.to_string(),
                tags: vec![SNAP_TAG.to_string()],
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await;

    // Custody + import rows seeded raw with plaintext beside their seal — the
    // dual-write shape the boot scrub converges.
    {
        let conn = db.conn().await;
        conn.execute(
            "INSERT INTO backup_custody (uploader_actor, folder_id, path_hash, size_bytes,
                    updated_at, path, path_sealed)
             VALUES (?1, ?2, ?3, 1, 1, ?4, ?5)",
            rusqlite::params![
                &actor()[..],
                fs_b,
                &fauna_core::sync::path_hash(&format!("vault/{FRAG_CUSTODY}.tiff"))[..],
                format!("vault/{FRAG_CUSTODY}.tiff"),
                owner_seal(&format!("vault/{FRAG_CUSTODY}.tiff")),
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO import_sessions (session_id, actor_id, source_descriptor, state,
                    started_at, last_progress_at, expires_at, source_sealed)
             VALUES ('s1', ?1, ?2, 'completed', 1, 1, 9999999999, x'33')",
            rusqlite::params![
                &actor()[..],
                format!("imap://mail.{FRAG_IMPORT}.test/INBOX")
            ],
        )
        .unwrap();
    }

    // ── THE WRITE-PATH PROOF: before any scrub or backfill runs, the sealed
    // planes never rested plaintext at all. This is strictly stronger than a
    // scrub-then-check — the plaintext must never have touched the file. (`flush` moves WAL/journal content into the main file
    // so the scan sees everything ever written.)
    db.flush().await.unwrap();
    let after_writers = resting_bytes(data_dir.path());
    for (frag, plane) in [
        (FRAG_PATH_A, "sync_changes.path"),
        (FRAG_PATH_B, "sync_changes.path + snapshot_files"),
        (FRAG_INCLUDE, "folders.include_paths (refused keyless)"),
        (FRAG_EXCLUDE, "folders.exclude_paths (refused keyless)"),
        (FRAG_LABEL, "sync_devices.label"),
        (FRAG_TAG, "snapshots.tags"),
        (FRAG_CONFLICT_PATH, "sync_conflicts.path"),
        (FRAG_CONFLICT_DETAILS, "sync_conflicts.details"),
    ] {
        assert!(
            !contains(&after_writers, frag),
            "write path: {plane} marker {frag:?} rested in the raw file — a \
             production writer still rests plaintext"
        );
    }
    // Positive controls — the keyless planes DO rest, so the scan is
    // provably able to see resting plaintext (the vacuous-pin lesson).
    for (frag, plane) in [
        (SET_A, "folders.name A"),
        (SET_B, "folders.name B"),
        (FRAG_RETENTION, "folders.retention_policy (ARMED)"),
        (FRAG_CUSTODY, "backup_custody.path (dual-write seed)"),
        (
            FRAG_IMPORT,
            "import_sessions.source_descriptor (dual-write seed)",
        ),
    ] {
        assert!(
            contains(&after_writers, frag),
            "positive control: {plane} marker {frag:?} must rest before the reconcile"
        );
    }

    // ── D1: the keyed backfill stamps the seals that license the boot scrub. ─
    let custody = LabelCustody::owner_only(owner_key());
    let folders =
        fauna_client_folders::FoldersClient::new(requester()).with_label_custody(custody.clone());
    let report = folders.backfill_sealed_fields().await.unwrap();
    assert_eq!(report.names, 2, "both sets' name_sealed stamped");
    assert_eq!(report.selective_sync, 0, "no list rests unsealed to stamp");
    assert_eq!(
        report.retention, 1,
        "set A's retention display copy stamped"
    );
    assert_eq!(report.bound_skipped, 0);

    // ── The next boot: re-open, so the scrub (and the VACUUM a clearing
    // scrub owes) runs before serving.
    db.flush().await.unwrap();
    drop(state);
    drop(sync_client);
    drop(folders);
    drop(db);
    // Held open for the scan: the open IS the boot under test.
    let _db = CacheDb::open(&db_path).unwrap();

    // ── The proof: nothing scrubbed rests anywhere in the DB directory. ──────
    let after = resting_bytes(data_dir.path());
    for (frag, plane) in [
        (SET_A, "folders.name A"),
        (SET_B, "folders.name B"),
        (FRAG_PATH_A, "sync_changes.path"),
        (FRAG_PATH_B, "sync_changes.path + snapshot_files"),
        (FRAG_INCLUDE, "folders.include_paths"),
        (FRAG_EXCLUDE, "folders.exclude_paths"),
        (FRAG_LABEL, "sync_devices.label"),
        (FRAG_TAG, "snapshots.tags"),
        (FRAG_CONFLICT_PATH, "sync_conflicts.path"),
        (FRAG_CONFLICT_DETAILS, "sync_conflicts.details"),
        (FRAG_CUSTODY, "backup_custody.path"),
        (FRAG_IMPORT, "import_sessions.source_descriptor"),
    ] {
        assert!(
            !contains(&after, frag),
            "{plane}: marker {frag:?} still rests in the raw DB file after the \
             boot scrub (scrub + VACUUM)"
        );
    }

    // The one plane that legally still rests, asserted PRESENT — an honest
    // flag, not a skip.
    let (frag, why) = (
        FRAG_RETENTION,
        "folders.retention_policy — ARMED: nest-parsed knobs, not a label",
    );
    assert!(
        contains(&after, frag),
        "expected-resting plane vanished ({why}): {frag:?} — if a slice \
         legitimately sealed it, move it to the absence list above"
    );
}

/// Requirement 3: the self-backup store's plaintext
/// window closes under **count-based** rotation (latency-independent by
/// construction — `testing.md` § point 14): a hot-copy taken BEFORE the boot
/// scrub carries the resting plaintext; after 24 post-scrub `backup_database` cycles
/// (`KEEP_SQLITE_BACKUPS`) it has rotated out of the store, while an ordinary
/// content blob in the same store survives. The 23rd-cycle assert is the
/// positive control that the scan can see the marker at all — withholding
/// rotation (the named mutation) keeps it visible and reddens the final
/// absence assert.
#[tokio::test]
async fn the_self_backup_store_rotates_out_scrubbed_plaintext() {
    let data_dir = tempfile::tempdir().unwrap();
    let db_path = data_dir.path().join("nest.db");
    let blob_root = tempfile::tempdir().unwrap();
    let db = Arc::new(CacheDb::open(&db_path).unwrap());

    // An ordinary user-content blob sharing the store — rotation must not
    // touch it.
    let store = DiskBlobStore::new(blob_root.path()).unwrap();
    let content = b"ordinary user content blob: villa_floorplan_bytes";
    let content_hash = ContentHash::of_raw(content);
    store.put(&content_hash, content).await.unwrap();

    // A row resting plaintext beside its seal — the state the next boot
    // scrubs — captured into hot-copy #0.
    let fs = db.create_folder("rotation-proof", &actor()).await.unwrap();
    {
        let conn = db.conn().await;
        conn.execute(
            "INSERT INTO sync_changes (actor_id, path_hash, change_type, created_at,
                    folder_id, path, path_sealed)
             VALUES (?1, ?2, 'create', 1, ?3, 'attic/eviction_notice.pdf', x'AA')",
            rusqlite::params![&actor()[..], &[1u8; 32][..], fs],
        )
        .unwrap();
    }
    db.flush().await.unwrap();
    // Production self-backup shape: NO encryption key — the blobs rest in the
    // clear, which is exactly why this requirement exists.
    let svc = fauna_nest::backup::service::BackupService::new(
        db.clone(),
        None,
        false,
        blob_root.path().to_path_buf(),
        Some(db_path.clone()),
    )
    .unwrap();
    svc.backup_database().await.unwrap();
    let store_bytes = resting_bytes_recursive(blob_root.path());
    assert!(
        contains(&store_bytes, "eviction_notice"),
        "positive control: hot-copy #0 carries the plaintext"
    );

    // The next boot: reopen — scrub + VACUUM; the live DB is clean, the old
    // hot-copy still is not.
    drop(svc);
    drop(db);
    let db = Arc::new(CacheDb::open(&db_path).unwrap());
    let svc = fauna_nest::backup::service::BackupService::new(
        db.clone(),
        None,
        false,
        blob_root.path().to_path_buf(),
        Some(db_path.clone()),
    )
    .unwrap();

    // 24 post-scrub cycles. Each iteration perturbs one row so every copy is
    // byte-distinct (content-addressed store: identical copies would share a
    // hash and rotation accounting would not model production, where every
    // hourly copy differs).
    for i in 0..fauna_nest::backup::service::KEEP_SQLITE_BACKUPS {
        {
            let conn = db.conn().await;
            conn.execute(
                "INSERT OR REPLACE INTO sync_devices (actor_id, device_id, label,
                        registered_at, last_seen)
                 VALUES (?1, ?2, '', 1, ?3)",
                rusqlite::params![&actor()[..], &[9u8; 32][..], i as i64],
            )
            .unwrap();
        }
        db.flush().await.unwrap();
        if i == fauna_nest::backup::service::KEEP_SQLITE_BACKUPS - 1 {
            // Positive control at 23 of 24: the pre-scrub copy is the oldest of
            // exactly KEEP retained copies — still in the store, still seen.
            let bytes = resting_bytes_recursive(blob_root.path());
            assert!(
                contains(&bytes, "eviction_notice"),
                "positive control: the pre-scrub copy must still rest until the \
                 24th rotation replaces it (a scan that cannot see it would \
                 green-wash the absence assert below)"
            );
        }
        svc.backup_database().await.unwrap();
    }

    let bytes = resting_bytes_recursive(blob_root.path());
    assert!(
        !contains(&bytes, "eviction_notice"),
        "req 3: after {} post-scrub backup cycles the scrubbed \
         plaintext must have rotated out of the self-backup store",
        fauna_nest::backup::service::KEEP_SQLITE_BACKUPS
    );
    assert!(
        store.get(&content_hash).await.unwrap().is_some(),
        "rotation must never touch an ordinary user-content blob"
    );
}

/// Every regular file under `dir`, recursively — the self-backup store nests
/// blobs in fan-out subdirectories.
fn resting_bytes_recursive(dir: &std::path::Path) -> Vec<u8> {
    let mut all = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                all.extend(std::fs::read(&p).unwrap());
            }
        }
    }
    all
}
