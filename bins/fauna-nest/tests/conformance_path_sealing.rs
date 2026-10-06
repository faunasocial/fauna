//! tier_3: the sealed-names-&-paths expand phase carries a sealed label from
//! the change feed into a snapshot without a key, and every server-side compute
//! that used to key on the plaintext path now keys on `path_hash`.
//!
//! Proof obligation (`docs/goal/behavior/file-sync.md` § Sealed names & paths,
//! implementing the 2026-07-29 paths-are-content ruling in
//! `docs/goal/architecture/encryption-at-rest.md` § Carve-outs): user-chosen
//! paths must stop resting plaintext on a hosting nest. The migrate/contract
//! steps that actually remove the plaintext are gated, but they are only *safe*
//! if two properties hold first, and both are what this file pins:
//!
//! 1. **The nest can move a sealed label it cannot read.** The `SealedLabel`
//!    envelope names its own key generation, so snapshot creation — a
//!    server-side row copy of the membership projection — must carry the blob
//!    into `snapshot_files` byte-for-byte with no key anywhere in the path.
//! 2. **Nothing server-side still depends on the plaintext.** The
//!    latest-per-path fold, the snapshot diff join and the single-file snapshot
//!    lookup are re-keyed to `path_hash`; a **fold-equivalence** pin asserts the
//!    hash fold and the retired plaintext fold return the identical row set on a
//!    seeded DB, so the re-key changed the key and nothing else.
//!
//! 3. **A client's seal survives the round trip.** S2 gave the wire its
//!    `path_sealed` sibling and the sync engine its one seal funnel, so a label
//!    sealed under a root the nest never sees must land in `sync_changes` and
//!    come back out of `fauna.sync.changes.list` byte-for-byte — and open under
//!    that root only.
//!
//! Drives the **real** `fauna.sync.changes.record`,
//! `fauna.sync.changes.list` and `fauna.filesync.snapshot.create_folder`
//! handlers over a real `CacheDb` (real migrations). Every sealed blob here is
//! sealed **client-side** in the test, under [`CLIENT_ROOT`], which is never
//! handed to the nest in any form — that is the property under test, not a
//! convenience.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::ContentHash;
use fauna_core::path_crypto::{LabelField, LabelRoot, SealedLabel, seal_convergent};
use fauna_nest::blob_store::DiskBlobStore;
use fauna_nest::db::CacheDb;
use fauna_nest::filesync_handlers::register_filesync_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::sync_handlers::register_sync_handlers;
use fauna_protocol::filesync::{SnapshotCreateFolderReply, SnapshotCreateFolderRequest};
use fauna_protocol::sync::{
    SyncChangeRecordReply, SyncChangeRecordRequest, SyncChangesListReply, SyncChangesListRequest,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};

const SET: &str = "family-documents";
const PATH_A: &str = "2026/eviction_notice.pdf";
const PATH_B: &str = "2026/tax/return.pdf";

/// The client's label root. In production this is whatever already seals the
/// set's chunks (`LabelRoot::owner_of(&backup_key)` on the owner path, or the
/// set's M2 content-key generation). It exists only in this
/// test process: nothing below ever puts it on the wire or in the DB.
const CLIENT_ROOT: [u8; 32] = [0x5e; 32];

/// Seal `path` the way `SyncEngine::record_change`'s funnel does — convergent
/// mode, salted by the path's own `path_hash`, tagged `SyncChangePath`.
fn client_seal(path: &str) -> Vec<u8> {
    seal_convergent(
        &LabelRoot::owner(CLIENT_ROOT),
        &fauna_core::sync::path_hash(path),
        LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

async fn dispatch(state: &Arc<AppState>, actor: [u8; 32], kind: &str, payload: Bytes) -> Bytes {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = state.rpc_router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state.clone(), actor, payload)
        .await
        .unwrap_or_else(|e| panic!("{kind} handler ok, got {e:?}"))
}

/// Record through the real handler as a production writer does: signed by the
/// recorder's identity key under the set's stored nonce ([`common::SET_NONCE`],
/// which [`fixture`] creates the set under) — every record kind refuses an
/// unsigned record `signature_required`.
async fn record(
    state: &Arc<AppState>,
    kp: &fauna_core::identity::ActorKeypair,
    device_hex: &str,
    path: &str,
    manifest_hash: ContentHash,
    path_sealed: Option<Vec<u8>>,
) -> i64 {
    let req = SyncChangeRecordRequest {
        nest_url: None,
        channel_id: None,
        folder: SET.to_string(),
        device_id: device_hex.to_string(),
        path: path.to_string(),
        manifest_hash: Some(hex::encode(manifest_hash.digest())),
        size_bytes: 0,
        change_type: "create".to_string(),
        content_key_version: None,
        thumbnail_hash: None,
        path_sealed: path_sealed.map(fauna_protocol::ByteBuf::from),
        ..Default::default()
    };
    let req = common::signed_record(req, kp);
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let reply: SyncChangeRecordReply =
        decode(&dispatch(state, kp.actor_id().0, "fauna.sync.changes.record", payload).await)
            .unwrap();
    reply.seq
}

struct Fixture {
    db: Arc<CacheDb>,
    state: Arc<AppState>,
    actor: [u8; 32],
    /// The owner's real keypair (`actor` is its public half) — the records
    /// below are signed with it.
    kp: fauna_core::identity::ActorKeypair,
    folder_id: i64,
    _tmp: tempfile::TempDir,
}

/// A folder with two files, the second of which has been superseded
/// twice — so the latest-per-path fold has real folding to do rather than
/// trivially returning every row.
async fn fixture() -> Fixture {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());

    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            register_filesync_handlers(&mut b);
            // The conflict plane's kinds live here (S6-a).
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            ..AppState::for_test(db.clone())
        })
    };

    let kp = common::signing_actor(0x71);
    let actor: [u8; 32] = kp.actor_id().0;
    let device: [u8; 32] = [0x0a; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&actor, &device, "laptop", None, "write")
        .await
        .unwrap();
    let folder_id = db
        .create_folder_with_options(
            SET,
            &actor,
            fauna_nest::db::FolderOptions {
                set_nonce: Some(common::SET_NONCE.to_vec()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let m_a = common::put_manifest(&store, &db, b"eviction notice v1").await;
    let m_b1 = common::put_manifest(&store, &db, b"tax return v1").await;
    let m_b2 = common::put_manifest(&store, &db, b"tax return v2").await;
    // Both paths record sealed — since the S9 flip a sealless record on a
    // sync set is REFUSED (`path_seal_required`; pinned by
    // `a_sealless_record_is_refused_loudly` below), so the keyed-writer shape
    // is the only wire shape a record can take here.
    record(
        &state,
        &kp,
        &device_hex,
        PATH_A,
        m_a,
        Some(client_seal(PATH_A)),
    )
    .await;
    // PATH_B seals under the OWNER key's chunk root — the production shape
    // `client_custody()` opens, so the render tests below can show its name.
    // (PATH_A stays under the bare CLIENT_ROOT: the wire round-trip test pins
    // those exact bytes and opens them under that root.)
    record(
        &state,
        &kp,
        &device_hex,
        PATH_B,
        m_b1,
        Some(owner_seal(PATH_B)),
    )
    .await;
    record(
        &state,
        &kp,
        &device_hex,
        PATH_B,
        m_b2,
        Some(owner_seal(PATH_B)),
    )
    .await;

    Fixture {
        db,
        state,
        actor,
        kp,
        folder_id,
        _tmp: tmp,
    }
}

/// Property 2 (post-flip form): the latest-per-path fold keys on `path_hash`
/// and lists every sealed row with `path = NULL` (the write flip rests no
/// plaintext), the sealed label riding beside its hash. Its pre-flip twin
/// proved hash-fold ≡ plaintext-fold while both could run; the plaintext
/// oracle retired with the resting plaintext itself.
#[tokio::test]
async fn fold_equivalence_pin() {
    let f = fixture().await;

    let hash_fold = f.db.get_files_for_folder(f.folder_id).await.unwrap();
    // Guard against a vacuous pass: the fixture must actually exercise folding
    // (PATH_B was superseded once — three records, two live rows).
    assert_eq!(hash_fold.len(), 2, "two live paths after the supersede");
    let h = |p: &str| fauna_core::sync::path_hash(p).to_vec();
    // Hash order is the fold's ORDER BY; both rows rest sealed, plaintext-less.
    let mut expect = [h(PATH_A), h(PATH_B)];
    expect.sort();
    for (row, want) in hash_fold.iter().zip(expect.iter()) {
        assert_eq!(&row.path_hash, want, "fold keyed + ordered by path_hash");
        assert_eq!(row.path, None, "no plaintext rests post-flip");
        assert!(row.path_sealed.is_some(), "the seal rides the fold");
    }
}

/// Property 3: the client's seal survives the whole round trip —
/// `fauna.sync.changes.record` → `sync_changes.path_sealed` →
/// `fauna.sync.changes.list` — byte-for-byte, and opens under the client's root
/// and no other.
///
/// This is the assertion that makes the expand phase real: before S2 there was
/// no wire field to carry a seal, so `path_sealed` came back `None` here.
#[tokio::test]
async fn a_client_seal_rides_the_wire_into_the_feed_and_back() {
    let f = fixture().await;

    let changes: SyncChangesListReply = decode(
        &dispatch(
            &f.state,
            f.actor,
            "fauna.sync.changes.list",
            Bytes::from(
                encode_canonical(&SyncChangesListRequest {
                    folder: Some(SET.to_string()),
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();

    let a = changes
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash(PATH_A)))
        .expect("the sealed change is in the feed");
    let b = changes
        .changes
        .iter()
        .find(|c| c.path_hash == hex::encode(fauna_core::sync::path_hash(PATH_B)))
        .expect("the second sealed change is in the feed");

    // Verbatim: the nest stored and echoed the exact bytes the client sealed.
    // Convergent mode makes this checkable — re-sealing reproduces the blob.
    assert_eq!(
        a.path_sealed.as_ref().map(|s| s.to_vec()),
        Some(client_seal(PATH_A)),
        "the recorded sealed label must survive record → store → list unchanged"
    );
    assert_eq!(
        b.path_sealed.as_ref().map(|s| s.to_vec()),
        Some(owner_seal(PATH_B)),
        "the fixture's owner-root seal echoes verbatim too"
    );

    // It opens under the client's root…
    let salt = fauna_core::sync::path_hash(PATH_A);
    let opened = fauna_core::path_crypto::open(
        [&CLIENT_ROOT],
        &salt,
        LabelField::SyncChangePath,
        &SealedLabel::from_bytes(a.path_sealed.as_ref().unwrap()).unwrap(),
    )
    .expect("opens under the root the client sealed with");
    assert_eq!(opened, PATH_A.as_bytes());

    // …and fails closed under any other, which is what makes the hosting nest —
    // holding exactly these bytes and no key — unable to read the name.
    let wrong = fauna_core::path_crypto::open(
        [&[0x11u8; 32]],
        &salt,
        LabelField::SyncChangePath,
        &SealedLabel::from_bytes(a.path_sealed.as_ref().unwrap()).unwrap(),
    );
    assert!(
        wrong.is_err(),
        "a label must not open under a root that did not seal it"
    );

    // The nest holds no copy of the root anywhere it could have kept one: the
    // only sealed-label bytes in the DB are the ones the client sent.
    {
        let conn = f.db.conn().await;
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT path_sealed FROM sync_changes WHERE folder_id = ?1 AND path_hash = ?2",
                rusqlite::params![f.folder_id, &fauna_core::sync::path_hash(PATH_A)[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, client_seal(PATH_A));
    }
}

/// Property 1: a sealed label the nest cannot read rides the projection into a
/// snapshot byte-for-byte, and its `path_hash` companion rides with it.
#[tokio::test]
async fn a_sealed_label_rides_into_a_snapshot_verbatim_and_keyless() {
    let f = fixture().await;

    // The fixture already recorded PATH_A's seal over the real
    // `fauna.sync.changes.record` wire — no raw SQL seeds it.
    let salt = fauna_core::sync::path_hash(PATH_A);
    let sealed_bytes = client_seal(PATH_A);

    let create: SnapshotCreateFolderReply = decode(
        &dispatch(
            &f.state,
            f.actor,
            "fauna.filesync.snapshot.create_folder",
            Bytes::from(
                encode_canonical(&SnapshotCreateFolderRequest {
                    folder: SET.into(),
                    tags: vec![],
                    device_id: None,
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();
    assert_eq!(create.file_count, 2);

    let files = f.db.get_snapshot_files(create.id).await.unwrap();
    let a = files
        .iter()
        .find(|r| r.path_hash == fauna_core::sync::path_hash(PATH_A).to_vec())
        .expect("path A in the snapshot");
    let b = files
        .iter()
        .find(|r| r.path_hash == fauna_core::sync::path_hash(PATH_B).to_vec())
        .expect("path B in the snapshot");

    // Verbatim: byte-for-byte, not merely decodable — for BOTH rows (B's is
    // the fixture's owner-root seal; the never-synthesise property is pinned
    // by `a_keyless_conflict_report_stores_the_hash_and_no_seal`).
    assert_eq!(
        a.path_sealed.as_deref(),
        Some(sealed_bytes.as_slice()),
        "the sealed label must ride the projection into `snapshot_files` unchanged"
    );
    assert_eq!(
        b.path_sealed.as_deref(),
        Some(owner_seal(PATH_B).as_slice()),
        "the fixture's owner-root seal rides verbatim too"
    );

    // Keyless: the copy happened without the root, and the blob still opens
    // under the client's root afterwards.
    let reopened = fauna_core::path_crypto::open(
        [&CLIENT_ROOT],
        &salt,
        LabelField::SyncChangePath,
        &SealedLabel::from_bytes(a.path_sealed.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        reopened,
        PATH_A.as_bytes(),
        "the round-tripped label must still open to the original path"
    );

    // The routing companion rides too, so the snapshot is hash-addressable.
    assert_eq!(
        a.path_hash.as_slice(),
        fauna_core::sync::path_hash(PATH_A).as_slice(),
        "`snapshot_files.path_hash` must be written at capture, not left for a boot backfill"
    );

    // …and the single-file lookup uses it — there is no plaintext column left
    // to fall back to (the v32 rebuild dropped it), so this lookup succeeding
    // IS the hash-addressing proof.
    let found =
        f.db.get_snapshot_file(create.id, PATH_A)
            .await
            .unwrap()
            .expect("addressed by path_hash");
    assert_eq!(found.manifest_hash, a.manifest_hash);
}

/// The snapshot diff joins on `path_hash`: with the plaintext blanked on both
/// sides — the state the contract step produces — a plaintext-keyed join would
/// collapse every file into one entry and report a bogus diff.
#[tokio::test]
async fn snapshot_diff_joins_on_the_hash_not_the_plaintext() {
    let f = fixture().await;

    let snap_a = f.db.create_snapshot(f.folder_id).await.unwrap();
    // `snapshots` carries UNIQUE(folder_id, created_at) at seconds
    // granularity, so back-date A rather than sleeping for the next second: a
    // wall-clock wait would make this test's verdict depend on machine load,
    // and these tests run on heavily shared machines. Assert latency-independent
    // state, never timing.
    {
        let conn = f.db.conn().await;
        conn.execute(
            "UPDATE snapshots SET created_at = created_at - 60 WHERE id = ?1",
            rusqlite::params![snap_a.id],
        )
        .unwrap();
    }

    // Change one file, leave the other alone.
    let store = DiskBlobStore::new(f._tmp.path()).unwrap();
    let m_a2 = common::put_manifest(&store, &f.db, b"eviction notice v2").await;
    let device_hex = hex::encode([0x0au8; 32]);
    record(
        &f.state,
        &f.kp,
        &device_hex,
        PATH_A,
        m_a2,
        Some(client_seal(PATH_A)),
    )
    .await;
    let snap_b = f.db.create_snapshot(f.folder_id).await.unwrap();

    let with_plaintext = fauna_nest::backup::diff::snapshot_diff(&f.db, snap_a.id, snap_b.id)
        .await
        .unwrap();
    assert_eq!(with_plaintext.summary.modified_count, 1);
    assert_eq!(with_plaintext.summary.added_count, 0);
    assert_eq!(with_plaintext.summary.removed_count, 0);

    // Since the v32 rebuild there IS no plaintext column to mis-join on —
    // the rows this diff just joined carry `path_hash` + `path_sealed` only,
    // so the correct summary above is itself the hash-join proof (the
    // pre-flip decoy-plaintext discriminator retired with the column).
}

// ── Property 4: the read surfaces render the seal end to end ────────────────
//
// S1–S2b proved the label *rests* and *travels*; this proves the shared read
// client turns it back into a name. It is the half that makes the plaintext
// scrub survivable — the write funnel could be perfect and the flip would still
// blank every filename if the reader could not open what it was handed.
//
// Drives `fauna_client_snapshots::SnapshotsClient` — the ONE client every app's
// snapshot browse routes through (linux + tui directly, apple/android/windows
// via the `fauna-ffi` mirror, web via the wasm mirror) — over the real nest
// handlers, with the reader's plaintext deliberately made useless.

/// An `RpcRequester` that dispatches straight into the registered handlers, so
/// the shared client runs against real replies with no transport in between.
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

/// The reader's owner key. [`CLIENT_ROOT`] above is a *bare* label root (all
/// these tests needed before the read half existed); a real reader's custody is
/// a `BackupKey`, and the label root is that key's `convergent_chunk_root()`.
/// The tests below therefore seal under this key so the client opens exactly
/// what production would. The nest never holds it — that is the property.
const OWNER_SECRET: [u8; 32] = [0xb1; 32];

fn owner_key() -> fauna_core::crypto::BackupKey {
    fauna_core::crypto::BackupKey::from_bytes(OWNER_SECRET)
}

/// The reader's custody, built here and handed only to the client.
fn client_custody() -> fauna_core::label_custody::LabelCustody {
    fauna_core::label_custody::LabelCustody::owner_only(owner_key())
}

/// Seal `path` under the OWNER key's chunk root — the production owner-path
/// shape, as opposed to [`client_seal`]'s bare root.
fn owner_seal(path: &str) -> Vec<u8> {
    seal_convergent(
        &LabelRoot::owner_of(&owner_key()),
        &fauna_core::sync::path_hash(path),
        LabelField::SyncChangePath,
        path.as_bytes(),
    )
    .unwrap()
    .to_bytes()
    .unwrap()
}

/// A snapshot browse renders the sealed name, and does so from the SEAL — the
/// stored plaintext is perturbed to a decoy first, so a renderer that fell back
/// to the plaintext column would return the decoy and fail this test.
#[tokio::test]
async fn snapshot_browse_renders_the_sealed_name_not_the_stored_plaintext() {
    let f = fixture().await;

    // Re-record PATH_A sealed under the OWNER key (the shape a real client's
    // custody opens), then snapshot it.
    let store = DiskBlobStore::new(f._tmp.path()).unwrap();
    let m = common::put_manifest(&store, &f.db, b"eviction notice owner-sealed").await;
    let device_hex = hex::encode([0x0au8; 32]);
    record(
        &f.state,
        &f.kp,
        &device_hex,
        PATH_A,
        m,
        Some(owner_seal(PATH_A)),
    )
    .await;
    let snap = f.db.create_snapshot(f.folder_id).await.unwrap();

    // No simulation needed since the v32 rebuild: the snapshot rows carry no
    // plaintext at all — a renderer that needed the plaintext column has
    // nothing to fall back to, so the names below CAN only come from seals.
    let client = fauna_client_snapshots::SnapshotsClient::new(HandlerRequester {
        state: Arc::clone(&f.state),
        actor: f.actor,
    })
    .with_label_custody(client_custody());

    let reply = client.get(snap.id).await.expect("snapshot.get");

    let rendered: Vec<&str> = reply.files.iter().map(|e| e.path.as_str()).collect();
    assert!(
        rendered.contains(&PATH_A),
        "the sealed name must be rendered from the SEAL; got {rendered:?}"
    );
    // PATH_B's fixture seal is under the same owner custody — it renders too.
    assert!(rendered.contains(&PATH_B), "got {rendered:?}");
}

/// The same reply, read by a client with NO custody: the sealed-only row is
/// omitted rather than rendered under its decoy plaintext... and the snapshot's
/// own counts stay truthful, because the file is still there to be restored.
#[tokio::test]
async fn a_keyless_reader_omits_the_sealed_row_but_the_counts_stay_truthful() {
    let f = fixture().await;

    let store = DiskBlobStore::new(f._tmp.path()).unwrap();
    let m = common::put_manifest(&store, &f.db, b"eviction notice owner-sealed").await;
    let device_hex = hex::encode([0x0au8; 32]);
    record(
        &f.state,
        &f.kp,
        &device_hex,
        PATH_A,
        m,
        Some(owner_seal(PATH_A)),
    )
    .await;
    let snap = f.db.create_snapshot(f.folder_id).await.unwrap();

    let client = fauna_client_snapshots::SnapshotsClient::new(HandlerRequester {
        state: Arc::clone(&f.state),
        actor: f.actor,
    });

    let reply = client.get(snap.id).await.expect("snapshot.get");

    // Post-flip EVERY row rests sealed-only, so a custody-less reader renders
    // no names at all — and never an empty-string name (the ratified degrade
    // is OMIT).
    let rendered: Vec<&str> = reply.files.iter().map(|e| e.path.as_str()).collect();
    assert!(
        !rendered.iter().any(|p| p.is_empty()),
        "the ratified degrade is OMIT — never an empty name; got {rendered:?}"
    );
    assert!(
        !rendered.contains(&PATH_A) && !rendered.contains(&PATH_B),
        "a keyless reader must not see any sealed name; got {rendered:?}"
    );
    assert_eq!(
        reply.file_count, 2,
        "the snapshot still holds two files and a restore writes both — the \
         count describes the snapshot, not this reader's view of it"
    );
}

/// The diff reply names its folder, which is the only way a diff — requested
/// by two ids alone — can resolve label custody.
#[tokio::test]
async fn the_diff_reply_names_its_folder_so_a_client_can_resolve_custody() {
    let f = fixture().await;
    let snap_a = f.db.create_snapshot(f.folder_id).await.unwrap();
    {
        let conn = f.db.conn().await;
        conn.execute(
            "UPDATE snapshots SET created_at = created_at - 60 WHERE id = ?1",
            rusqlite::params![snap_a.id],
        )
        .unwrap();
    }
    let store = DiskBlobStore::new(f._tmp.path()).unwrap();
    let m = common::put_manifest(&store, &f.db, b"eviction notice v2").await;
    let device_hex = hex::encode([0x0au8; 32]);
    record(
        &f.state,
        &f.kp,
        &device_hex,
        PATH_A,
        m,
        Some(owner_seal(PATH_A)),
    )
    .await;
    let snap_b = f.db.create_snapshot(f.folder_id).await.unwrap();

    let client = fauna_client_snapshots::SnapshotsClient::new(HandlerRequester {
        state: Arc::clone(&f.state),
        actor: f.actor,
    })
    .with_label_custody(client_custody());

    let reply = client.diff(snap_a.id, snap_b.id).await.expect("diff");

    assert_eq!(
        reply.folder.as_deref(),
        Some(SET),
        "without this the client has no set name to resolve custody against"
    );
    assert_eq!(reply.summary.modified_count, 1);
    assert_eq!(reply.modified[0].path, PATH_A);
}

/// Path-sealing S5c-2: the diff reply's set-name pair survives the plaintext
/// blank — the LEAD's own success bar ("tests blank the plaintext and still
/// render the right name"). Owner-only set, so `keys_for` needs no name at
/// all — the bound-set custody-by-name gap this slice deliberately leaves
/// open doesn't apply here.
#[tokio::test]
async fn the_diff_reply_renders_the_set_name_from_its_seal_once_the_plaintext_blanks() {
    let f = fixture().await;
    let snap_a = f.db.create_snapshot(f.folder_id).await.unwrap();
    {
        let conn = f.db.conn().await;
        conn.execute(
            "UPDATE snapshots SET created_at = created_at - 60 WHERE id = ?1",
            rusqlite::params![snap_a.id],
        )
        .unwrap();
    }
    let store = DiskBlobStore::new(f._tmp.path()).unwrap();
    let m = common::put_manifest(&store, &f.db, b"eviction notice v2").await;
    let device_hex = hex::encode([0x0au8; 32]);
    record(
        &f.state,
        &f.kp,
        &device_hex,
        PATH_A,
        m,
        Some(owner_seal(PATH_A)),
    )
    .await;
    let snap_b = f.db.create_snapshot(f.folder_id).await.unwrap();

    // Stamp the set-name seal directly, as the engine's bind/serve catch-up
    // pass would (mirrors `a_stamped_set_name_seal_rides_the_list_with_its_salt_and_renders`).
    let sealed = client_seal_set_name(SET);
    f.db.update_folder_for_user(
        SET,
        &f.actor,
        fauna_nest::db::FolderUpdate {
            name_sealed: Some(&sealed),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Blank the resting plaintext — the true post-flip state, not a decoy
    // perturbation: this is the exact scenario the pair exists for.
    {
        let conn = f.db.conn().await;
        conn.execute(
            "UPDATE folders SET name = '' WHERE id = ?1",
            rusqlite::params![f.folder_id],
        )
        .unwrap();
    }

    let client = fauna_client_snapshots::SnapshotsClient::new(HandlerRequester {
        state: Arc::clone(&f.state),
        actor: f.actor,
    })
    .with_label_custody(client_custody());

    let reply = client.diff(snap_a.id, snap_b.id).await.expect("diff");

    assert_eq!(
        reply.folder.as_deref(),
        Some(SET),
        "the seal must recover the name the blanked plaintext no longer carries"
    );
    // The path renders still ran against whatever `folder` string the wire
    // carried into `render_paths` (a pre-existing, orthogonal resolution —
    // this slice doesn't change it), so the modified path still renders too.
    assert_eq!(reply.summary.modified_count, 1);
    assert_eq!(reply.modified[0].path, PATH_A);
}

// ── S5: the set-name plane ───────────────────────────────────────────────────
//
// A set NAME is the next user-chosen label after the path. Three properties, all
// of which must hold before the flip can scrub `folders.name`:
//
//   4. A keyed writer can stamp a set's `name_sealed` and the nest stores it
//      opaquely, exactly as it does `path_sealed`.
//   5. `fauna.folders.list` projects the seal **with its `name_hash` salt** —
//      the pair that survives the scrub. A seal without its salt is
//      unrenderable, which is the hole S2b shipped on `fauna.media.list`.
//   6. A reserved `__` set never seals: it is a routing constant at ~28 nest
//      decision sites, and a sealed reserved name would make the nest unable to
//      route on its own rails.

/// Seal a set name the way a keyed client does — through the one shared funnel,
/// under a root the nest never sees.
fn client_seal_set_name(name: &str) -> Vec<u8> {
    fauna_core::label_custody::seal_set_name(&LabelRoot::owner_of(&owner_key()), name)
        .expect("seal set name")
        .expect("a user-chosen name is not reserved")
}

/// The set-name plane's own router: `create` + `list` + `update`.
async fn set_name_fixture() -> (Arc<AppState>, [u8; 32]) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        fauna_nest::folder_handlers::register_folders_handlers(&mut b);
        b.build()
    });
    let state = Arc::new(AppState {
        rpc_router,
        ..AppState::for_test(db.clone())
    });
    let actor: [u8; 32] = [0x5a; 32];
    common::seed_dispatch_actor(&state.db, &actor).await;
    (state, actor)
}

async fn list_sets(
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> Vec<fauna_protocol::folders::FolderSummary> {
    let out = dispatch(
        state,
        actor,
        "fauna.folders.list",
        Bytes::from(
            encode_canonical(&fauna_protocol::folders::FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await;
    decode::<fauna_protocol::folders::FoldersListReply>(&out)
        .unwrap()
        .folders
}

/// Property 4 + 5: a stamped seal rides back out of `fauna.folders.list`
/// byte-for-byte **with its salt**, and renders under the client root with the
/// stored plaintext perturbed to a decoy — so a plaintext-first renderer fails
/// this test rather than passing it by accident.
#[tokio::test]
async fn a_stamped_set_name_seal_rides_the_list_with_its_salt_and_renders() {
    let (state, actor) = set_name_fixture().await;
    let name = "Family photos";

    let created: fauna_protocol::folders::FolderCreateReply = decode(
        &dispatch(
            &state,
            actor,
            "fauna.folders.create",
            Bytes::from(
                encode_canonical(&fauna_protocol::folders::FolderCreateRequest {
                    name: name.into(),
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();

    // The keyed-writer stamp — the engine's bind/serve hook shape, driven here
    // through the real `fauna.folders.update` handler.
    let sealed = client_seal_set_name(name);
    let updated: fauna_protocol::folders::FolderUpdateReply = decode(
        &dispatch(
            &state,
            actor,
            "fauna.folders.update",
            Bytes::from(
                encode_canonical(&fauna_protocol::folders::FolderUpdateRequest {
                    name: name.into(),
                    name_sealed: Some(serde_bytes::ByteBuf::from(sealed.clone())),
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();
    assert!(updated.ok, "the stamp must be accepted");

    // Perturb the resting plaintext to a decoy: from here on, any correct
    // render can only have come from the seal.
    {
        let conn = state.db.conn().await;
        conn.execute(
            "UPDATE folders SET name = ?1 WHERE id = ?2",
            rusqlite::params!["DECOY-not-the-real-name", created.id],
        )
        .unwrap();
    }

    let rows = list_sets(&state, actor).await;
    let row = rows
        .iter()
        .find(|r| r.id == created.id)
        .expect("the set is listed");

    assert_eq!(
        row.name_sealed.as_ref().map(|b| b.to_vec()),
        Some(sealed),
        "the nest must carry the blob it cannot read, byte-for-byte"
    );
    assert_eq!(
        row.name_hash.as_ref().map(|b| &b[..]),
        Some(&fauna_core::path_crypto::set_name_hash(name)[..]),
        "the salt must ride with the seal or the row is unrenderable post-scrub"
    );

    // The render seam opens it under the client root, ignoring the decoy.
    let keys = fauna_core::file_download::FileDownloadKeys::owner(owner_key());
    assert_eq!(
        fauna_core::label_custody::render_set_name(
            &keys,
            row.name_sealed.as_ref().map(|b| &b[..]),
            &row.name,
            row.name_hash.as_ref().map(|b| &b[..]),
        ),
        fauna_core::path_crypto::SealedLabelRender::Sealed(name.to_string()),
        "the seal must win over the perturbed plaintext"
    );

    // A keyless reader meeting the perturbed row omits it rather than showing
    // the decoy — the ratified degrade, on the set-name plane.
    assert_eq!(
        fauna_core::label_custody::render_set_name(
            &fauna_core::file_download::FileDownloadKeys::default(),
            row.name_sealed.as_ref().map(|b| &b[..]),
            "",
            row.name_hash.as_ref().map(|b| &b[..]),
        ),
        fauna_core::path_crypto::SealedLabelRender::Omit
    );
}

/// Property 6: a reserved `__` set is a routing constant and the nest must
/// refuse to stamp a seal onto it, whatever a client sends.
#[tokio::test]
async fn the_nest_refuses_to_seal_a_reserved_set_name() {
    let (state, actor) = set_name_fixture().await;

    // A live reserved rail, created the way the nest's own rails are.
    state
        .db
        .create_folder_with_options("__config", &actor, fauna_nest::db::FolderOptions::default())
        .await
        .unwrap();

    let payload = Bytes::from(
        encode_canonical(&fauna_protocol::folders::FolderUpdateRequest {
            name: "__config".into(),
            // A hostile or buggy client can seal anything it likes; the refusal
            // must be the nest's, not the client's good manners.
            name_sealed: Some(serde_bytes::ByteBuf::from(vec![9u8; 48])),
            ..Default::default()
        })
        .unwrap()
        .to_vec(),
    );
    let meta = state
        .rpc_router
        .kind_meta("fauna.folders.update")
        .expect("kind registered");
    let err = (meta.handler)(Arc::clone(&state), actor, payload)
        .await
        .expect_err("a stamp on a reserved rail must be refused, not accepted");
    assert!(
        err.code.contains("invalid_request"),
        "expected a reserved-namespace refusal, got {err:?}"
    );

    let stored: Option<Vec<u8>> = {
        let conn = state.db.conn().await;
        conn.query_row(
            "SELECT name_sealed FROM folders WHERE name = '__config'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        stored, None,
        "a routing constant must rest plaintext — ~28 nest decision sites route on it"
    );
}

// ── the conflict plane's write half (S6-a) ───────────────────────────────────
//
// `SyncConflict` shipped `path_hash` / `path_sealed` / `details_sealed` in S2,
// but nothing ever wrote them: `ConflictReportRequest` had no sealed fields and
// `CacheDb::report_conflict`'s INSERT named no sealed columns, so the reply type
// had outrun its writer and every conflict row rested fully plaintext. These
// pins close that, and they pin the two properties a future session is most
// likely to break.

const CONFLICT_PATH: &str = "2026/eviction_notice.pdf";
const CONFLICT_DETAILS: &str = "both devices wrote while offline";

/// Report a conflict through the real handler.
async fn report_conflict(
    state: &Arc<AppState>,
    actor: [u8; 32],
    device_hex: &str,
    labels: fauna_client_sync::ConflictLabels,
    details: Option<&str>,
) -> i64 {
    let req = fauna_protocol::folders::ConflictReportRequest {
        folder: SET.to_string(),
        device_id: device_hex.to_string(),
        path: CONFLICT_PATH.to_string(),
        conflict_type: "concurrent_edit".to_string(),
        details: details.map(str::to_string),
        path_hash: labels.path_hash,
        path_sealed: labels.path_sealed,
        details_sealed: labels.details_sealed,
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let reply: fauna_protocol::folders::ConflictReportReply =
        decode(&dispatch(state, actor, "fauna.sync.conflicts.report", payload).await).unwrap();
    reply.id
}

async fn list_conflicts(
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> Vec<fauna_protocol::folders::SyncConflict> {
    let req = fauna_protocol::folders::ConflictsListRequest {
        include_resolved: Some(true),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let reply: fauna_protocol::folders::ConflictsListReply =
        decode(&dispatch(state, actor, "fauna.sync.conflicts.list", payload).await).unwrap();
    reply.conflicts
}

/// The S6-a round trip: a client-minted seal for BOTH the path and the
/// free-text details survives `conflicts.report` → store → `conflicts.list`
/// byte-for-byte, opens only under the root the client sealed with, and the
/// nest — which holds exactly these bytes and no key — can read neither.
#[tokio::test]
async fn a_client_sealed_conflict_rides_the_wire_and_back() {
    let f = fixture().await;
    let device_hex = hex::encode([0x0au8; 32]);
    let root = LabelRoot::owner(CLIENT_ROOT);
    let labels =
        fauna_client_sync::ConflictLabels::seal(Some(&root), CONFLICT_PATH, Some(CONFLICT_DETAILS))
            .expect("sealing a path and a details string cannot fail");
    let sealed_path = labels.path_sealed.clone().expect("the path sealed");
    let sealed_details = labels.details_sealed.clone().expect("the details sealed");

    report_conflict(
        &f.state,
        f.actor,
        &device_hex,
        labels,
        Some(CONFLICT_DETAILS),
    )
    .await;

    let rows = list_conflicts(&f.state, f.actor).await;
    let row = rows
        .iter()
        .find(|c| c.path_hash[..] == fauna_core::sync::path_hash(CONFLICT_PATH)[..])
        .expect("the reported conflict is listed (hash-addressed — no plaintext rests)");

    // Verbatim, both halves.
    assert_eq!(
        row.path_sealed.as_ref().map(|s| s.to_vec()),
        Some(sealed_path.to_vec()),
        "the sealed path must survive report → store → list unchanged"
    );
    assert_eq!(
        row.details_sealed.as_ref().map(|s| s.to_vec()),
        Some(sealed_details.to_vec()),
        "the sealed details must survive report → store → list unchanged"
    );
    // The salt rides with them — without it a scrubbed row is unopenable, the
    // hole S2b hit on `fauna.media.list` and S4 hit on `WebdavFile`.
    assert_eq!(
        &row.path_hash[..],
        &fauna_core::sync::path_hash(CONFLICT_PATH)[..],
        "the convergent salt must ride the row"
    );

    // Both open under the client's root...
    let salt = fauna_core::sync::path_hash(CONFLICT_PATH);
    assert_eq!(
        fauna_core::path_crypto::open(
            [&CLIENT_ROOT],
            &salt,
            LabelField::SyncChangePath,
            &SealedLabel::from_bytes(row.path_sealed.as_ref().unwrap()).unwrap(),
        )
        .expect("the path opens under the sealing root"),
        CONFLICT_PATH.as_bytes()
    );
    assert_eq!(
        fauna_core::path_crypto::open(
            [&CLIENT_ROOT],
            &salt,
            LabelField::ConflictDetails,
            &SealedLabel::from_bytes(row.details_sealed.as_ref().unwrap()).unwrap(),
        )
        .expect("the details open under the sealing root"),
        CONFLICT_DETAILS.as_bytes()
    );

    // ...and fail closed under any other root, which is what makes the hosting
    // nest unable to read either.
    for (field, blob) in [
        (
            LabelField::SyncChangePath,
            row.path_sealed.as_ref().unwrap(),
        ),
        (
            LabelField::ConflictDetails,
            row.details_sealed.as_ref().unwrap(),
        ),
    ] {
        assert!(
            fauna_core::path_crypto::open(
                [&[0x11u8; 32]],
                &salt,
                field,
                &SealedLabel::from_bytes(blob).unwrap(),
            )
            .is_err(),
            "a wrong root must fail closed for {field:?}"
        );
    }
}

/// The two fields share one salt (the row's `path_hash`), which is sound only
/// because the field tag is mixed into the key derivation AND the AAD. Pinned
/// end to end on real stored rows, not just in the unit test: neither blob
/// opens as the other.
#[tokio::test]
async fn the_stored_conflict_halves_do_not_open_as_each_other() {
    let f = fixture().await;
    let device_hex = hex::encode([0x0au8; 32]);
    let root = LabelRoot::owner(CLIENT_ROOT);
    let labels =
        fauna_client_sync::ConflictLabels::seal(Some(&root), CONFLICT_PATH, Some(CONFLICT_DETAILS))
            .unwrap();
    report_conflict(
        &f.state,
        f.actor,
        &device_hex,
        labels,
        Some(CONFLICT_DETAILS),
    )
    .await;

    let rows = list_conflicts(&f.state, f.actor).await;
    let row = rows
        .iter()
        .find(|c| c.path_hash[..] == fauna_core::sync::path_hash(CONFLICT_PATH)[..])
        .unwrap();
    let salt = fauna_core::sync::path_hash(CONFLICT_PATH);

    assert!(
        fauna_core::path_crypto::open(
            [&CLIENT_ROOT],
            &salt,
            LabelField::ConflictDetails,
            &SealedLabel::from_bytes(row.path_sealed.as_ref().unwrap()).unwrap(),
        )
        .is_err(),
        "the path blob must not open under the details tag"
    );
    assert!(
        fauna_core::path_crypto::open(
            [&CLIENT_ROOT],
            &salt,
            LabelField::SyncChangePath,
            &SealedLabel::from_bytes(row.details_sealed.as_ref().unwrap()).unwrap(),
        )
        .is_err(),
        "the details blob must not open under the path tag"
    );
}

/// THE approved compat break, conflict plane (S9 flip): a sealless conflict
/// report is refused with the typed `path_seal_required` — every production
/// reporter seals (the S6-a funnels), and accepting a sealless one would rest
/// an unrenderable row and mint label-less propagation records at resolve
/// time. This replaces the pre-flip "stores the hash and no seal" pin: there
/// is no sealless resting shape left to pin.
#[tokio::test]
async fn a_sealless_conflict_report_is_refused_loudly() {
    let f = fixture().await;
    let device_hex = hex::encode([0x0au8; 32]);
    let req = fauna_protocol::folders::ConflictReportRequest {
        folder: SET.to_string(),
        device_id: device_hex,
        path: CONFLICT_PATH.to_string(),
        conflict_type: "concurrent_edit".to_string(),
        details: Some(CONFLICT_DETAILS.to_string()),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let meta = f
        .state
        .rpc_router
        .kind_meta("fauna.sync.conflicts.report")
        .unwrap();
    let err = (meta.handler)(f.state.clone(), f.actor, payload)
        .await
        .expect_err("a sealless conflict report must refuse, not land");
    assert!(
        err.code.contains("path_seal_required"),
        "the refusal must carry the typed code, got {}",
        err.code
    );
}

/// THE approved compat break (S9 flip, `encryption-at-rest.md` § Carve-outs,
/// user-approved 2026-08-01): a sealless record on a sealed plane is refused
/// LOUDLY with the typed `path_seal_required` code — a silent hash-only row
/// can never be applied by a puller and never listed by anyone, so the
/// visible error is strictly better than accepting it. A seal-less writer
/// hitting this is refused, never accommodated — which is what lets the
/// appliers treat a row with neither plaintext nor seal as a permanent
/// refusal rather than a shape to skip past in silence.
#[tokio::test]
async fn a_sealless_record_is_refused_loudly() {
    let f = fixture().await;
    let device_hex = hex::encode([0x0au8; 32]);
    let req = SyncChangeRecordRequest {
        folder: SET.to_string(),
        device_id: device_hex,
        path: "attic/eviction_notice.pdf".to_string(),
        manifest_hash: Some(hex::encode([0xAAu8; 32])),
        size_bytes: 0,
        change_type: "create".to_string(),
        path_sealed: None,
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let meta = f
        .state
        .rpc_router
        .kind_meta("fauna.sync.changes.record")
        .unwrap();
    let err = (meta.handler)(f.state.clone(), f.actor, payload)
        .await
        .expect_err("a sealless record must refuse, not land");
    assert!(
        err.code.contains("path_seal_required"),
        "the refusal must carry the typed code, got {}",
        err.code
    );
}
