//! **The native full restore** (tier_3) — `FfiSyncEngineHost::restore_snapshot_to_dir`
//! driven end to end against a real in-process nest, on a snapshot whose file rows
//! rest **sealed**, asserting the bytes land on disk.
//!
//! Goal doc: `docs/goal/behavior/path-sealing.md` § THE CONSUMER-WIRING RULE →
//! *The rule's own sweep* + `docs/goal/ui/backups.md`
//! § User actions (the `snapshot-item` row: the file list is a sealed-plane read,
//! and "the client must hand it label custody or the list renders empty").
//!
//! # What broke, and why the seam pins did not catch it
//!
//! `restore` built its `SnapshotsClient` **keyless** while the host held the
//! owner seed and spent it on the chunks forty lines later. `render_paths` drops
//! every row it cannot open (the ratified non-audience degrade, `Omit => false`),
//! so post-flip the listing came back **empty**, the walk had nothing to fetch,
//! and the function returned `files_restored: 0` **with `Ok`** — an empty
//! directory reported to the user as a successful backup restore.
//!
//! Both halves of that composition were *already* pinned in isolation, and both
//! pins stayed green while the bug shipped:
//!
//! - `fauna-ffi::snapshots_client`'s `the_read_seam_is_always_keyed` pins the
//!   *constructor's* shape — but a consumer that never calls it is invisible to it.
//! - `fauna-client-snapshots`' `get_for_restore_refuses_…_write_nothing` pins the
//!   *sink* — but only for a caller that routes through it.
//! - `fauna-sync-engine`'s `build_restore_engine_restores_a_sealed_snapshot` pins
//!   the walk + write — fed a hand-built `Vec<SnapshotFileToRestore>`, so it never
//!   reads a listing at all.
//!
//! The defect lived in none of those three and in all of the gaps between them:
//! **the composition**. That is what this file pins, and it is the only test that
//! drives the listing read, the chunk walk and the filesystem write as one call.
//!
//! # Why headless, and what is genuinely left to a Mac
//!
//! This is the `conformance_*_client.rs` vehicle its sibling
//! `conformance_file_provider_client.rs` established and `on-demand-files.md`
//! § On-Demand Files → *Headless-first testing* ratified: drive the FFI host
//! **directly** against a real nest, with no app, no OS surface and no human in
//! the loop. `FfiSyncEngineHost` is plain async Rust, so the mechanism needs no
//! Mac — it compiles and runs identically on all three dev machines, which is
//! what makes it a merge-gate-able regression barrier rather than a manual pass.
//!
//! The host here is built through the **production macOS path**, not a test
//! shortcut: `FfiNestClient::new` → `connect()` → `start_account_runtime(..)` →
//! `sync_engine_host(..)` is exactly what `FaunaClient.startAccountRuntime()`
//! and `FaunaClient.startSyncHost()` call on macOS
//! post-`fauna-sync-agent`-cutover (`syncEngineHost`), so the credential shape
//! under test (`EngineCredential::Seed`) is the shipped one, and the listing
//! read resolves custody through the seat's account runtime as the app's does.
//! What remains above this seam on macOS is three lines of SwiftUI glue in
//! `MacRestoreView.startRestore()` and an `NSOpenPanel` folder choice — an OS
//! file picker, which is the *only* part of this feature that a human's eye is
//! actually needed for.
//!
//! # The sealed plane under test
//!
//! "Sealed" here is the **label/path** plane — the one that produced
//! `files_restored: 0`, because it is the listing render that dropped rows. The
//! owner's files are captured through the production `SyncEngine::upload_file`,
//! which seals each path under the owner `BackupKey`'s chunk root *and* seals the
//! content chunks, so both planes are real; the content plane additionally has
//! its own dedicated coverage in `fauna-sync-engine`'s
//! `restore_snapshot_files_to_dir_restores_a_sealed_snapshot`.
//!
//! Tier: tier_3 (real `AppState` + `CacheDb` + `BackupService` over a bound
//! `TcpListener`, real WS-RPC control plane, real chunk-store HTTP byte routes,
//! real seal and real decrypt — nothing mocked).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_snapshots::SnapshotsClient;
use fauna_core::crypto::BackupKey;
use fauna_core::format::{ConflictPolicy, FormatRegistry};
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::transfer::TransferPool;

/// The owner's Ed25519 secret — the sole custody root. Both the capturing engine
/// and the restoring host derive their owner `BackupKey` from exactly this, which
/// is the property that makes the restore openable at all.
const OWNER_SECRET: [u8; 32] = [0x53; 32];
/// The device that captured the files (the user's laptop).
const WRITER_DEVICE_ID: [u8; 32] = [0x09; 32];
/// The restoring host's own device id — distinct from the writer's, the realistic
/// one-owner-N-devices shape (and the shape macOS actually has: the resident
/// engines live in the external agent, the app's one-shot host is its own device).
const HOST_DEVICE_ID: [u8; 32] = [0x0b; 32];
const FOLDER: &str = "restore_flow";
/// The set's client-minted nonce, stored on `FOLDER` the way the owner's
/// create stores it — every change record the capture engine writes is signed
/// under it (the nest refuses an unsigned record `signature_required`).
const SET_NONCE: [u8; 32] = [0x6E; 32];

/// Deliberately non-ASCII: a byte-exact assertion on disk catches an encoding
/// slip in the walk that a plain-ASCII fixture would pass.
const TOP_CONTENT: &[u8] = b"fauna full-restore proof \xe2\x9c\x93\n";
/// Nested, so the restore covers the create-parent-directories path.
const NESTED_CONTENT: &[u8] = b"nested under a directory the restore must create\n";
const TOP_PATH: &str = "notes.txt";
const NESTED_PATH: &str = "docs/nested.txt";

/// A live nest plus everything the test needs to address it.
struct Nest {
    /// `http://127.0.0.1:<port>` — the WS adapter rewrites `http→ws`.
    base: String,
    db: Arc<CacheDb>,
    /// A bearer for the owner, for the capture engine's HTTP chunk uploads.
    owner_bearer: String,
    /// The chunk store's backing dir. Held (not `mem::forget`-leaked as the
    /// sibling FP conformance does) so the ~KBs go away with the test: every
    /// request the restore issues has been awaited by the time this drops.
    _blob_dir: tempfile::TempDir,
}

impl Nest {
    /// [`FOLDER`]'s row as the nest holds it — its id, and the set nonce every
    /// record into it is signed under.
    async fn set_row(&self) -> fauna_nest::db::FolderRow {
        // Addressed by the set's name hash: a seat-created set rests sealed
        // from birth (no plaintext name on the row), a directly created one
        // carries both.
        let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
        if let Some(row) = self
            .db
            .get_folder_for_actor_by_name_hash(
                &fauna_core::path_crypto::set_name_hash(FOLDER),
                &owner,
            )
            .await
            .unwrap()
        {
            return row;
        }
        self.db
            .get_folder_for_actor(FOLDER, &owner)
            .await
            .unwrap()
            .expect("the set exists on the nest")
    }
}

/// How [`FOLDER`] comes to exist on the nest.
enum SetCreation {
    /// The fixture writes the row and [`SET_NONCE`] straight into the DB, and
    /// no custody exists anywhere — the keyless negative arm's shape.
    Direct,
    /// Nothing is pre-created: the restoring seat creates the set through its
    /// own production door (`FfiFoldersClient::create` → `create_set`), which
    /// mints the nonce into the account plane's custody — where the seat's
    /// licence read resolves it from — and stores it on the nest's row. The
    /// capture engine then signs under the nonce read back off that row.
    BySeat,
}

/// The nest's deployment identity — the escrow-receipt signer the account
/// plane's first custody write (a [`SetCreation::BySeat`] create) deposits
/// its generation against, as the sibling seed-holding fixtures pin it.
fn deployment_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x66; 32])
}

/// Start a real in-process nest serving the auth + sync + filesync + folder +
/// version-history + escrow WS-RPC kinds and the chunk-store HTTP byte routes
/// over a bound `TcpListener`, with the owner registered and — for
/// [`SetCreation::Direct`] — an owner-only folder pre-created.
///
/// `register_filesync_handlers` is the one this file cannot do without: it serves
/// `fauna.filesync.snapshot.get`, the sealed-plane read whose custody wiring is
/// the whole subject.
async fn start_test_nest(creation: SetCreation) -> Nest {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    // The blob dir must outlive the capture engine — the restore's chunk GETs
    // read it long after that engine is gone — so it is owned by the returned
    // `Nest`, which lives as long as the test body.
    let blob_dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, blob_dir.path().to_path_buf(), None).unwrap(),
    );

    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id();
    db.create_user(&owner.0, "free", "test").await.unwrap();
    if let SetCreation::Direct = creation {
        db.create_folder(FOLDER, &owner.0).await.unwrap();
        // The set's stored nonce — what a signed record's statement is
        // verified under (the owner's create helper sends it; this arm creates
        // the row directly, so it stores it the way the owner's update would).
        db.update_folder_for_user(
            FOLDER,
            &owner.0,
            fauna_nest::db::FolderUpdate {
                set_nonce: Some(&SET_NONCE),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    db.register_sync_device(&owner.0, &WRITER_DEVICE_ID, "laptop", None, "write")
        .await
        .unwrap();

    let token_store = Arc::new(TokenStore::new());
    let owner_bearer = token_store.insert(owner, 3600).await;

    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            // The restoring host's `AuthClient::new` mints its HTTP bearer over
            // the WS-RPC `fauna.auth.handshake` kind (WsChallengeBearer), and
            // `FfiNestClient::connect` needs the same — without these the host
            // fails at connect rather than at the read under test.
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            // `fauna.filesync.snapshot.get` — the sealed-plane read.
            fauna_nest::filesync_handlers::register_filesync_handlers(&mut b);
            // `fauna.files.versions.list` — the licence read: every snapshot
            // entry restores only under the judged change row of the set that
            // named it, resolved from the version history before any byte
            // (`backup-restore.md` § Security Properties → Integrity).
            fauna_nest::files_handlers::register_files_handlers(&mut b);
            // The capture engine resolves the owner-only content binding via
            // `fauna.folders.list`; without it the binding is indeterminate and
            // the engine fails closed instead of sealing.
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            // The generation-escrow doors and `fauna.nest.info` — what the
            // seat's account plane needs for its first custody write (the
            // `SetCreation::BySeat` create mints the generation it seals
            // under and deposits it here).
            fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store,
            ..Default::default()
        },
        nest_signing_key: Some(deployment_key()),
        ..AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    Nest {
        base: format!("http://{addr}"),
        db,
        owner_bearer,
        _blob_dir: blob_dir,
    }
}

/// The owner's seed-holding capture engine (owner-only `BackupKey`, no MLS),
/// whose control-plane `NestClient` is genuinely connected — the production
/// upload path, mirroring `conformance_file_provider_client.rs`. It signs every
/// record directly with the owner's identity key under the set's nonce
/// (`set_nonce` — [`SET_NONCE`], or the one the seat's create minted), as a
/// seed-holding engine does.
fn owner_capture_engine(
    url: &str,
    bearer: &str,
    watch_path: std::path::PathBuf,
    set_nonce: [u8; 32],
) -> (SyncEngine, Arc<NestClient>) {
    let (engine_client, nest_client) = fauna_nest::test_support::sync_engine_auth_client(
        url,
        bearer,
        OWNER_SECRET,
        &WRITER_DEVICE_ID,
    );
    let ignore = IgnoreMatcher::load(&watch_path).unwrap_or_default();

    let engine = SyncEngine::new(
        watch_path,
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some(FOLDER.to_string()),
        WRITER_DEVICE_ID,
        None, // mls
        None, // epoch_secret
        Some(BackupKey::derive(&OWNER_SECRET).into()),
        None, // mls_group_id
        None, // content_keys
        ConflictPolicy::Auto,
        FormatRegistry::new(),
        ignore,
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    )
    .with_change_signer(
        Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
            &ActorKeypair::from_secret(OWNER_SECRET),
        )),
        Some(set_nonce),
    );
    (engine, nest_client)
}

/// Capture both files through the production engine (seal path + content →
/// chunk/manifest HTTP upload → `fauna.sync.changes.record`), then take a
/// snapshot of the set. Returns the snapshot id.
///
/// The returned `TempDir` guard is dropped by the caller *after* the restore, so
/// the source tree cannot be mistaken for the restore output.
async fn capture_and_snapshot(nest: &Nest) -> (i64, tempfile::TempDir) {
    capture_files_and_snapshot(
        nest,
        &[(TOP_PATH, TOP_CONTENT), (NESTED_PATH, NESTED_CONTENT)],
    )
    .await
}

/// [`capture_and_snapshot`] over any set of `(relative path, bytes)` files.
async fn capture_files_and_snapshot(
    nest: &Nest,
    files: &[(&str, &[u8])],
) -> (i64, tempfile::TempDir) {
    let watch = tempfile::tempdir().unwrap();
    for (rel, body) in files {
        let target = watch.path().join(rel);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, body).unwrap();
    }

    let set = nest.set_row().await;
    let set_nonce: [u8; 32] = set
        .set_nonce
        .as_deref()
        .and_then(|n| n.try_into().ok())
        .expect("the set carries its nonce");
    let (engine, nest_client) = owner_capture_engine(
        &nest.base,
        &nest.owner_bearer,
        watch.path().to_path_buf(),
        set_nonce,
    );
    // Force the WS auth handshake up front: without it the first record can time
    // out inside its own deadline and read identically to a genuine rejection.
    nest_client
        .connect()
        .await
        .expect("capture nest_client must reach Connected (WS auth handshake)");

    for (rel, _) in files {
        let outcome = engine
            .upload_file(rel)
            .await
            .unwrap_or_else(|e| panic!("upload_file({rel}): {e}"));
        assert!(
            outcome.recorded,
            "the real nest must record the owner's capture of {rel}"
        );
    }

    let snap = nest.db.create_snapshot(set.id).await.unwrap();
    assert_eq!(
        snap.file_count,
        files.len() as i64,
        "the snapshot must hold every captured file — the restore's own witness \
         for how many rows it owes (`get_for_restore` compares against this)"
    );
    (snap.id, watch)
}

/// Point this process's credential slot at a private directory, once, before
/// any test body runs.
///
/// The seat's account runtime keeps its writer key in the machine's credential
/// store, and the production constructor resolves the OS keyring unless
/// `FAUNA_E2E_CREDENTIAL_DIR` redirects it (`fauna-credential-store`, the
/// harness redirect every app e2e launch sets). Without this a test run would
/// write into the developer's own keyring.
///
/// Every test in this binary calls it as its first statement, so no test body
/// is running while the variable is written.
fn isolate_credentials() {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: inside the `OnceLock` initializer and ahead of every test
        // body in this binary — the other test threads are blocked on this
        // same initializer or have not started their bodies (edition 2024
        // marks set_var unsafe because of cross-thread visibility).
        unsafe {
            std::env::set_var("FAUNA_E2E_CREDENTIAL_DIR", dir.path());
        }
        dir
    });
}

/// The seat's account runtime is one per process (`fauna_ffi`'s host slot), so
/// the arms that start one take turns.
static SEAT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A restoring seat: the connected client, its engine host, and the temp dirs
/// both keep state under.
struct RestoringSeat {
    client: Arc<fauna_ffi::FfiNestClient>,
    host: Arc<fauna_ffi::FfiSyncEngineHost>,
    _state_dir: tempfile::TempDir,
    _store_dir: tempfile::TempDir,
}

impl RestoringSeat {
    /// Create [`FOLDER`] the way the owner's app does
    /// (`FfiFoldersClient::create` → `fauna_client_folders::create_set`): the
    /// set nonce minted into this seat's account-plane custody and stored on
    /// the nest's row. The restore's licence read judges every entry's row
    /// under the nonce this custody resolves (`backup-restore.md` § Security
    /// Properties → Integrity), so the positive arms own their set this way
    /// rather than through a DB write no custody knows of.
    ///
    /// The create's first custody write mints the account's generation and
    /// deposits it in escrow over the runtime's own data path — the store
    /// principal's client, whose first connect waits for its grant to be
    /// registered on a runtime pass (`spawn_connect_retry`; a principal the
    /// process already holds re-registers against this nest first). The
    /// runtime reports itself assembled before that path is up, so the
    /// create is retried, poking a pass each time, within one generous
    /// deadline a green run pays a tick of — exactly the retry the app's user
    /// makes on the same early error.
    async fn create_set(&self) {
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(240);
        loop {
            match self.client.folders().create(FOLDER.to_string(), None).await {
                Ok(_) => return,
                Err(e) if std::time::Instant::now() < deadline => {
                    println!(
                        "create_set: retrying at {:?} after {e:?}",
                        started.elapsed()
                    );
                    self.client.account_pump_now().await;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!(
                    "the owner's production create (custody on the plane, the row on the \
                     nest) must succeed within the deadline: {e:?}"
                ),
            }
        }
    }

    /// Stop the account runtime before the next arm starts its own.
    async fn stop(self) {
        self.client.stop_account_runtime().await;
    }
}

/// The macOS app's own seat construction: `FaunaClient.startAccountRuntime()`
/// (`FfiNestClient::start_account_runtime`) and `FaunaClient.startSyncHost()`
/// (`APIClient.syncEngineHost`).
///
/// **The account runtime is part of the seat, not an optional extra.** The
/// restore's listing read resolves each set's custody through the runtime's
/// folder-key store (`fauna_ffi::account_runtime::folder_key_store`), and a
/// read that meets no runtime is custody-unreadable: the resolver answers
/// "could not determine", the reader gets no keys (never the owner fallback),
/// and every sealed row is omitted. A fixture that skips the start reds on
/// `get_for_restore`'s refusal, which is the product working as designed.
///
/// Two inputs differ from the app's, for isolation only: the store roots under
/// a temp dir rather than the per-user root, and the credential slot is the
/// harness's file backend ([`isolate_credentials`]).
async fn restoring_seat(base: &str) -> RestoringSeat {
    // What this machine PINNED for its nest is the only escrow holder the
    // seat's account plane trusts (`fauna_client::trust::trusted_escrow_holders`
    // — never the nest's own claim about itself). Production pins by TOFU on
    // the first clean connect; the fixture seeds the same pin through the
    // test door, so the `SetCreation::BySeat` create's first custody write
    // verifies the receipt the nest's deployment identity signs.
    fauna_anon_client::trust::pin_identity_for_test(
        base,
        deployment_key().verifying_key().to_bytes(),
    );
    let state_dir = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let client = fauna_ffi::FfiNestClient::new(base.to_string(), OWNER_SECRET.to_vec())
        .expect("build FfiNestClient");
    client.connect().await.expect("FfiNestClient connect");
    client
        .start_account_runtime(
            state_dir.path().to_string_lossy().into_owned(),
            Some(fauna_ffi::FfiStoreContainer {
                dir: store_dir.path().to_string_lossy().into_owned(),
                exclusion: fauna_ffi::FfiCloudBackupExclusion::DeclaredInManifest {
                    declaration: "test: a temp dir, in no cloud backup".to_string(),
                },
            }),
            Some(HOST_DEVICE_ID.to_vec()),
            None,
        )
        .await
        .expect("start the account runtime");
    // The assembly is spawned; the restore must not race it. Deadline-poll the
    // seat's own published state — one generous budget a green run pays a tick
    // of.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let state: serde_json::Value =
                serde_json::from_str(&client.account_pump_cycles_json()).unwrap();
            if state["runtime"] == true {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the account runtime must assemble (the seat's `account_pump_cycles` state)");
    let host = client
        .sync_engine_host(
            OWNER_SECRET.to_vec(),
            HOST_DEVICE_ID.to_vec(),
            state_dir.path().to_string_lossy().into_owned(),
            "fauna-macos".to_string(),
            None,
        )
        .expect("sync engine host");
    RestoringSeat {
        client,
        host,
        _state_dir: state_dir,
        _store_dir: store_dir,
    }
}

/// **The positive arm.** A sealed snapshot restored through the real
/// `FfiSyncEngineHost` puts the real bytes on disk — every entry licensed by
/// the owner's signed change row, read from the real nest's version history
/// and judged under the set nonce the seat's custody resolves
/// (`backup-restore.md` § Security Properties → Integrity): the seat owns the set through its production create door
/// ([`SetCreation::BySeat`]), so the composition under test is the app's own.
///
/// The assertion that matters is on the **return value and the filesystem**, not
/// on a log line: the original bug's whole signature was a *successful-looking*
/// return, so a test that only checked "no error" would have passed against it.
///
/// **The snapshot holds a multi-chunk file too.** The walk fetches, opens and
/// reassembles every chunk a manifest lists, in order; a small file never
/// lists more than one, so it cannot catch a reassembly that drops, repeats or
/// reorders chunks. 20 MiB of incompressible bytes is well past the chunker's
/// single-chunk threshold — the fixture asserts that itself, so the test
/// cannot quietly degrade to one chunk. (Until 2026-10-01 this was pinned only
/// end to end through the legacy daemon's `restore`; until 2026-10-13 it was
/// an arm of its own, folded in here while a SECOND account-runtime start in
/// one process never brought the store principal's data path up — its client
/// signed with a writer key the assembly had already rotated away. That is
/// fixed and pinned by
/// [`a_second_account_runtime_in_one_process_brings_its_data_path_up`].)
#[tokio::test]
async fn a_sealed_snapshot_restores_its_files_to_disk() {
    isolate_credentials();
    const LARGE_PATH: &str = "media/large.bin";
    // Deterministic, incompressible (xorshift64*), so compression cannot fold
    // it under the threshold and a failure reproduces exactly.
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let large: Vec<u8> = (0..20 * 1024 * 1024 / 8)
        .flat_map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes()
        })
        .collect();
    let chunks = fauna_core::chunker::chunk_file(&large).chunk_hashes.len();
    assert!(
        chunks > 1,
        "fixture precondition: the file must cut into several chunks, got {chunks}"
    );

    let _seat_turn = SEAT.lock().await;
    let nest = start_test_nest(SetCreation::BySeat).await;
    let seat = restoring_seat(&nest.base).await;
    seat.create_set().await;
    let (snapshot_id, _watch) = capture_files_and_snapshot(
        &nest,
        &[
            (TOP_PATH, TOP_CONTENT),
            (NESTED_PATH, NESTED_CONTENT),
            (LARGE_PATH, large.as_slice()),
        ],
    )
    .await;

    let out = tempfile::tempdir().unwrap();
    let restored = seat
        .host
        .restore_snapshot_to_dir(snapshot_id, out.path().to_string_lossy().into_owned())
        .await;
    seat.stop().await;
    let summary = restored.expect("the full restore must succeed");

    // The signature, stated positively: the count is what the snapshot
    // holds, never the zero a dropped listing would produce.
    assert_eq!(
        summary.files_restored, 3,
        "every sealed row must restore; `files_restored: 0` here is exactly the \
         silent data-loss bug this test exists to prevent (summary: {summary:?})",
    );
    assert_eq!(
        summary.skipped, 0,
        "no row may be skipped (summary: {summary:?})"
    );
    assert_eq!(
        summary.bytes_written,
        (TOP_CONTENT.len() + NESTED_CONTENT.len() + large.len()) as u64,
        "every captured byte must be written (summary: {summary:?})",
    );

    // The filesystem is the witness the summary cannot fake.
    assert_eq!(
        std::fs::read(out.path().join(TOP_PATH)).expect("top-level file restored"),
        TOP_CONTENT,
        "the restored bytes must be byte-identical to what was captured",
    );
    assert_eq!(
        std::fs::read(out.path().join(NESTED_PATH)).expect("nested file restored"),
        NESTED_CONTENT,
        "the nested file must restore, parent directory created",
    );
    let restored = std::fs::read(out.path().join(LARGE_PATH)).expect("large file restored");
    assert_eq!(
        restored.len(),
        large.len(),
        "the restored file must be whole"
    );
    assert!(
        restored == large,
        "the {chunks}-chunk file must restore byte-identically (first difference at byte {:?})",
        restored.iter().zip(&large).position(|(a, b)| a != b)
    );
}

/// **A second seat in one process reaches its own data path.** The seat's
/// account runtime is one per process, but a process starts it more than once
/// — an account switch, a nest move — and each start's first custody write
/// deposits its generation over THAT runtime's store-principal connection.
/// Two cycles against two nests: the second create must land within the
/// principal's retry backoff, never stall until the app is relaunched.
#[tokio::test]
async fn a_second_account_runtime_in_one_process_brings_its_data_path_up() {
    isolate_credentials();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _seat_turn = SEAT.lock().await;

    let first = start_test_nest(SetCreation::BySeat).await;
    let seat = restoring_seat(&first.base).await;
    seat.create_set().await;
    seat.stop().await;

    let second = start_test_nest(SetCreation::BySeat).await;
    let seat = restoring_seat(&second.base).await;
    let started = std::time::Instant::now();
    seat.create_set().await;
    let took = started.elapsed();
    seat.stop().await;
    second.set_row().await;
    assert!(
        took < std::time::Duration::from_secs(45),
        "the second runtime's first custody write must land within the store \
         principal's retry backoff (its 30 s ceiling plus a pass), took {took:?}"
    );
}

/// **The negative arm — the regression barrier.** The same snapshot, read the way
/// `restore` read it *before the fix*: a keyless `SnapshotsClient`.
///
/// This is the A/B on the exact line the fix changed, against real nest replies
/// rather than a planted mock, and it pins both halves of that fix:
///
/// 1. **Why it was silent**: a keyless `get` renders **zero** rows while the
///    reply's own `file_count` still says 2 — nothing errors, the walk simply has
///    nothing to do, and a restore built on it writes an empty directory.
/// 2. **Why it cannot be silent again**: routed through `get_for_restore`, that
///    same shortfall is a **loud error**. Every restore entry point routes
///    through it (`path-sealing.md` § THE CONSUMER-WIRING RULE → "the class now
///    has a SINK"), so a future seam that forgets its custody gets this error
///    instead of the empty directory above.
#[tokio::test]
async fn a_custody_less_restore_read_errors_instead_of_silently_restoring_nothing() {
    isolate_credentials();
    let nest = start_test_nest(SetCreation::Direct).await;
    let (snapshot_id, _watch) = capture_and_snapshot(&nest).await;

    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    let client = NestClient::new(nest.base.clone(), owner_kp);
    client.connect().await.expect("connect (auth + WS upgrade)");

    // Exactly what `restore` used to build: `SnapshotsClient::new(nest)`, no
    // `with_label_custody`, while the caller held the seed all along.
    let keyless = SnapshotsClient::new(Arc::clone(&client));

    let browsed = keyless
        .get(snapshot_id)
        .await
        .expect("the browse read itself succeeds — that is the trap");
    assert!(
        browsed.files.is_empty(),
        "a keyless reader must render no sealed row (the ratified Omit degrade); \
         got {:?}",
        browsed.files.iter().map(|f| &f.path).collect::<Vec<_>>(),
    );
    assert_eq!(
        browsed.file_count, 2,
        "…while the snapshot's own count still says 2 — the shortfall is entirely \
         client-side, which is what makes `file_count` a trustworthy witness",
    );

    // The sink. Same client, same snapshot, the restore-audience read.
    let err = keyless.get_for_restore(snapshot_id).await.expect_err(
        "a restore read that would write fewer files than the snapshot holds \
             MUST fail loudly — returning Ok here is the bug verbatim",
    );
    let msg = err.to_string();
    assert!(
        msg.contains('2'),
        "the refusal must name what was dropped so the next unwired consumer can \
         be found from the error alone; got {msg:?}",
    );
}
