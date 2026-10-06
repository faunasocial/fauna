//! The **file half** of mailbox-export reclaim (`docs/goal/behavior/mail-export.md`
//! § Reclaim: the row is the authority, the file follows).
//!
//! `db::mail_export` owns the `export_sessions` rows; this module owns what
//! happens to the sealed blob files those rows name, for the three lifecycle
//! events no session RPC is part of:
//!
//! - **Account deletion** — [`unlink_export_blobs_for_actor`], called by
//!   `pending_actions::finalize_user_deletion` *before* the registry sweep
//!   deletes the rows.
//! - **Identity succession** — the registry burns the retired identity's rows
//!   inside the ceremony transaction (`db::actor_tables`, the
//!   `export_sessions` entry), which can unlink nothing;
//!   [`reclaim_orphaned_export_blobs`] collects the files afterwards.
//! - **Expiry** (§ Expiry) — [`run_export_expiry_tick`], on the nest's
//!   periodic expiry loop: every session past `expires_at` loses its file and
//!   then its row, and the orphan reclaim runs on the same tick, so neither an
//!   expired blob nor an unnamed one outlives the next tick.
//!
//! # The one rule both directions rest on
//!
//! **A file under `<data-dir>/exports/` that no `export_sessions.blob_path`
//! names is garbage.** That makes every ordering mistake recoverable instead of
//! a permanent leak of a whole-mailbox snapshot: a crash between an unlink and
//! its row delete, a burn that could not touch the disk, a cold resume's
//! disowned generation — each leaves an orphan, and
//! the orphan reclaim finds it by listing the directory rather than by asking
//! a table that has already forgotten.
//!
//! The rule is only safe because of its converse, which is a contract on the
//! blob **writer**: *the row is committed before the file is created*
//! (`create_export_session` takes `blob_path` for exactly this reason). A
//! writer that created the file first would race the reclaim and lose its blob.
//!
//! `blob_path` is the nest-relative path **of the file itself**, verbatim —
//! whatever suffix the writer gives the file on disk is part of the recorded
//! path. Nothing here appends one.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::db::CacheDb;
use crate::routes::AppState;

/// The one directory, relative to the nest data dir, an export blob may live in.
pub const EXPORT_BLOB_DIR: &str = "exports";

/// Resolve a stored `blob_path` to the file it names — or `None` when it is not
/// the shape this nest mints: exactly `exports/<one plain file name>`.
///
/// The path is nest-minted, never client-supplied, so a refusal here is not an
/// expected outcome; it is the floor under a future regression. This module's
/// callers turn a stored string into an `unlink`, and the one thing a bug
/// upstream must never buy is an unlink outside the export directory. So the
/// check is structural (path components, not substring tests): no absolute
/// path, no `..`, no nesting, no empty name.
pub fn resolve_export_blob(data_dir: &Path, blob_path: &str) -> Option<PathBuf> {
    let mut parts = Path::new(blob_path).components();
    let (Some(Component::Normal(dir)), Some(Component::Normal(file)), None) =
        (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    (dir == EXPORT_BLOB_DIR).then(|| data_dir.join(dir).join(file))
}

/// Mint the session's blob path from its id — the one place the suffix
/// § Blob shape on disk pins is spelled, so nothing else has to know it.
///
/// The minted string is what `create_export_session` records in `blob_path`,
/// verbatim, and what [`resolve_export_blob`] later turns back into a file.
pub fn export_blob_path_for(session_id: &str) -> String {
    export_blob_path_for_generation(session_id, 0)
}

/// [`export_blob_path_for`] for a given stream generation (§ Resume: a cold
/// resume restarts the stream in a file of its own, never by truncating the
/// old one). Generation 0 keeps the un-numbered name every pre-restart session
/// already rests under; generation *n* ≥ 1 is `<session-id>.<n>.zip.zst.sealed`.
///
/// A distinct name per generation is what lets a restart be one row UPDATE with
/// no lock shared with the up-leg: an upload that reserved under the old
/// generation appends to the OLD name, which no row names any more, so it can
/// never put a stale frame ahead of the new stream's frame 0.
pub fn export_blob_path_for_generation(session_id: &str, generation: u64) -> String {
    match generation {
        0 => format!("{EXPORT_BLOB_DIR}/{session_id}.zip.zst.sealed"),
        n => format!("{EXPORT_BLOB_DIR}/{session_id}.{n}.zip.zst.sealed"),
    }
}

/// Append one already-sealed frame to the session's blob
/// (`mail-export.md` § Blob shape on disk).
///
/// ⚠ **The bytes are opaque and stay that way.** The nest holds no session key
/// and parses no frame — it concatenates what the client sealed, in the order
/// the store's `next_chunk_idx` guard admitted. So there is nothing to
/// validate here, and validating anything would be the beginning of the
/// nest-side conversion § Export pipeline records as the rejected alternative.
///
/// The caller must have taken the reservation first
/// (`CacheDb::append_export_blob_bytes` returning `Appended`): that is what
/// enforces the ceiling and the frame order, and doing it in the other order
/// would let a refused chunk leave bytes on disk the ceiling never counted.
///
/// Creates the export directory on first use — a nest that has never exported
/// has no `exports/`, which is also why [`reclaim_orphaned_export_blobs`]
/// treats a missing directory as success.
///
/// ⚠ **Only frame 0 creates the file** (`chunk_idx == 0`; § Resume). A later
/// frame that finds no file is not a blob to begin: it is an upload into a
/// stream generation a restart has already unlinked, an upload racing a cancel,
/// or a crash that lost frame 0 after its reservation. Creating the file there
/// would seed a blob that starts mid-stream — one that fails AEAD at download,
/// hours later, naming nothing — and, in the restart and cancel cases, would
/// resurrect a file the nest has just deliberately removed.
pub async fn append_export_blob(
    data_dir: &Path,
    blob_path: &str,
    chunk_idx: u64,
    sealed: &[u8],
) -> Result<()> {
    let file = resolve_export_blob(data_dir, blob_path)
        .with_context(|| format!("export blob path {blob_path} is not the minted shape"))?;
    let dir = data_dir.join(EXPORT_BLOB_DIR);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    let mut f = tokio::fs::OpenOptions::new()
        .create(chunk_idx == 0)
        .append(true)
        .open(&file)
        .await
        .with_context(|| format!("open export blob {}", file.display()))?;
    use tokio::io::AsyncWriteExt;
    f.write_all(sealed)
        .await
        .with_context(|| format!("append to export blob {}", file.display()))?;
    f.flush()
        .await
        .with_context(|| format!("flush export blob {}", file.display()))?;
    Ok(())
}

/// Unlink one session's blob, ahead of deleting its row (§ Reclaim rule 2:
/// file before row, on the way out).
///
/// Returns whether a file was actually removed. A path that is not the minted
/// shape answers `Ok(false)` after logging — the same posture
/// [`unlink_export_blobs_for_actor`] takes, and for the same reason: nothing
/// under `exports/` can be leaked by refusing, because a real blob there is
/// either named by a well-formed row or is an orphan rule 3 collects.
pub async fn unlink_export_blob(data_dir: &Path, blob_path: &str) -> Result<bool> {
    let Some(file) = resolve_export_blob(data_dir, blob_path) else {
        tracing::error!(
            blob_path,
            "export blob path is outside the export directory's minted shape; refusing to unlink"
        );
        return Ok(false);
    };
    unlink_if_present(&file)
        .await
        .with_context(|| format!("unlink export blob {blob_path}"))
}

/// Remove one file, treating "already gone" as success — every caller here is
/// retried or re-run, so the second attempt must not fail on the first one's
/// progress.
async fn unlink_if_present(file: &Path) -> std::io::Result<bool> {
    match tokio::fs::remove_file(file).await {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Account deletion's blob leg: unlink every export blob `actor`'s sessions
/// name. Returns how many files were removed.
///
/// **Runs BEFORE the rows are purged, and fails the deletion if an unlink
/// fails.** Unlink-then-delete is the recoverable order: a file that would not
/// go leaves its row standing, `finalize_user_deletion` returns the error, the
/// pending action is never marked executed, and the next tick finds the same
/// row and tries again. The reverse order deletes the only record of where the
/// blob is — for an account that will never call again.
///
/// A stored path that is not the minted shape is **skipped, loudly, and does
/// not block the deletion**: refusing forever would make the account
/// undeletable over a string this nest should never have written, and nothing
/// under `exports/` can be leaked by it (a real blob there is either named by
/// a well-formed row or is an orphan the reclaim collects).
pub async fn unlink_export_blobs_for_actor(state: &Arc<AppState>, actor: &[u8; 32]) -> Result<u64> {
    let paths = state.db.export_blob_paths_for_actor(actor).await?;
    // The common case by a wide margin, and the one every deletion test that
    // runs on a data-dir-less `AppState::for_test` takes.
    if paths.iter().all(String::is_empty) {
        return Ok(0);
    }
    let data_dir = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path)
        .context("export blobs to unlink, but the nest data dir cannot be derived from db_path")?;

    let mut unlinked = 0u64;
    for blob_path in paths.iter().filter(|p| !p.is_empty()) {
        let Some(file) = resolve_export_blob(&data_dir, blob_path) else {
            tracing::error!(
                actor_id = %hex::encode(actor),
                blob_path,
                "account deletion: an export session names a blob path outside the export \
                 directory's minted shape; refusing to unlink it"
            );
            continue;
        };
        if unlink_if_present(&file)
            .await
            .with_context(|| format!("unlink export blob {blob_path}"))?
        {
            unlinked += 1;
        }
    }
    Ok(unlinked)
}

/// Unlink every file under `<data-dir>/exports/` that no `export_sessions` row
/// names. Returns how many were removed. Idempotent; a missing directory is a
/// nest that has never exported.
///
/// ⚠ **The directory is listed BEFORE the rows are read, and the order is the
/// correctness argument.** The writer commits a session's row before it creates
/// the file, so any file the listing saw already had its row. Reading the rows
/// *afterwards* therefore sees that row unless the session has since been
/// deleted — in which case the file is garbage anyway. Swap the two and a
/// session created in between shows up in the listing but not in the row set,
/// and the reclaim unlinks a live export.
///
/// A failed unlink is logged and skipped rather than returned: this is a
/// janitor, the file is still an orphan on the next pass, and one stuck file
/// must not shield the rest.
pub async fn reclaim_orphaned_export_blobs(db: &CacheDb, data_dir: &Path) -> Result<u64> {
    let dir = data_dir.join(EXPORT_BLOB_DIR);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("list {}", dir.display())),
    };
    let mut on_disk: Vec<(String, PathBuf)> = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .context("read exports dir entry")?
    {
        if !entry
            .file_type()
            .await
            .context("stat exports dir entry")?
            .is_file()
        {
            continue;
        }
        // `blob_path` is TEXT, so a name that is not UTF-8 was never minted
        // here and cannot be matched against a row — leave it alone.
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        on_disk.push((format!("{EXPORT_BLOB_DIR}/{name}"), entry.path()));
    }
    if on_disk.is_empty() {
        return Ok(0);
    }

    let named = db.all_export_blob_paths().await?;
    let mut reclaimed = 0u64;
    for (blob_path, file) in on_disk {
        if named.contains(&blob_path) {
            continue;
        }
        match unlink_if_present(&file).await {
            Ok(true) => {
                reclaimed += 1;
                tracing::info!(blob_path, "reclaimed an export blob no session names");
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(blob_path, error = %e, "orphaned export blob could not be unlinked");
            }
        }
    }
    Ok(reclaimed)
}

/// [`reclaim_orphaned_export_blobs`] for a caller holding only the configured
/// `db_path` — the boot path, the succession handler and the expiry tick.
/// Never fails its caller: each is doing something more important, and the
/// next tick re-runs the reclaim.
pub async fn reclaim_orphaned_export_blobs_best_effort(db: &CacheDb, db_path: &str, after: &str) {
    let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(db_path) else {
        return;
    };
    if let Err(e) = reclaim_orphaned_export_blobs(db, &data_dir).await {
        tracing::warn!(error = %e, after, "orphaned export-blob reclaim failed");
    }
}

/// What one pass of [`sweep_expired_export_blobs`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExpirySweep {
    /// Sessions whose blob went and whose row followed.
    pub reclaimed: u64,
    /// Sessions whose blob would not unlink: their rows stand, so the next
    /// tick finds and retries them (§ Reclaim rule 2).
    pub stuck: u64,
}

/// § Expiry's GC across every actor: unlink each expired session's blob, and
/// delete its row only once the file is gone (§ Reclaim rule 2 — file before
/// row, a failed unlink leaves the row standing).
///
/// A janitor, like [`reclaim_orphaned_export_blobs`]: one stuck file is
/// logged and counted, never allowed to shield the rest, and is retried on the
/// next tick because its row still names it. A stored path outside the minted
/// shape is refused by [`unlink_export_blob`] and its row still goes: nothing
/// under `exports/` can be leaked by that (a real blob there is either named by
/// a well-formed row or is an orphan rule 3 collects), and keeping the row
/// would retry an unlink that can never happen, forever.
///
/// The row delete is conditioned on the row still being expired and still
/// naming the unlinked file ([`CacheDb::delete_expired_export_session`]).
pub async fn sweep_expired_export_blobs(db: &CacheDb, data_dir: &Path) -> Result<ExpirySweep> {
    let mut sweep = ExpirySweep::default();
    for expired in db.expired_export_sessions().await? {
        if let Err(e) = unlink_export_blob(data_dir, &expired.blob_path).await {
            sweep.stuck += 1;
            tracing::warn!(
                blob_path = expired.blob_path,
                error = %e,
                "expired export blob could not be unlinked; its session row stays for the next tick"
            );
            continue;
        }
        if db.delete_expired_export_session(&expired).await? {
            sweep.reclaimed += 1;
        }
    }
    Ok(sweep)
}

/// The export leg of the nest's periodic expiry loop (`main.rs`): the expiry
/// sweep, then the orphan reclaim, on every tick.
///
/// The orphan reclaim rides the same tick because the classes it collects —
/// a crash between rule 2's halves, a cold resume's disowned generation, an
/// upload that raced a restart and created an old-generation file after the
/// restart's unlink — are manufacturable at will by an ordinary user, and at
/// boot-only cadence each would rest on disk until the next restart. Running
/// it after the sweep is safe in either order (a row the sweep deleted had
/// its file unlinked first; a row whose unlink failed still names its file),
/// and its own list-before-read ordering is what keeps it race-free against a
/// concurrent start or restart.
///
/// Never fails the tick: the loop carries unrelated jobs, and every pass
/// here is idempotent and re-run an hour later.
pub async fn run_export_expiry_tick(db: &CacheDb, db_path: &str) {
    // No data dir, no blob: the writer refuses to write without one either.
    let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(db_path) else {
        return;
    };
    match sweep_expired_export_blobs(db, &data_dir).await {
        Ok(ExpirySweep { reclaimed, stuck }) if reclaimed > 0 || stuck > 0 => {
            tracing::info!(reclaimed, stuck, "expired export sessions swept");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "export expiry sweep failed"),
    }
    reclaim_orphaned_export_blobs_best_effort(db, db_path, "expiry tick").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minted_path_carries_the_pinned_suffix_and_resolves() {
        // § Blob shape on disk pins `.zip.zst.sealed`: what rests on nest disk
        // is framed ciphertext, never a mountable `.zip.zst`. The mint and the
        // resolver are the two ends of one string, so they are pinned together
        // — a suffix change that broke the resolver's shape check would
        // otherwise only surface as an unlink that silently did nothing.
        let minted = export_blob_path_for("0198-abcd");
        assert_eq!(minted, "exports/0198-abcd.zip.zst.sealed");
        assert_eq!(
            resolve_export_blob(Path::new("/data"), &minted),
            Some(PathBuf::from("/data/exports/0198-abcd.zip.zst.sealed"))
        );
    }

    #[tokio::test]
    async fn appends_concatenate_and_unlink_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let blob = export_blob_path_for("s1");

        // The directory does not exist yet — a nest that has never exported.
        append_export_blob(data, &blob, 0, b"frame-0")
            .await
            .unwrap();
        append_export_blob(data, &blob, 1, b"frame-1")
            .await
            .unwrap();
        let file = resolve_export_blob(data, &blob).unwrap();
        assert_eq!(tokio::fs::read(&file).await.unwrap(), b"frame-0frame-1");

        assert!(unlink_export_blob(data, &blob).await.unwrap());
        // Second discard of the same session: the same answer, not an error.
        assert!(!unlink_export_blob(data, &blob).await.unwrap());
    }

    #[test]
    fn every_stream_generation_has_a_file_of_its_own_that_resolves() {
        // § Resume: a restart never truncates in place. Generation 0 keeps the
        // name every pre-restart session rests under; later ones are numbered,
        // and each is still the one-plain-file shape the resolver admits.
        assert_eq!(
            export_blob_path_for_generation("s1", 0),
            export_blob_path_for("s1")
        );
        let g1 = export_blob_path_for_generation("s1", 1);
        let g2 = export_blob_path_for_generation("s1", 2);
        assert_eq!(g1, "exports/s1.1.zip.zst.sealed");
        assert_ne!(g1, g2);
        assert_eq!(
            resolve_export_blob(Path::new("/data"), &g1),
            Some(PathBuf::from("/data/exports/s1.1.zip.zst.sealed"))
        );
    }

    #[tokio::test]
    async fn only_frame_zero_creates_the_blob() {
        // A later frame that finds no file is an upload into a generation a
        // restart already unlinked (or one racing a cancel). It must fail and
        // leave NOTHING behind, not seed a blob that starts mid-stream.
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let blob = export_blob_path_for("s1");
        assert!(append_export_blob(data, &blob, 3, b"stale").await.is_err());
        assert!(!resolve_export_blob(data, &blob).unwrap().exists());

        append_export_blob(data, &blob, 0, b"frame-0")
            .await
            .unwrap();
        assert!(unlink_export_blob(data, &blob).await.unwrap());
        assert!(append_export_blob(data, &blob, 1, b"stale").await.is_err());
        assert!(!resolve_export_blob(data, &blob).unwrap().exists());
    }

    #[tokio::test]
    async fn an_unminted_path_is_never_written_or_unlinked() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        assert!(
            append_export_blob(data, "../escape.sealed", 0, b"x")
                .await
                .is_err()
        );
        assert!(!unlink_export_blob(data, "../escape.sealed").await.unwrap());
    }

    #[test]
    fn only_a_single_file_directly_under_exports_resolves() {
        let data = Path::new("/data");
        assert_eq!(
            resolve_export_blob(data, "exports/s1.zip.zst"),
            Some(PathBuf::from("/data/exports/s1.zip.zst"))
        );
        for refused in [
            "",
            "exports",
            "exports/",
            "/etc/passwd",
            "/data/exports/s1.zip.zst",
            "exports/../nest.db",
            "../exports/s1.zip.zst",
            "exports/a/b.zip.zst",
            "./exports/s1.zip.zst",
            "blobs/s1.zip.zst",
            "exports/..",
        ] {
            assert_eq!(
                resolve_export_blob(data, refused),
                None,
                "{refused:?} must not resolve to a file this module would unlink"
            );
        }
    }
}
