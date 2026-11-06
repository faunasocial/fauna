//! Snapshot restore for the FFI apps that build no engine host: fetch **one**
//! file's decrypted bytes out of a snapshot (`snapshot-file-download-button[i]`,
//! [`download_snapshot_file_bytes`]), or restore the **whole** snapshot into a
//! directory (`snapshot-restore-button`, [`restore_snapshot_to_dir`]).
//!
//! The per-file fn is the FFI twin of the wasm `downloadSnapshotFileBytes`
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
//!
//! The whole-snapshot fn is windows' seam for `snapshot-restore-button`
//! (`backups.md` § Where logic lives → *Whole-snapshot restore*): the same thin
//! adapter over the shared `restore_snapshot_into_dir` that the apple host's
//! `FfiSyncEngineHost::restore_snapshot_to_dir` is, returning the same
//! [`FfiRestoreSummary`] — no second shape.

use std::future::Future;
use std::sync::Arc;
use std::thread;

use fauna_client::AuthClient;
use fauna_sync_engine::engine_lifecycle::{
    build_restore_engine, resolve_restore_destination, restore_snapshot_into_dir,
    snapshot_files_to_restore,
};
use tokio::runtime::Builder;
use tokio::sync::oneshot;

use crate::FfiError;
use crate::FfiRestoreSummary;
use crate::crypto::{device32, secret32};
use crate::nest_client::FfiNestClient;

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
    let set = fauna_client_snapshots::SnapshotSet::of(&snapshot);
    // The snapshot reader's nonce source: this seat's custody — the resolver
    // the listing read above already rendered sealed paths through.
    let snapshot_nonces = crate::snapshots_client::folder_key_resolver(&nest_rpc, &secret)
        .map(fauna_client_sync::SetNonceSource::Resolver);

    let nest_url = nest_rpc.nest_url().to_string();
    on_one_shot_worker("snapshot-download", move || async move {
        let engine = build_restore_engine(
            owner_auth(nest_url, secret),
            nest_rpc,
            device,
            &hex::encode(secret),
            snapshot_nonces,
        );
        // The ONE snapshot entry reader, per-file door
        // (`backup-restore.md` § Security Properties → Integrity): the entry,
        // sealed or plaintext, licensed by the judged change row of the
        // snapshot's set that named its manifest under its path, before any
        // byte.
        engine
            .download_snapshot_entry_bytes(&set, &file)
            .await
            .map_err(|e| format!("download {path:?} from snapshot {snapshot_id}: {e:#}"))
    })
    .await
}

/// Restore a **folder** snapshot's whole file tree into `output_dir` via the
/// shared client-side walk — windows' `snapshot-restore-button`
/// (`docs/goal/ui/backups.md` § Whole-snapshot restore).
///
/// A thin adapter over the ONE whole-snapshot entry point,
/// [`restore_snapshot_into_dir`], exactly as the apple host's
/// `FfiSyncEngineHost::restore_snapshot_to_dir` is: the listing is read
/// custody-wired over `get_for_restore` (a custody gap fails loudly, never an
/// empty directory reported as success), a **sealed** snapshot restores here
/// because the walk runs where the owner `BackupKey` lives, and the per-file
/// `manifest_hash` never crosses the FFI. `output_dir` is the resolved
/// destination ([`resolve_snapshot_restore_destination`]); a missing directory
/// is created.
///
/// **A partial result is a result, not an error:** files that failed to fetch,
/// open or verify come back in [`FfiRestoreSummary::unverified`] with
/// everything else written, and the page words the line through the shared
/// `snapshot_restore_result_text` (`ui/backups.md` § Whole-snapshot restore →
/// *Outcome*).
///
/// # Errors
///
/// `FfiError::General` for a failure **before** the walk — `owner_secret` /
/// `device_id` not 32 bytes, `output_dir` naming something that is not a
/// directory, the listing read failing or short, a message-kind (`mail` /
/// `calendar` / `conv`) snapshot (it restores via
/// `fauna.filesync.snapshot.restore_message_kind`) — carrying the reason. The
/// page paints it in the same `snapshot-restore-result` line, never
/// `error-message`.
#[fauna_uniffi_async::export]
pub async fn restore_snapshot_to_dir(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    device_id: Vec<u8>,
    snapshot_id: i64,
    output_dir: String,
) -> Result<FfiRestoreSummary, FfiError> {
    let secret = secret32(&owner_secret)?;
    let device = device32(&device_id)?;

    let nest_rpc = nest.nest_arc();
    // This seat's custody — what opens the snapshot's sealed paths for the
    // listing read and serves the snapshot reader's nonces.
    let folder_keys = crate::snapshots_client::folder_key_resolver(&nest_rpc, &secret);
    let nest_url = nest_rpc.nest_url().to_string();
    let summary = on_one_shot_worker("snapshot-restore", move || async move {
        restore_snapshot_into_dir(
            owner_auth(nest_url, secret),
            nest_rpc,
            device,
            &secret,
            folder_keys,
            snapshot_id,
            std::path::Path::new(&output_dir),
        )
        .await
        .map_err(|e| format!("{e:#}"))
    })
    .await?;
    Ok(FfiRestoreSummary {
        files_restored: summary.files_restored,
        bytes_written: summary.bytes_written,
        skipped: summary.skipped,
        unverified: summary.unverified,
    })
}

/// `snapshot-restore-destination-input`'s typed path → the directory
/// [`restore_snapshot_to_dir`] writes into, through the shared
/// [`resolve_restore_destination`] tui and linux call: a `~/` prefix (or a bare
/// `~`) names the home directory, anything else must already be absolute.
/// `None` = not a usable destination — the page says so in
/// `snapshot-restore-result` before any fetch.
#[uniffi::export]
pub fn resolve_snapshot_restore_destination(typed: String) -> Option<String> {
    // tui's reading of "home": `HOME`, else the Windows profile directory.
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    resolve_restore_destination(&typed, home).map(|p| p.to_string_lossy().into_owned())
}

/// The auth client both walks build beside the app's already-connected WS-RPC
/// control plane (it authenticates lazily on first request), matching how the
/// engine host splits these two.
fn owner_auth(nest_url: String, secret: [u8; 32]) -> Arc<AuthClient> {
    Arc::new(AuthClient::new(
        nest_url,
        fauna_core::identity::ActorKeypair::from_secret(secret),
    ))
}

/// Run one restore walk on a dedicated worker thread and await its reply.
///
/// The walk is `!Send` — the throwaway `SyncEngine` owns a rusqlite `SyncDb`,
/// whose connection must be created, used and dropped on ONE thread. So it runs
/// on its own thread with a current-thread runtime and replies over a oneshot,
/// leaving the exported future `Send` as UniFFI requires. Same
/// construct-run-drop shape as the engine host's `request_worker`; one-shot
/// because neither walk has anything to keep resident. `label` names the thread
/// and every error this plumbing raises.
async fn on_one_shot_worker<T, F, Fut>(label: &'static str, work: F) -> Result<T, FfiError>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, String>>,
{
    let (tx, rx) = oneshot::channel();
    thread::Builder::new()
        .name(format!("fauna-{label}"))
        .spawn(move || {
            let result = match Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt.block_on(work()),
                Err(e) => Err(format!("build {label} runtime: {e}")),
            };
            let _ = tx.send(result);
        })
        .map_err(|e| FfiError::General {
            msg: format!("spawn {label} worker: {e}"),
        })?;

    rx.await
        .map_err(|_| FfiError::General {
            msg: format!("{label} worker exited without replying"),
        })?
        .map_err(|msg| FfiError::General { msg })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typed destination resolves through the shared rule: an absolute path
    /// passes through, a relative one is not a destination.
    #[test]
    fn a_typed_destination_must_be_absolute() {
        let absolute = if cfg!(windows) {
            r"C:\srv\restored"
        } else {
            "/srv/restored"
        };
        assert_eq!(
            resolve_snapshot_restore_destination(format!("  {absolute} ")),
            Some(absolute.to_string())
        );
        assert_eq!(
            resolve_snapshot_restore_destination("restored".to_string()),
            None
        );
    }
}
