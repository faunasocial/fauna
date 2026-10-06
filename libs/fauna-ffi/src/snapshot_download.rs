//! Single-file restore for the native apps: fetch **one** file's decrypted
//! bytes out of a snapshot (`snapshot-file-download-button[i]`).
//!
//! The FFI twin of the wasm `downloadSnapshotFileBytes`
//! (`libs/fauna-wasm/src/snapshot_download.rs`), over the same shared
//! `fauna_core::file_download` walk — `docs/goal/ui/backups.md` § Where logic
//! lives → *Single-file byte download* (re-ratified 2026-07-14). Both bindings
//! exist only to reach the one walk from their own runtime; the invariants (seal
//! discriminator, key precedence, decompression, whole-file verify) live in the
//! walk, never here.
//!
//! **A free fn, not an `FfiSyncEngineHost` method** — deliberately. The engine
//! host is an apple-only object (windows and android never build one), and the
//! ratified native seam for the Backups page is the thin free-fn family in
//! `backup_destinations.rs` (`backups.md` § Where logic lives → *Backup-destination
//! management*: "not a shared machine"). A free fn is reachable from all three
//! FFI clients; a method would have served apple alone (priority #1).

use std::sync::Arc;
use std::thread;

use fauna_client::AuthClient;
use fauna_sync_engine::engine_lifecycle::build_restore_engine;
use tokio::runtime::Builder;
use tokio::sync::oneshot;

use crate::FfiError;
use crate::crypto::{device32, secret32};
use crate::nest_client::FfiNestClient;
use crate::sync_engine_host::snapshot_files_to_restore;

/// Fetch one snapshot file's decrypted bytes via the shared client-side walk.
///
/// `path` is resolved against the snapshot **server-side**, so the file's
/// `manifest_hash` never crosses the FFI — the same posture as
/// [`crate::FfiSyncEngineHost::restore_snapshot_to_dir`], the full-restore
/// sibling this mirrors. The bytes come back in memory rather than being written
/// here: where they land is the caller's platform-native save step (file-save
/// dialog on desktop, save/share sheet on mobile), the only part of this leg that
/// is client glue.
///
/// A **sealed** snapshot decrypts correctly here, because the walk runs where the
/// owner `BackupKey` lives.
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` / `device_id` are not 32 bytes.
/// - `FfiError::General` if the snapshot is a message-kind (`mail` / `calendar` /
///   `conv`) snapshot — those restore via
///   `fauna.filesync.snapshot.restore_message_kind`, not a file walk.
/// - `FfiError::General` if the snapshot has no **regular** file at `path`, or
///   carrying the fetch/decrypt/verify error chain from the walk.
#[fauna_uniffi_async::export]
pub async fn download_snapshot_file_bytes(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    device_id: Vec<u8>,
    snapshot_id: i64,
    path: String,
) -> Result<Vec<u8>, FfiError> {
    let secret = secret32(&owner_secret)?;
    let device = device32(&device_id)?;

    let nest_rpc = nest.nest_arc();
    // Custody-wired from the `owner_secret` this fn already takes — NOT the bare
    // `SnapshotsClient::new` it used to build. Keyless,
    // every sealed row rendered to `Omit` and was dropped, so the `find` below
    // missed and this returned "snapshot N has no file at {path}" — for a path
    // the (custody-wired) list face had just rendered on screen. The class
    // definition satisfied inside a single function: the key was in hand at the
    // top and spent on the chunk walk at the bottom, with the row read in the
    // middle reaching for neither.
    let snapshot =
        crate::snapshots_client::owner_read_snapshots_client(Arc::clone(&nest_rpc), &secret)
            .get_for_restore(snapshot_id)
            .await
            .map_err(|e| FfiError::General {
                msg: format!("fetch snapshot {snapshot_id}: {e}"),
            })?;
    if let Some(kind) = &snapshot.message_kind {
        return Err(FfiError::General {
            msg: format!(
                "snapshot {snapshot_id} is a '{kind}' message-kind snapshot — it has no \
                 per-file byte walk; restore it via fauna.filesync.snapshot.restore_message_kind"
            ),
        });
    }

    // Narrow to the requested entry BEFORE mapping, then reuse the full-restore
    // mapper on just that one. Reusing it keeps the `file_type == "regular"`
    // predicate and the 32-byte manifest-hash check single-sourced across the two
    // restore legs (the nest's value is `"regular"`, NOT `"file"` —
    // `bins/fauna-nest/src/db/sync_storage.rs`; a `"file"` literal silently matches
    // nothing). Mapping the whole snapshot first would instead make one corrupt
    // *unrelated* entry fail an otherwise-fine download, and allocate every file's
    // entry per click.
    let entry = snapshot
        .files
        .iter()
        .find(|f| f.path == path)
        .ok_or_else(|| FfiError::General {
            msg: format!("snapshot {snapshot_id} has no file at {path:?}"),
        })?;
    let file = snapshot_files_to_restore(std::slice::from_ref(entry))
        .map_err(|msg| FfiError::General { msg })?
        .into_iter()
        .next()
        .ok_or_else(|| FfiError::General {
            msg: format!(
                "snapshot {snapshot_id}: {path:?} is not a regular file — only regular \
                 files carry a restorable manifest"
            ),
        })?;

    // The walk itself is `!Send` — the throwaway `SyncEngine` owns a rusqlite
    // `SyncDb`, whose connection must be created, used and dropped on ONE thread.
    // So it runs on a dedicated worker with its own current-thread runtime and
    // replies over a oneshot, leaving this exported future `Send` as UniFFI
    // requires. Same construct-run-drop shape as the engine host's
    // `request_worker`; one-shot here because a per-file download has nothing
    // to keep resident.
    let (tx, rx) = oneshot::channel();
    let nest_url = nest_rpc.nest_url().to_string();
    thread::Builder::new()
        .name("fauna-snapshot-download".to_string())
        .spawn(move || {
            let rt = match Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("build snapshot-download runtime: {e}")));
                    return;
                }
            };
            let result = rt.block_on(async move {
                // Auth is built alongside the app's already-connected WS-RPC
                // control plane (it authenticates lazily on first request),
                // matching how the engine host splits these two.
                let auth = Arc::new(AuthClient::new(
                    nest_url,
                    fauna_core::identity::ActorKeypair::from_secret(secret),
                ));
                let engine = build_restore_engine(auth, nest_rpc, device, &hex::encode(secret));
                engine
                    .download_file_bytes_by_manifest(
                        file.manifest_hash,
                        file.content_key_version,
                        &file.relative_path,
                    )
                    .await
                    .map_err(|e| format!("download {path:?} from snapshot {snapshot_id}: {e:#}"))
            });
            let _ = tx.send(result);
        })
        .map_err(|e| FfiError::General {
            msg: format!("spawn snapshot-download worker: {e}"),
        })?;

    rx.await
        .map_err(|_| FfiError::General {
            msg: "snapshot-download worker exited without replying".to_string(),
        })?
        .map_err(|msg| FfiError::General { msg })
}
