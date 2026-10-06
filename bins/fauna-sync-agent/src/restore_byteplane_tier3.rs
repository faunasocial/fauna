//! Tier_3: the cfapi **byte-plane** half of Restore, exercised against a REAL
//! in-process nest chunk store, headless (`docs/goal/behavior/file-sync.md`
//! § Restore — the on-demand byte round-trip).
//!
//! `versions_tier3.rs` (Slice 1) proved the restore **control plane** against a
//! real nest: record → the version projection → `restore_file_version` re-points
//! the local `SyncDb` row at the historical manifest. What it explicitly left
//! open — and what this module closes — is the **byte plane**:
//!
//! > *"the next open re-hydrates the historical bytes, because the hydration
//! > FETCH_DATA resolves its manifest from that same local row"* (§ Restore).
//!
//! The cfapi live harness (`cfapi_live_integration.rs`) fakes
//! `download_file_bytes` with a path→blob map (`DbBackedHost.blobs`), so it
//! **cannot** prove a re-pointed manifest resolves to the right chunks. This
//! module drops that fake for a **real nest chunk store**: a real `fauna-nest`
//! byte plane (`build_router` bound on an ephemeral `127.0.0.1:0` port over a
//! real `BackupService` / `DiskBlobStore`), the exact
//! `conformance_content_key_chunk_route.rs` idiom. So two distinct versions'
//! chunks/manifests really land in a content-addressed store, and a re-point to
//! the older version's manifest re-hydrates the older version's **bytes**.
//!
//! ## What is real here, and what is faked
//!
//! **Real:** the whole byte plane. The nest is a real `fauna-nest` axum router
//! over a real `DiskBlobStore`; the engine is the shipped `SyncEngine` sealing
//! through the real `chunk_crypto` owner-`BackupKey` path and POSTing/​GETting
//! real chunks + manifests over HTTP; `download_file_bytes` resolves the manifest
//! from the real `SyncDb` row and fetches + decrypts the real chunks. The
//! re-point is the shipped `SyncDb::upsert_entry` a restore writes.
//!
//! **Faked:** nothing on the byte plane. `download_file_bytes(rel)` is exactly
//! what `serve_fetch` calls on a cfapi `FETCH_DATA` (`bridge.rs`), so this proves
//! the manifest→chunks resolution the full cfapi path relies on. The one thing
//! *not* exercised here is the cfapi OS shim itself (the `extern "system"`
//! `FETCH_DATA` callback → `CfExecute(TRANSFER_DATA)` → the OS writing the bytes)
//! — that is `restore_byteplane_cfapi` below, which composes this real byte plane
//! with a real sync root. The cross-platform, no-nest engine-level guard of the
//! same claim is `download_file_bytes_test::download_file_bytes_serves_historical_
//! version_after_repoint` (a stateful wiremock store); this is its real-nest twin.
//!
//! ## Run
//!
//! Opt-in (pulls the whole `fauna-nest` crate — off the default test loop, like
//! tier_4):
//!
//! ```text
//! cargo-win.cmd test -p fauna-sync-agent --features tier3-nest restore_byteplane
//! ```

use std::sync::Arc;
use std::time::Duration;

use fauna_core::data::ContentHash;
use fauna_ipc::sync::{BearerToken, SyncCapability};
use fauna_nest::routes::AppState;
use fauna_sync_engine::FileHydrator;
use fauna_sync_engine::always_resident::LocalWriteHost;
use fauna_sync_engine::db::{SyncDb, SyncState};
use fauna_sync_engine::engine::{PlaceholderFold, StaleHydratedRow, SyncEngine};
use fauna_sync_engine::engine_host::CancellationToken;
use fauna_sync_engine::enumerate::{PlaceholderLister, PlaceholderRow};
use tokio::sync::{broadcast, mpsc, watch};

use crate::bridge::{
    CfapiInvalidator, HydrationHost, agent_control_plane, agent_engine_params, run_hydration_loop,
};
use crate::cfapi_live_integration::{dir_from_another_process, read_from_another_process};
use crate::config::SyncPaths;

/// The owner's identity seed. The engine derives its `BackupKey` from this, so
/// the same seed on the upload side and the read side opens the sealed chunks.
const OWNER_SECRET: [u8; 32] = [0x42; 32];
const DEVICE_ID: [u8; 32] = [0x07; 32];

// ─────────────────────────────────────────────────────────────────────
// Real nest byte plane (real chunk routes + BackupService + DiskBlobStore)
// ─────────────────────────────────────────────────────────────────────

/// Start a real in-process nest serving the chunk-store HTTP routes, backed by a
/// real `DiskBlobStore` (no at-rest encryption / compression → the store is an
/// opaque passthrough, exactly what a content-addressed store is to a
/// client-sealed ciphertext). The owner is registered and an HTTP bearer minted
/// for the chunk plane.
///
/// The body is `fauna_nest::test_support::start_test_nest`, reached through
/// `tier3-nest`'s forward of `fauna-nest/test-helpers`; only the handler set and
/// the blob dir are this module's. `tempfile` stays here because it is a
/// dev-only dependency of `fauna-nest`.
async fn start_test_nest(owner_secret: [u8; 32]) -> (String, Arc<AppState>, String) {
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the process; never deleted under test

    let nest = fauna_nest::test_support::start_test_nest(owner_secret, Some(blob_path), |b| {
        fauna_nest::sync_handlers::register_sync_handlers(b);
        fauna_nest::folder_handlers::register_folders_handlers(b);
    })
    .await;
    (nest.base_url, nest.state, nest.http_token)
}

/// Build an owner-only `backup_key` `SyncEngine` pointed at `dest_url` — the
/// unbound, owner-scoped shape (the ordinary single-user folder / backup
/// deployment): `backup_key = Some(BackupKey::derive(seed))`, no group, no
/// content keys. The chunk plane rides a static-bearer HTTP `SyncClient`.
fn backup_engine(dest_url: &str, http_token: &str, watch_path: std::path::PathBuf) -> SyncEngine {
    fauna_nest::test_support::backup_engine(fauna_nest::test_support::EngineFixture {
        dest_url,
        http_token,
        owner_secret: OWNER_SECRET,
        device_id: DEVICE_ID,
        watch_path,
    })
}

// ─────────────────────────────────────────────────────────────────────
// Engine-level: a re-pointed row re-hydrates the HISTORICAL version's bytes
// ─────────────────────────────────────────────────────────────────────

/// Two versions of a file are uploaded through the real chunk route (their
/// chunks + manifests really land in a `DiskBlobStore`). The `SyncDb` row is then
/// re-pointed from the newer version's manifest back to the OLDER version's —
/// exactly what `pipe_server::repoint_entry` writes on an on-demand restore — and
/// `download_file_bytes(rel)` (what `serve_fetch` calls on a cfapi `FETCH_DATA`)
/// must re-hydrate the OLDER version's bytes, resolving them from the real store.
///
/// This is the real-nest twin of the cross-platform wiremock guard
/// `download_file_bytes_test::download_file_bytes_serves_historical_version_after_
/// repoint`: same claim, but against the production `/api/v1/{chunks,manifests}`
/// routes, so the "engine resolves X ∧ mock serves X" composition is replaced by
/// a single end-to-end assertion against the real byte plane.
#[tokio::test]
async fn restore_repoint_rehydrates_historical_bytes_against_real_nest() {
    let (url, _state, token) = start_test_nest(OWNER_SECRET).await;

    let watch = tempfile::tempdir().unwrap();
    let engine = backup_engine(&url, &token, watch.path().to_path_buf());

    let rel = "docs/report.bin";

    // Version 1 (the historical version we restore to). Multi-chunk (>64 KiB
    // forces FastCDC boundaries), under the 64 MiB streaming threshold so the
    // in-memory reassembly path runs. `upload_bytes` returns the manifest hash.
    let v1: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(13) % 251) as u8)
        .collect();
    let manifest_v1 = engine
        .upload_bytes(v1.clone(), rel, "__backup")
        .await
        .expect("upload v1 through the real chunk route");

    // Version 2 (a later edit — the head). Both versions' chunks + manifests now
    // coexist in the real store.
    let v2: Vec<u8> = (0..220_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();
    let manifest_v2 = engine
        .upload_bytes(v2.clone(), rel, "__backup")
        .await
        .expect("upload v2 through the real chunk route");
    assert_ne!(
        manifest_v1, manifest_v2,
        "two distinct versions must produce two distinct manifests"
    );

    // Restore to v1: re-point the row at v1's manifest and free the bytes — the
    // row `pipe_server::repoint_entry` writes on an on-demand client
    // (`manifest_hash` re-pointed, `local/remote_hash = None`, Placeholder, size =
    // v1's, owner-only so `content_key_version = None`). `download_file_bytes`
    // reads only `manifest_hash` + `content_key_version` from the row.
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(manifest_v1),
            SyncState::Placeholder,
            0,
            1,
            v1.len() as i64,
            1,
            None,
        )
        .expect("re-point row at v1's manifest");

    // The next open resolves v1's manifest from the row → fetches v1's chunks from
    // the real store → decrypts under the owner BackupKey → reassembles v1's
    // bytes, NOT the v2 head that also sits in the store.
    let bytes = engine
        .download_file_bytes(rel)
        .await
        .expect("download_file_bytes after restore re-point");
    assert_eq!(
        bytes, v1,
        "a restore must re-hydrate the historical (v1) bytes from the real nest store"
    );
    assert_ne!(bytes, v2, "the pre-restore head (v2) must NOT be served");
}

// ─────────────────────────────────────────────────────────────────────
// Full cfapi FETCH_DATA: a restored on-demand file re-hydrates historical bytes
// ─────────────────────────────────────────────────────────────────────

/// The folder the cfapi sync root binds to (matches `cfapi_live_integration`).
const FOLDER: &str = "docs";
/// The bound set's identity: the agent keys its engine and state DB by it.
const FOLDER_REF: fauna_core::folder_keys::FolderRef = fauna_core::folder_keys::FolderRef::Local(1);
/// The engine's `BackupKey` bytes for the cfapi test — one engine both uploads
/// and serves, so the seal key is consistent by construction.
const CFAPI_BACKUP_KEY: [u8; 32] = [0x3c; 32];

/// A [`HydrationHost`] that is a **pure decorator over a real [`SyncEngine`]** —
/// unlike `cfapi_live_integration::DbBackedHost`, it does **not** fake
/// `download_file_bytes`: it delegates it to the engine, so a FETCH_DATA resolves
/// the manifest from the real `SyncDb` row and fetches the chunks from the real
/// nest. `prepare`/`repull` are no-ops because the placeholder row is pre-seeded
/// (the restored state); the loop needs no live `changes.list` fold.
struct RealNestHost {
    engine: SyncEngine,
    /// Kept alive so the loop's reconnect arm stays pending; no test drives one.
    reconnect: (watch::Sender<u64>, watch::Receiver<u64>),
}

#[async_trait::async_trait(?Send)]
impl FileHydrator for RealNestHost {
    async fn download_file_bytes(&self, relative_path: &str) -> anyhow::Result<Vec<u8>> {
        // The point of this whole module: the REAL engine resolves the row's
        // (re-pointed) manifest and fetches its chunks from the real nest.
        FileHydrator::download_file_bytes(&self.engine, relative_path).await
    }
}

#[async_trait::async_trait(?Send)]
impl PlaceholderLister for RealNestHost {
    async fn list_placeholder_rows(&self) -> anyhow::Result<Vec<PlaceholderRow>> {
        self.engine.list_placeholder_rows().await
    }
}

#[async_trait::async_trait(?Send)]
impl LocalWriteHost for RealNestHost {
    fn is_ignored(&self, rel: &str) -> bool {
        LocalWriteHost::is_ignored(&self.engine, rel)
    }
    fn was_recent_download(&self, rel: &str) -> bool {
        LocalWriteHost::was_recent_download(&self.engine, rel)
    }
    fn was_recent_removal(&self, rel: &str) -> bool {
        LocalWriteHost::was_recent_removal(&self.engine, rel)
    }
    async fn upload_file(
        &self,
        rel: &str,
    ) -> anyhow::Result<fauna_sync_engine::engine::UploadOutcome> {
        LocalWriteHost::upload_file(&self.engine, rel).await
    }
    async fn handle_delete(&self, rel: &str) -> anyhow::Result<()> {
        LocalWriteHost::handle_delete(&self.engine, rel).await
    }
    async fn converge(&self, folder: &str) -> Vec<String> {
        LocalWriteHost::converge(&self.engine, folder).await
    }
}

#[async_trait::async_trait(?Send)]
impl HydrationHost for RealNestHost {
    async fn prepare(&self) -> anyhow::Result<PlaceholderFold> {
        Ok(PlaceholderFold::default())
    }
    async fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> anyhow::Result<()> {
        HydrationHost::mark_hydrated(&self.engine, rel, content_hash).await
    }
    // An earlier passadded `mark_placeholder` to the trait but not to this
    // feature-gated impl — the `tier3-nest` build was silently broken until the
    // pin-reaction track next compiled it. Delegate like every other method.
    async fn mark_placeholder(&self, rel: &str) -> anyhow::Result<()> {
        HydrationHost::mark_placeholder(&self.engine, rel).await
    }
    async fn mark_seen(&self, rels: &[String]) -> anyhow::Result<()> {
        HydrationHost::mark_seen(&self.engine, rels).await
    }
    async fn clear_seen_all(&self) -> anyhow::Result<()> {
        HydrationHost::clear_seen_all(&self.engine).await
    }
    async fn repoint_placeholder(&self, row: &StaleHydratedRow) -> anyhow::Result<()> {
        HydrationHost::repoint_placeholder(&self.engine, row).await
    }
    async fn repull(&self) -> anyhow::Result<PlaceholderFold> {
        Ok(PlaceholderFold::default())
    }
    // The restore byte-plane vehicle drives one hydrate against a real nest; the
    // corpus passes are not what it measures and would re-record nothing here (a
    // fresh engine over an unbound row set). A deliberate no-op, owned here.
    async fn converge_corpus_at_start(&self, _folder: &str) {}
    async fn refresh_and_converge_corpus(&self, _folder: &str) {}
    async fn answer_engine_command(&self, cmd: fauna_sync_engine::always_resident::EngineCommand) {
        HydrationHost::answer_engine_command(&self.engine, cmd).await
    }
    async fn rescan_interval(&self) -> Duration {
        Duration::from_secs(3600)
    }
    fn reconnects(&self) -> watch::Receiver<u64> {
        self.reconnect.1.clone()
    }

    async fn is_dehydration_safe(&self, rel: &str) -> bool {
        self.engine.is_dehydration_safe(rel)
    }

    async fn subtree_fully_synced(&self, dir_rel: &str) -> bool {
        self.engine.subtree_fully_synced(dir_rel).unwrap_or(false)
    }
}

/// **The NEXT's literal success line.** A tracked file has two versions in a real
/// nest chunk store; it is restored to v1 (its `SyncDb` row re-pointed at v1's
/// manifest, a placeholder); a subsequent open **from another process** fires a
/// real cfapi FETCH_DATA, which resolves v1's manifest from the row and
/// re-hydrates **v1's bytes** — asserted headlessly (`file-sync.md` § Restore:
/// "the next open re-hydrates the historical bytes, because the hydration
/// FETCH_DATA resolves its manifest from that same local row").
///
/// One engine both uploads (so v1/v2 land in the real store, sealed) and serves
/// (so the seal key matches). The byte plane is the real `/api/v1/{chunks,
/// manifests}` routes; `download_file_bytes` is NOT faked (unlike `DbBackedHost`)
/// — the whole point is that the manifest→chunks resolution is real. This is the
/// cfapi twin of `restore_repoint_rehydrates_historical_bytes_against_real_nest`
/// (which stops at the engine boundary); here the OS shim runs end to end.
#[tokio::test]
async fn restore_byteplane_cfapi_fetch_data_serves_historical_bytes() {
    let (url, _state, token) = start_test_nest(OWNER_SECRET).await;

    // A real cfapi sync root + a per-folder SyncDb path. run_hydration_loop is
    // driven directly (no IPC / SyncServiceState), so only the paths are needed.
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).expect("create sync root");
    let db_path = paths.sync_db_path_for_ref(FOLDER_REF);
    std::fs::create_dir_all(db_path.parent().unwrap()).expect("create db dir");

    // One engine: real nest URL, real bearer (for the upload POSTs), a fixed
    // BackupKey. It both uploads v1/v2 and (after being moved into the host)
    // serves FETCH_DATA — same seal key by construction.
    let capability = crate::bearer::CapabilitySlot::new(Some(SyncCapability::new(
        vec![9u8; 32],
        vec![7u8; 32],
        url.clone(),
        "restore-byteplane-cfapi".into(),
        BearerToken::new(token.clone(), 4_000_000_000),
    )));
    // Assembled by the shared builder's construction half from the agent's own
    // inputs — the engine the driver builds, minus the row read (owner-only,
    // same-nest; the folder's state DB is `db_path`, under the data-root).
    let engine = fauna_sync_engine::engine_lifecycle::assemble_engine(
        agent_engine_params(
            agent_control_plane(capability, url.clone(), [9u8; 32]),
            paths.base_dir(),
            root.clone(), // watch_dir
            FOLDER_REF,
            [7u8; 32],                                    // device_id
            CFAPI_BACKUP_KEY,                             // backup_key bytes
            &crate::bridge::AgentPredecessors::default(), // no succession in this fixture
            None,                                         // progress_tx
            fauna_sync_engine::access_gate::AccessGate::new(),
            None, // change_signer
            std::sync::Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
        ),
        fauna_sync_engine::engine_lifecycle::ResolvedBinding {
            folder: FOLDER.to_string(),
            ..Default::default()
        },
    )
    .expect("build the production SyncEngine against the real nest")
    .engine;

    let rel = "report.bin";
    // Version 1 (restore target) + version 2 (the head) — both land in the real
    // store. `upload_bytes` returns the manifest hash and needs no file on disk.
    let v1: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(13) % 251) as u8)
        .collect();
    let manifest_v1 = engine
        .upload_bytes(v1.clone(), rel, "__backup")
        .await
        .expect("upload v1 to the real nest");
    let v2: Vec<u8> = (0..220_000u32)
        .map(|i| (i.wrapping_mul(29) % 251) as u8)
        .collect();
    let manifest_v2 = engine
        .upload_bytes(v2.clone(), rel, "__backup")
        .await
        .expect("upload v2 to the real nest");
    assert_ne!(manifest_v1, manifest_v2);

    // Restore to v1: seed this device's row as a Placeholder re-pointed at v1's
    // manifest (what `pipe_server::repoint_entry` writes; owner-only ⇒ ckv None).
    // A separate SyncDb connection — WAL makes the write visible to the engine's
    // own connection when the FETCH_DATA read opens it.
    {
        let db = SyncDb::open(&db_path).expect("open per-folder db");
        db.upsert_entry(
            rel,
            None,
            None,
            Some(manifest_v1),
            SyncState::Placeholder,
            0,
            1,
            v1.len() as i64,
            1,
            None,
        )
        .expect("seed the restored placeholder row");
    }

    // Serve the root with the engine (download_file_bytes delegated, real).
    let host = RealNestHost {
        engine,
        reconnect: watch::channel(0),
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root_for_loop = root.clone();
    let root_str = root.to_string_lossy().to_string();
    let dst = tmp.path().join("hydrated-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root_for_loop,
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let placeholder = format!("{root_str}\\{rel}");
        let dst2 = dst.clone();
        // Browse (creates the placeholder) then open it — both from another
        // process, both driving real cfapi callbacks.
        let bytes = tokio::task::spawn_blocking(move || {
            dir_from_another_process(&root_str);
            read_from_another_process(&placeholder, &dst2)
        })
        .await
        .expect("hydrate task");

        assert_eq!(
            bytes, v1,
            "cfapi FETCH_DATA must re-hydrate the restored (v1) bytes from the real nest — \
             resolving the re-pointed manifest from the row and fetching v1's chunks"
        );
        assert_ne!(bytes, v2, "the pre-restore head (v2) must NOT be served");

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}
