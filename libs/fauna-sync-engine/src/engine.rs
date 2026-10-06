//! Core sync engine coordinating local <-> remote synchronization.
//!
//! The [`SyncEngine`] ties together the filesystem watcher, content-addressed
//! chunker, local sync database, and node HTTP client to implement the upload
//! pipeline: hash, chunk, deduplicate, upload, and update state.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use fauna_core::data::ContentHash;
use fauna_protocol::folders::{ConflictCandidate, ConflictReportRequest};

use fauna_mls::engine::MlsEngine;

use crate::causal::CausalStamp;
use crate::db::{SyncDb, SyncState};
use crate::nest_client::SyncClient;

#[path = "deposit_adoption.rs"]
mod deposit_adoption;
pub use deposit_adoption::DepositAdoption;

// The two WS-RPC control-plane kinds this engine speaks
// (`fauna.sync.conflicts.report`, `fauna.folders.update`) now live behind the
// `crate::nest_api::SyncControlApi` seam, alongside their transport — see that
// module's doc for why the seam exists.

/// One file to restore from a snapshot listing (the client-side full-restore walk,
/// [`SyncEngine::restore_snapshot_files_to_dir`]). Built from a `SnapshotFileEntry`
/// (`fauna_protocol::filesync`): the raw 32-byte `manifest_hash` resolved to a
/// [`ContentHash`], the snapshot-relative `path`, and the M2 content-key generation
/// (`None` on the owner `BackupKey` path — a folder backup snapshot).
#[derive(Debug, Clone)]
pub struct SnapshotFileToRestore {
    /// Folder-relative path (the write target under the output dir).
    pub relative_path: String,
    /// The file's manifest content hash — the walk's entry point.
    pub manifest_hash: ContentHash,
    /// M2 content-key generation stamped on the snapshot's chunks (`None` for an
    /// owner-only / plaintext folder — the `BackupKey` root opens it).
    pub content_key_version: Option<u64>,
}

/// The outcome of a client-side full restore
/// ([`SyncEngine::restore_snapshot_files_to_dir`], [`restore_snapshot_walk`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreSummary {
    /// Files decrypted + written into the output directory.
    pub files_restored: u64,
    /// Total plaintext bytes written across all restored files.
    pub bytes_written: u64,
    /// Entries skipped because their resolved target escaped the output directory
    /// (path-traversal guard) — never written.
    pub skipped: u64,
    /// Redacted paths of files that failed to fetch, open, or verify — a bad
    /// row costs that row, never the rest of the snapshot (the same contract
    /// [`SyncEngine::ingest_peer_share_rows`] already keeps for the peer-share
    /// plane). Everything recoverable is still written; the caller decides
    /// whether a non-empty list fails the command or is just reported.
    pub unverified: Vec<String>,
}

/// The client-side restore walk — the ONE implementation every restore host
/// drives: [`SyncEngine::restore_snapshot_files_to_dir`] (the FFI/macOS
/// `MacRestoreView` surface). The since-removed headless daemon's restore CLI
/// once ran its own download/verify/write loop beside this walk before being
/// collapsed into it.
///
/// Per file: fetch + open + reassemble + verify via the ONE shared
/// [`fauna_core::file_download::download_file_bytes_by_manifest`], then the ONE
/// shared containment guard + atomic write. A file that
/// fails ANY step — fetch, decrypt, or the whole-file integrity check — is
/// skipped and named in [`RestoreSummary::unverified`], never fatal to the rest
/// of the snapshot: the same "a bad row costs that row, never the page"
/// contract [`SyncEngine::ingest_peer_share_rows`] already keeps for the
/// peer-share plane. Everything recoverable is written; the caller decides
/// whether a non-empty `unverified` fails the command (`cmd_restore`'s
/// `restore_summary_error`) or is just reported (this crate's FFI binding,
/// `sync_engine_host.rs::restore`, does the same).
///
/// `on_file` is called once per file successfully written, with its position in
/// `files` (0-based), `files.len()`, and its RAW relative path — the CLI's
/// `[i/n] path` progress line; an engine caller passes a no-op. The path is
/// deliberately **unredacted** here, unlike every log line and error string
/// below: this callback is the user's own terminal naming their own files,
/// not a log or error string that could leave the device (`file-sync.md`
/// § Sealed names & paths).
pub async fn restore_snapshot_walk(
    fetcher: &dyn fauna_core::file_download::BlobFetcher,
    keys: &fauna_core::file_download::FileDownloadKeys,
    files: &[SnapshotFileToRestore],
    output_dir: &Path,
    mut on_file: impl FnMut(usize, usize, &str),
) -> Result<RestoreSummary> {
    std::fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "creating restore output dir {}",
            fauna_core::log_redact::log_path(&output_dir.to_string_lossy())
        )
    })?;

    let total = files.len();
    let mut summary = RestoreSummary::default();
    for (idx, file) in files.iter().enumerate() {
        // Fail-closed on an unsafe relative path (`..`, absolute, …): skip +
        // count it, never abort the whole restore, and never let the walk write
        // outside the output tree. The fetch below never even runs for it.
        if !crate::path_guard::is_safe_relative_path(&file.relative_path) {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(&file.relative_path),
                "skipping restored file with an unsafe relative path"
            );
            summary.skipped += 1;
            continue;
        }
        let file_data = match fauna_core::file_download::download_file_bytes_by_manifest(
            fetcher,
            keys,
            file.manifest_hash,
            file.content_key_version,
            &file.relative_path,
        )
        .await
        {
            Ok(data) => data,
            Err(e) => {
                // Everything recoverable is written; nothing unverified is. A
                // restore that aborted here would deny the user every *good*
                // file after this one, and a re-run would abort at the same
                // place — the exact failure mode `cmd_restore`'s own
                // `reassembly_is_intact` was added to avoid (finding: restore
                // was the only reader writing to disk with no whole-file
                // integrity anchor). Covers a fetch/network failure and a
                // whole-file hash mismatch alike, uniformly.
                let redacted = fauna_core::log_redact::log_path(&file.relative_path);
                tracing::error!(
                    path = %redacted,
                    error = %e,
                    "restoring this file failed; it was not written (every other \
                     file in the snapshot still is)"
                );
                summary.unverified.push(redacted);
                continue;
            }
        };

        // Re-check the *resolved* target stays under the output root before
        // creating any parent dirs: the walk rejects `..` in the relative
        // path, but a symlink materialized in the tree could still redirect
        // the write. ONE implementation now — the shared containment guard
        // every `join`-write door uses. Skip an escapee.
        let Some(dest) =
            fauna_core::path_guard::resolved_target_within_root(output_dir, &file.relative_path)
        else {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(&file.relative_path),
                "skipping restored file with a resolved path outside the output directory"
            );
            summary.skipped += 1;
            continue;
        };
        // The terminal write is `atomic_write_file` — temp-in-same-dir +
        // fsync + rename — for two reasons, both load-bearing:
        //
        // 1. Containment of the LAST hop. `resolved_target_within_root`
        //    deliberately leaves the target's final component
        //    uncanonicalized (it legitimately may not exist yet), so that
        //    hop is contained by the write *replacing* a symlink instead of
        //    following it. A direct `std::fs::write` follows a **dangling**
        //    link — the one shape the guard cannot see, since `canonicalize`
        //    fails on it and the ancestor walk falls back to the output root
        //    — landing the bytes at an attacker-chosen absolute path
        //    (the path is nest-supplied).
        // 2. Crash safety: a mid-write crash leaves the previous file
        //    intact rather than a truncated restore.
        //
        // It creates the parent dirs itself, and only after the containment
        // guard above passed — so nothing is created beyond the root.
        crate::atomic_write::atomic_write_file(&dest, &file_data)
            .await
            .with_context(|| {
                format!(
                    "writing restored file {}",
                    fauna_core::log_redact::log_path(&dest.to_string_lossy())
                )
            })?;
        summary.files_restored += 1;
        summary.bytes_written += file_data.len() as u64;
        on_file(idx, total, &file.relative_path);
    }
    Ok(summary)
}

/// A hydrated ([`SyncState::Synced`]) row whose nest head has moved — its on-disk
/// bytes are a stale copy of a file the nest has since changed.
///
/// [`SyncEngine::record_placeholders_from_changes`] never rewrites such a row: the
/// bytes are real files on disk, and freeing them is the platform placeholder
/// surface's job (Windows cfapi). The fold therefore *reports* the row, carrying
/// the nest's head, and a cfapi-aware caller performs the invalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleHydratedRow {
    /// Folder-relative path, forward-slash normalized (`fauna_core::sync`).
    pub relative_path: String,
    /// The nest's current manifest for this path — the new hydration anchor.
    pub manifest_hash: ContentHash,
    /// The nest's current size for this path.
    pub size_bytes: i64,
    /// M2 content-key generation the head's chunks were sealed under.
    pub content_key_version: Option<u64>,
    /// The head change's `created_at`, stamped as the row's `remote_mtime`.
    pub remote_mtime: i64,
    /// The existing row's `version_num`, carried so the re-point preserves it.
    pub version_num: i64,
}

/// What one [`SyncEngine::record_placeholders_from_changes`] pass did, and what it
/// deliberately left for its caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaceholderFold {
    /// Placeholder rows written — new, or re-pointed at a moved head.
    pub recorded: usize,
    /// The rows among [`recorded`](Self::recorded) that are brand NEW — a path that
    /// had no row before this fold — sorted by path. A re-point is not here: its
    /// placeholder is already wherever its directory was listed. An on-demand host
    /// whose platform lists a directory only once (Windows cfapi marks a populated
    /// directory `DISABLE_ON_DEMAND_POPULATION`) materializes these eagerly, or a
    /// remote create into an already-browsed directory never appears
    /// (`docs/goal/behavior/delete-propagation.md` § *The floor on an on-demand
    /// root*, decision (f)).
    pub created: Vec<crate::enumerate::PlaceholderRow>,
    /// Hydrated rows the fold refused to touch, sorted by path. Empty on every
    /// platform that keeps its folders always-resident; non-empty only on an
    /// on-demand host whose local copy the nest has superseded.
    pub stale_hydrated: Vec<StaleHydratedRow>,
    /// The share plane's provisional rows this fold retired — what a host
    /// that owns its tree settles its peer-landed bodies by
    /// (`provider_face::owned_tree::OwnedTree::settle_peer_bodies`).
    pub overlay: OverlayReconcile,
}

/// What one retire-on-arrival pass over the share overlay did
/// (`SyncEngine::retire_share_overlay`): every overlaid path the nest spoke
/// for is retired, and the two outcomes that touch a landed body are named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayReconcile {
    /// Paths whose peer-landed bytes the nest's row CONFIRMED — the withheld
    /// dehydration proof is stamped, so the body may be freed like any
    /// recorded one. Sorted.
    pub confirmed: Vec<String>,
    /// Peer-landed bodies the nest's row SUPERSEDED (another head, or a
    /// delete). Sorted by path.
    pub superseded: Vec<SupersededBody>,
}

impl OverlayReconcile {
    pub fn is_empty(&self) -> bool {
        self.confirmed.is_empty() && self.superseded.is_empty()
    }
}

/// One peer-landed body the nest's row for its path superseded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersededBody {
    pub path: String,
    /// Hex content hash of the bytes the share plane landed — a body that no
    /// longer hashes to it was written over since and is not the peer's.
    pub content_hash: String,
}

/// How [`SyncEngine::ingest_peer_share_rows_on_demand`] lands bodies: into
/// `tree`'s kept root, above the storage floor. `free_space` answers the
/// device's free space where they land — a closure so the floor is measured
/// at each landing, and a test can script it.
#[cfg(feature = "p2p-share")]
pub struct OnDemandLanding<'a> {
    pub tree: &'a crate::provider_face::owned_tree::OwnedTree,
    pub free_space: &'a dyn Fn() -> Option<u64>,
}

/// One diverging version the engine saw at conflict detection — the inputs
/// needed to build a [`ConflictCandidate`] for the nest. The *local* side is
/// chunked + (idempotently) uploaded so its manifest is retrievable when the
/// user picks it as winner; the *incoming* side is already on the nest (the
/// change that conflicted carried it).
struct IncomingVersion {
    /// Manifest hash of the incoming change (the version that conflicted).
    manifest_hash: ContentHash,
    /// Byte length of the incoming file data.
    size_bytes: i64,
    /// Hex-encoded device id that produced the incoming change, if known.
    device_id: Option<String>,
    /// When the incoming change was recorded (unix millis, the
    /// `sync_changes.created_at` stamp) — the latest-writer-wins clock.
    created_at_ms: i64,
    /// M2 content-key generation the incoming version's chunks were sealed
    /// under (bound sets) — echoed onto its conflict candidate.
    content_key_version: Option<u64>,
}

/// The resolved half of an auto-resolve report (`report_conflict_ws`).
struct ReportResolution {
    /// `"merged"` | `"latest_wins"`.
    kind: &'static str,
    winning_manifest_hex: String,
    /// Required for a merged (non-candidate) winner; `None` for candidate
    /// winners (the nest takes the candidate's recorded size).
    winning_size_bytes: Option<i64>,
    winning_content_key_version: Option<u64>,
    /// Causal pair for the propagated rows (`conflicts.md` clause 5): the
    /// winner head row's watermark claim (the incoming row's seq for an
    /// in-order pass) and the loser retention row's (the ledger ancestor seq).
    winning_derived_through: Option<i64>,
    losing_derived_through: Option<i64>,
    /// The winner carries this device's UNPUBLISHED pre-merge novelty and
    /// must mint EDIT-class (the same-anchor ruling, 2026-08-05 —
    /// `ConflictReportRequest::winning_carries_novelty`).
    winning_carries_novelty: Option<bool>,
}

/// What `auto_resolve_conflict` decided the caller must do to the LOCAL file.
/// The nest-side retention + propagation already happened (or, for
/// `Unresolved`, deliberately did not).
enum ResolvedApply {
    /// The incoming version won — write it (the normal apply).
    ApplyIncoming,
    /// The local version won — do NOT touch the file; the local entry
    /// re-points at the uploaded local manifest (now the propagated head).
    KeepLocal {
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
    },
    /// A clean three-way merge won — write `merged` and point the entry at
    /// its uploaded manifest (the propagated head).
    WriteMerged {
        merged: Vec<u8>,
        /// The uploaded manifest of `merged` — what the store-key index
        /// records once the merged bytes are written.
        manifest: Box<fauna_core::chunk::ChunkManifest>,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
    },
    /// Fail-closed fallback: keep the local file untouched; an unresolved
    /// conflict row + unresolved report own the divergence (the chooser flow).
    Unresolved,
}

/// What [`SyncEngine::download_and_write_file`] did with the row — the leg-4
/// ruling (2026-08-05, `conflicts.md` § Concurrent resolution & ancestor
/// freshness) splits the covering-adopt defer out of the ordinary outcomes:
/// it is a transient-class CAP (the seal-cap / unresolved-cap family), never
/// a consumed skip. The pre-ruling arm returned plain `Ok(())` with the pull
/// anchor advancing regardless (`set_anchor(max_seq)` runs whatever the arms
/// did), which permanently discarded the deferred row — on the engine host
/// nothing re-lists a consumed row, and the dropped rows were exactly the
/// covering winners carrying the fleet's convergence (the leg-4 repair rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DownloadOutcome {
    /// The row was consumed: applied, merged, or skip-accounted.
    Applied,
    /// The verbatim adopt was deferred because this device's own pending
    /// rows are unlisted (the unknown-seq report window, or the ack→echo
    /// window where the licence rode the recorded witness alone). The caller
    /// must hold the anchor BELOW this row's seq: the next pull re-lists it
    /// together with the pending rows (log contiguity), the pre-pass counts
    /// them, and the retried judgement is exact — a truly covering row still
    /// adopts, a non-covering one merges.
    DeferredCap,
}

/// A thumbnail download-backfill target (Seam B): the batch-latest live change
/// for a path that still lacks a thumbnail. Produced by
/// [`SyncEngine::thumbnail_backfill_targets`], consumed by the pull loop to
/// re-record the file with its now-generated thumbnail carrying the same
/// `size_bytes` (quota delta 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackfillTarget {
    /// Seq of the batch-latest change for the path (so the pull loop backfills
    /// only when it downloads *that* change, once per path).
    pub(crate) seq: i64,
    /// The change's `size_bytes`, re-recorded verbatim so the nest's quota
    /// delta stays 0 (only `thumbnail_hash` changes).
    pub(crate) size_bytes: i64,
}

/// Current unix time in seconds (native host — `SystemTime` is available).
fn now_unix_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

/// Build the two-element candidate set for a conflict from the engine's
/// diverging versions: `[local, incoming]`. Pure (unit-tested independently);
/// the impure upload that makes the local manifest retrievable lives in
/// [`SyncEngine::report_conflict_with_candidates`]. `now_secs` is stamped as
/// each candidate's recording time. See `file-sync.md` § Conflicts.
/// Normalize the nest's `sync_changes.created_at` — stamped in **milliseconds**
/// (`now_epoch_millis()`, `bins/fauna-nest/src/db/sync_storage.rs`) — into the
/// **Unix seconds** every mtime in this engine is denominated in.
///
/// The wire stays millis (it is also the conflict "latest-writer-wins" clock, where
/// sub-second resolution matters — see `IncomingVersion::created_at_ms`); it is only
/// as an **mtime** that the value must be seconds. That is the one conversion, and it
/// belongs here, at the single point a change becomes a row's `remote_mtime`:
/// `remote_mtime` sits beside `local_mtime` (`as_secs()`), and flows out through
/// `PlaceholderRow.mtime` ("Unix seconds") into cfapi's `unix_to_filetime`, which
/// **overflows `i64` on a millisecond value** — panicking in debug, and silently
/// wrapping to a garbage FILETIME in release. Found live on Windows, 2026-07-13.
fn created_at_ms_to_unix_secs(created_at_ms: i64) -> i64 {
    created_at_ms / 1_000
}

fn build_conflict_candidates(
    local_manifest_hex: String,
    local_size: i64,
    local_content_key_version: Option<u64>,
    local_device_hex: String,
    incoming: &IncomingVersion,
    now_secs: i64,
) -> Vec<ConflictCandidate> {
    vec![
        ConflictCandidate {
            manifest_hash: local_manifest_hex,
            device_id: local_device_hex,
            size_bytes: local_size,
            created_at: now_secs,
            content_key_version: local_content_key_version,
            ..Default::default()
        },
        ConflictCandidate {
            manifest_hash: hex::encode(incoming.manifest_hash.digest()),
            device_id: incoming.device_id.clone().unwrap_or_default(),
            size_bytes: incoming.size_bytes,
            created_at: now_secs,
            content_key_version: incoming.content_key_version,
            ..Default::default()
        },
    ]
}

/// Receipt from an [`SyncEngine::upload_file_inner`] upload, consumed by the
/// re-seal migration's verify + supersede step. When the record FAILED the
/// local entry deliberately keeps the merge-base manifest (the nest never saw
/// the new head), so this receipt is the only way a caller learns the new
/// manifest hash; on a successful record `commit_recorded_head` stamps the row
/// with it. (`pub(crate)` only because the in-crate test modules touch the
/// inner fns' signatures; not part of the engine's public surface.)
#[derive(Debug)]
pub(crate) struct ResealUpload {
    /// The uploaded manifest's hash — the path's new head on the nest once
    /// `recorded` is true.
    manifest_hash: ContentHash,
    /// Whether the `fauna.sync.changes.record` control-plane row reached the
    /// nest. When false the nest head is still the OLD manifest, so a supersede
    /// must not be attempted (it would head-mismatch — fail-safe, but noisy).
    recorded: bool,
}

/// What one path's re-seal actually achieved — the three outcomes a corpus pass
/// must tell apart, because two of them look identical from the byte plane
/// alone and mean opposite things for the pass's folder-level marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResealDisposition {
    /// Re-sealed **and** the change record landed: the nest's head is the new
    /// manifest, so this path is converged and may be marked done.
    Recorded,
    /// The bytes moved but the change record was refused. The nest head still
    /// names the OLD manifest, so the path is *not* converged — the pass keeps
    /// walking but must report the shortfall rather than mark or stamp
    /// (the discipline).
    Unrecorded,
    /// Nothing to re-seal anywhere: no bytes on this disk **and** no recorded
    /// head to fetch from the nest. Not a shortfall — a pass cannot converge
    /// content that exists in neither place, and counting it as one would wedge
    /// the folder marker forever. Distinct from [`Self::Unrecorded`] precisely
    /// because the remedy differs: retry helps that one, and nothing helps this.
    Nothing,
}

/// Whether a path's bytes are on this disk — the answer
/// [`SyncEngine::path_is_materialized`] gives, with the third state that makes
/// it honest.
///
/// A two-valued answer forced every `stat` failure into "not here", and at the
/// choke point "not here" can mean *converged*. So the fault case
/// gets its own arm: **unknown is not absent**, and the one thing it must never
/// do is resolve to "the bytes exist nowhere".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Materialization {
    /// `stat` succeeded and the bytes are readable here — the force-upload leg
    /// can read them.
    Present,
    /// The bytes are definitively **not** on this disk, in one of the two ways
    /// that is a normal steady state rather than a fault: the file does not
    /// exist, or it is a cloud-only placeholder. Both are safe to treat as
    /// "source from the nest, or there is nothing to converge".
    Absent,
    /// `stat` **failed** for a reason other than "it is not there". Whether the
    /// bytes are on this disk is unknown, so no caller may conclude anything
    /// about convergence from it. Carries the OS error's text for the refusal
    /// message — the operator needs to see EACCES vs ESTALE to fix it.
    Unknown(String),
}

/// Where [`SyncEngine::reseal_path_under_current`] gets the plaintext it
/// re-seals. The trio *after* the plaintext — upload → verify → supersede — is
/// identical either way, which is why this is a source selector rather than a
/// second re-seal path: one epilogue, so a later plane (the media-thumbnail
/// leg) inherits the verify and the reclaim instead of re-deriving them.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ResealSource {
    /// The bytes on this disk, force-uploaded through
    /// [`SyncEngine::upload_file_inner`]. Every pre-succession re-seal caller
    /// uses this: the pre-bind migration, the owner-only plaintext migration,
    /// and the drain's requeue-under-current all run on a device that holds the
    /// file.
    LocalFile,
    /// The nest's own copy, fetched under whichever root still opens it —
    /// including a **predecessor's**
    /// ([`fauna_core::file_download::FileDownloadKeys::predecessor_backup_keys`])
    /// — and re-uploaded under the current one.
    ///
    /// The post-succession case the aftermath actually exists for: a successor
    /// restoring onto a fresh device holds *nothing* in a watch dir, so this
    /// read is the only plaintext source there is. Carries the head it must
    /// fetch rather than re-resolving it, because the caller
    /// ([`SyncEngine::reseal_predecessor_sealed`]) has already read the row.
    NestBytes {
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
    },
}

/// What [`SyncEngine::reseal_predecessor_sealed`] concluded about one entry.
/// Three outcomes, not a `bool`, because "already under the current root" and
/// "moved it there" are both *done* while looking nothing alike in the counts,
/// and conflating either with "still owed" is what would let a completion check
/// license `sync-agent.md` bound (3) too early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResealOutcome {
    /// Nothing to move: it already opens under this identity's own root. Marked.
    AlreadyCurrent,
    /// Moved to the current root and the new head recorded. Marked.
    Resealed,
    /// Still sealed under a retired root as far as the nest is concerned.
    /// Left **unmarked** so the completion observable keeps reporting it.
    StillOwed,
}

/// What a completed [`SyncEngine::upload_file`] did — the caller-visible slice
/// of the upload receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UploadOutcome {
    /// The change record reached the nest (and `commit_recorded_head` stamped
    /// the row): local content, nest head, and DB row all agree, so the file is
    /// provably synced end-to-end. False for a failed record, or for a skip
    /// whose record provably never landed — a caller flipping platform sync
    /// state (the windows on-demand root's in-sync mark) must key on this,
    /// never on the upload alone: bytes on the nest without a record are
    /// unreachable for hydration. A skip of an already-synced file whose head
    /// IS recorded (`recorded_content_hash == local_hash`) reports `true`: the
    /// retry of a write whose ack was lost must converge to an ack, not spin
    /// forever.
    pub recorded: bool,
    /// The row moved to a head the caller's on-disk bytes are NOT — true
    /// exactly when a conflicted ingest re-pointed the row un-hydrated (merge /
    /// incoming winner, [`SyncEngine::ingest_conflicting`]). The File Provider
    /// extension surfaces this as `modifyItem`'s `shouldFetchContent`, so the
    /// OS re-fetches the winner instead of associating its loser bytes with the
    /// winning version. Always false on the plain (non-conflict) paths.
    pub content_changed: bool,
}

/// What [`SyncEngine::reseal_inherited_version`] re-sealed a version into —
/// the head a restore records in the historical manifest's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResealedVersion {
    /// The new manifest, sealed under the current owner root.
    pub manifest_hash: ContentHash,
    pub size_bytes: i64,
}

/// Result of the shared chunked-upload pipeline used by both
/// [`SyncEngine::upload_file`] and [`SyncEngine::upload_bytes`].
struct UploadedManifest {
    manifest: fauna_core::chunk::ChunkManifest,
    manifest_hash: ContentHash,
    /// Number of chunks actually uploaded (the rest were destination-side
    /// dedup hits returned by `check_chunks`).
    uploaded_count: usize,
    /// The M2 content-key generation the chunks were sealed under (to stamp into
    /// the change record). `None` for owner-only / plaintext / `backup_key`
    /// uploads, which carry no generation. See [`SyncEngine::content_seal_root`].
    content_key_version: Option<u64>,
}

// The streamed file hash lives in `fauna_core::chunker_stream`
// (`content_hash_streaming`) — this module already called it directly at four
// sites while a local `streaming_file_hash` served three more, same digest but
// its own buffer size and error shape, with nothing marking which was
// canonical. The local copy is gone; all seven sites call the shared one.

/// The mass-delete floor (`file-sync.md` § Files Appear Automatically, ratified
/// 2026-08-02): when EVERY known-synced path is missing at once, and there are
/// at least [`MASS_DELETE_FLOOR_MIN`] of them, that reads as infrastructure
/// failure (an unmounted/renamed volume, a bound dir removed under the process)
/// rather than user intent — recording it as deletes would erase the nest's copy
/// of the whole set (no-user-data-loss, `principles.md`). `MIN=2` is deliberate:
/// a single file's absence is far likelier a genuine delete, and its loss
/// magnitude is one version-recoverable file. The canonical predicate behind
/// [`SyncEngine::reconcile`]'s watcher-driven path — one threshold, so no
/// second delete rail can silently desync onto a different floor.
pub const MASS_DELETE_FLOOR_MIN: usize = 2;

/// How long the pre-seal row read ([`SyncEngine::refresh_seal_floor`]) waits for
/// a still-connecting control plane before holding the batch as unreadable. A
/// freshly built engine's connection is still dialling when its start-up
/// catch-up pass seals; without the wait that pass would be held for a whole
/// rescan cadence. Bounded, so a nest that is really down still holds the write.
const SEAL_FLOOR_CONNECT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// `true` when `missing` covers every one of `total` known-synced paths and
/// `total` clears [`MASS_DELETE_FLOOR_MIN`] — see that constant's doc for the
/// policy this implements.
///
/// ⚠ `total` is the count of rows the pass could actually **observe**, not
/// every `Synced` row — see [`MissingFromScan::observable`]. A row hidden under
/// an unreadable directory belongs in neither side of this comparison: counted
/// as present it would silently disarm the floor, and counted as missing it
/// would arm it on a fault the floor is not about.
pub fn is_mass_delete_floor(missing: usize, total: usize) -> bool {
    missing == total && total >= MASS_DELETE_FLOOR_MIN
}

/// What one scan's delete-detection pass may and may not conclude about the
/// `Synced` rows it did not find on disk — the output of
/// [`SyncEngine::missing_from_scan`].
///
/// The split is the whole safety content. *"A synced row is not in
/// the scan"* has two utterly different causes, and before this type they were
/// the same `Vec`: the file is gone (propagate the delete, that is sync working),
/// or the scan could not read where it lives (propagate nothing — a `chmod 000`,
/// a failing disk, an `ESTALE` on a network mount, and the user still has every
/// byte). `docs/goal/behavior/succession-aftermath.md`'s rule, *unreadable is
/// not absent*, applied to the delete rail.
struct MissingFromScan {
    /// Rows the scan **proves** are gone: their directory was enumerated and
    /// they were not in it. The floor's numerator, and the only rows any delete
    /// is ever recorded for.
    missing: Vec<crate::db::SyncEntry>,
    /// Rows withheld because they sit under a directory the scan could not
    /// enumerate. Not deleted, not held by the floor, not counted by it —
    /// reported outward so the fault is visible, and re-derived from scratch
    /// next pass (nothing is stored, exactly like the hold itself).
    withheld_unreadable: Vec<String>,
}

impl MissingFromScan {
    /// How many rows with evidence ([`SyncEngine::rows_with_evidence`]) this pass
    /// could actually observe — the floor's denominator. `total_synced` is the
    /// full row count the caller fetched.
    fn observable(&self, total_synced: usize) -> usize {
        total_synced.saturating_sub(self.withheld_unreadable.len())
    }
}

/// Build the format registry with every compiled-in semantic merge adapter
/// (feature-gated). Shared by every engine host so every deployment registers the *same* adapters
/// (the 3-way text merge) — callers don't repeat the `#[cfg]` dance.
pub fn default_format_registry() -> fauna_core::format::FormatRegistry {
    // The mutator below is feature-gated, so a build without `format_text`
    // never mutates this binding and `-D unused-mut` fires — invisible under
    // default features, which turn it on. The allow is scoped to exactly that
    // configuration rather than applied unconditionally, so a genuinely
    // redundant `mut` still goes red whenever an adapter IS compiled in.
    #[cfg_attr(not(feature = "format_text"), allow(unused_mut))]
    let mut registry = fauna_core::format::FormatRegistry::new();
    #[cfg(feature = "format_text")]
    registry.register(Box::new(fauna_core::format_text::TextAdapter));
    registry
}

/// The engine's binding of the shared walk's blob-fetch seam
/// ([`fauna_core::file_download::BlobFetcher`]) to its own transport: manifests
/// over the [`SyncClient`], chunks over the pooled [`TransferPool`](crate::transfer::TransferPool)
/// so a shared-Rust download keeps native concurrency + the bandwidth limiter.
///
/// Borrows rather than owns — it is built per call by
/// [`SyncEngine::blob_fetcher`] and lives only for that await.
struct EngineBlobFetcher<'a> {
    client: &'a SyncClient,
    transfer_pool: &'a crate::transfer::TransferPool,
}

#[async_trait::async_trait]
impl fauna_core::file_download::BlobFetcher for EngineBlobFetcher<'_> {
    async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        self.client.download_manifest(hash).await
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> Result<Vec<Vec<u8>>> {
        self.transfer_pool
            .download_chunks(self.client, store_keys, relative_path)
            .await
    }
}

/// The engine's binding of the shared re-seal's write seam
/// ([`fauna_core::nest_reseal::ChunkStoreSink`]): a dedup check, then the
/// pooled upload of what is missing — the [`EngineBlobFetcher`] twin, built per
/// call in [`SyncEngine::reseal_nest_copy_windowed`].
struct EngineChunkSink<'a> {
    client: &'a SyncClient,
    transfer_pool: &'a crate::transfer::TransferPool,
    /// Phase 5: a metadata-only folder's bytes never rest on the nest, so its
    /// chunk puts are skipped (the manifest still posts).
    metadata_only: bool,
}

#[async_trait::async_trait]
impl fauna_core::nest_reseal::ChunkStoreSink for EngineChunkSink<'_> {
    async fn put_chunks(
        &self,
        bodies: Vec<(ContentHash, Vec<u8>)>,
        relative_path: &str,
    ) -> Result<usize> {
        if self.metadata_only {
            return Ok(0);
        }
        let window_keys: Vec<ContentHash> = bodies.iter().map(|(k, _)| *k).collect();
        let missing = self
            .client
            .check_chunks(&window_keys)
            .await
            .context("re-seal: checking chunks with destination")?;
        let missing_bodies: Vec<(ContentHash, Vec<u8>)> = bodies
            .into_iter()
            .filter(|(k, _)| missing.contains(k))
            .collect();
        if missing_bodies.is_empty() {
            return Ok(0);
        }
        let results = self
            .transfer_pool
            .upload_chunks(self.client, &missing_bodies, relative_path)
            .await;
        let failed = results.iter().filter(|r| !r.success).count();
        if failed > 0 {
            anyhow::bail!(
                "re-seal: destination rejected {failed}/{} chunk upload(s) — refusing to post a \
                 manifest over missing chunks",
                results.len()
            );
        }
        Ok(missing_bodies.len())
    }

    async fn put_manifest(
        &self,
        _manifest_hash: ContentHash,
        manifest_bytes: Vec<u8>,
    ) -> Result<()> {
        self.client.upload_manifest(&manifest_bytes).await
    }
}

/// Core sync engine coordinating local <-> remote synchronization.
/// What one [`SyncEngine::apply_remote_changes`] batch actually did.
///
/// `deferred` is true when a transient-class cap left changes for a later pull
/// (a sealed path that did not open under this custody, or a peer delete
/// arriving while the sync mode is unresolved). The pass is then **unfinished**:
/// the anchor stayed below the cap, and the caller must not stamp the device
/// clean/caught-up on it (`SyncDb::mark_clean_pass_if_drained` — a status claim
/// the device has not reached).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AppliedBatch {
    /// Changes applied to disk/db (the old return value).
    pub applied: usize,
    /// A cap deferred at least one change to a later pull.
    pub deferred: bool,
}

/// The set's nest heads the reader admitted, by path — the pre-bind pass's
/// gate ([`SyncEngine::prebind_owed_rows`]).
#[derive(Debug, Clone, Default)]
pub(crate) struct PrebindHeads {
    heads: HashMap<String, ContentHash>,
    /// Unit tests only — see [`SyncEngine::prebind_heads`].
    admit_all: bool,
}

impl PrebindHeads {
    /// Whether `manifest` is `path`'s admitted head.
    fn admits(&self, path: &str, manifest: &ContentHash) -> bool {
        self.admit_all || self.heads.get(path) == Some(manifest)
    }
}

/// One `changes.list` read after the reader's verify step
/// ([`SyncEngine::fetch_change_batch`]).
#[derive(Debug)]
pub(crate) struct FetchedBatch {
    /// The rows the reader admitted, below the first held one.
    pub changes: Vec<fauna_protocol::sync::SyncChange>,
    /// How far the cursor may advance: past every refused row (absent), but
    /// below the first held one. `None` = nothing served (or held at the
    /// first row).
    pub advance_to: Option<i64>,
    /// A row could not be judged yet — the pass did not drain the feed.
    pub held: bool,
}

/// What one head re-judge pass did ([`SyncEngine::rejudge_passed_heads`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadRejudge {
    /// Every head the pass owed was folded, was a duplicate, or can never
    /// apply here. `false` leaves the pass owed: a row could not be judged
    /// yet, a label had no root yet, or a fold deferred.
    pub complete: bool,
    /// Heads folded onto this device (applied to disk/db).
    pub folded: usize,
}

/// A host's answer to [`SyncEngine::set_custody_refetch_request`].
pub type CustodyRefetchRequest = Arc<dyn Fn() + Send + Sync>;

pub struct SyncEngine {
    watch_dir: PathBuf,
    /// This engine's merge-base root, already resolved to its per-`(device,
    /// set)` subdirectory ([`SyncEngine::resolve_state_dir`]). Resolved at
    /// construction because the adoption walks a directory and the accessor
    /// is read on hot paths.
    base_dir: PathBuf,
    /// This engine's causal-state root, resolved the same way and for the same
    /// reason — the two are one hazard.
    causal_dir: PathBuf,
    db: SyncDb,
    client: SyncClient,
    folder: Option<String>,
    #[allow(dead_code)] // retained for future authorize-device flow
    device_id: [u8; 32],
    /// This engine's sync-mode **resolution** for the set — whether an
    /// authoritative answer exists at all, and what it is
    /// (`config::ModeResolution`; `file-sync.md` § 4). Read by
    /// [`Self::apply_remote_changes`]'s delete arm so a `backup`-mode host never
    /// erases a file because a peer did, and so an **unresolved** seat declines
    /// deletes *and holds its anchor* rather than guessing the delete-applying
    /// default (direction ratified 2026-08-02; `principles.md` § No user-data
    /// loss ranks the two error directions by reversibility).
    ///
    /// A **constructor argument** — not merely a setter like the row-derived
    /// knobs that used to sit beside it. Those tuned *when* work
    /// happens; this one decides whether user data is destroyed, so no
    /// construction site may leave it unstated: the nest-side guard's whole
    /// failure was being the only rail that had one, and a defaulted field would
    /// have reproduced exactly that shape one layer down.
    ///
    /// **Live, not once-per-process** ([`Self::refresh_sync_mode`]): the
    /// resident loop re-resolves it from the authoritative nest rows at entry
    /// and on every rescan tick, so a role the user changes in the wizard
    /// reaches the running engine within a cadence instead of waiting for a
    /// process restart — the leg-1 fix. The constructor value is only
    /// the host's honest starting position until that first resolution.
    mode: std::sync::RwLock<crate::config::ModeResolution>,
    /// Shared MLS engine (`Arc` so an in-process client — e.g. the Linux
    /// monolith — can drive sync, conversations, and DM key-packages off the
    /// *one* engine instance over the *one* `mls.db`). Accessed only through
    /// `&self`, so sharing is sound.
    ///
    /// `None` for a bearer-only on-demand hydration host (the Windows cfapi
    /// helper): hydration (`download_file_bytes*`) never touches MLS, and the
    /// helper holds no identity keypair to build an `MlsEngine` from. The only
    /// MLS-dependent surface — `device_sync_channel_id()` — returns `None` in
    /// that mode (the helper never device-syncs).
    mls: Option<Arc<MlsEngine>>,
    /// Paths recently WRITTEN by a download — suppress the `Created`/`Modified`
    /// echo that would re-upload what we just fetched.
    recent_writes: Mutex<HashSet<String>>,
    /// Paths recently REMOVED by an applied remote delete — suppress the
    /// `Removed` echo that would re-record, on the nest, a delete that *came
    /// from* the nest.
    ///
    /// Deliberately a **second** set rather than a reuse of `recent_writes`.
    /// Both suppressions are consume-on-check, and a path that is downloaded
    /// and then remote-deleted in the same pull (the ordinary case when a fresh
    /// folder binds to a set whose history contains deletes) queues a `Created`
    /// *and* a `Removed` against a single token: the `Created` consumed it and
    /// the `Removed` came up empty, so the engine re-recorded the delete it had
    /// just applied. Pinned by
    /// `always_resident::dir_event_tests::the_download_echo_does_not_disarm_the_removal_echo`.
    recent_removals: Mutex<HashSet<String>>,
    /// **This root's placeholders are never on its disk** — the linux FUSE
    /// root's posture (`on-demand-files.md` § Linux FUSE binding, the dehydrate
    /// rule, obligations 2 and 4), set once at boot by the host that serves the
    /// root ([`Self::set_placeholders_off_disk`]) and never cleared. A cfapi or
    /// File Provider placeholder is a file on the disk, so its absence can be a
    /// delete; a FUSE placeholder is only a row, so under this posture no
    /// `Placeholder` row is evidence ([`Self::rows_with_evidence`]), a watcher
    /// `Remove` of one is the provider's own unlink, the engine never writes a
    /// seen mark on one, and a `Placeholder` row found over real bytes is a crash
    /// between the dehydrate's two steps, repaired toward the bytes. An atomic
    /// only because the engine's other flags are shared-reference reads; one
    /// thread ever writes it.
    placeholders_off_disk: std::sync::atomic::AtomicBool,
    /// Epoch secret for per-chunk encryption. None = plaintext (an unkeyed or unbound engine).
    epoch_secret: Option<[u8; 32]>,
    /// The owner-audience key for per-chunk owner-only encryption. When set,
    /// takes priority over `epoch_secret`.
    ///
    /// Two variants (`fauna_core::crypto::OwnerSealKey`): every engine on a
    /// user's device is on the `Client` variant (the seed-derived `BackupKey`);
    /// the source nest's in-process segment-backup coordinator is on
    /// `SourceNest` (the owner-granted `NestBackupKey`). The engine only ever
    /// needs the convergent chunk root, which both variants supply — see
    /// [`Self::effective_backup_key`] for the bound-set precedence rule.
    backup_key: Option<fauna_core::crypto::OwnerSealKey>,
    /// Retired owner keys this account **succeeded from** — read candidates
    /// beside [`Self::backup_key`], never a seal root
    /// ([`fauna_core::file_download::FileDownloadKeys::predecessor_backup_keys`]
    /// carries the full contract).
    ///
    /// Set through [`Self::set_predecessor_backup_keys`] rather than [`Self::new`]
    /// deliberately: `new` already takes `backup_key` **positionally** among 18
    /// arguments, so a 19th would break every construction site in the workspace
    /// for a value that is empty on all but the handful of engines belonging to a
    /// successor.
    predecessor_backup_keys: Vec<fauna_core::file_download::PredecessorSealKey>,
    /// Raw MLS group id binding this engine's folder to a cross-user shared
    /// group (shared folders, Slice 1) — the **bound marker**. `Some` ⇒ the
    /// set is shared: chunks seal under the per-set **M2 content key**
    /// ([`Self::content_keys`]), version-stamped per snapshot, instead of the
    /// owner-only `backup_key`/`epoch_secret`. `None` ⇒ owner-only (the
    /// production default until a set is shared) — stays on `backup_key`. Stored
    /// as the raw group id (the nest's `folders.mls_group_id` BLOB shape);
    /// `ChannelId::from_group_id` derives the group-lookup key (used by the
    /// rotate-on-removal orchestration's envelope publish/eviction, Slice-3
    /// piece 5). **FS-BIND-5 keys off this marker, not [`Self::content_keys`]:** a
    /// bound set whose content keys failed to load (`mls_group_id = Some`,
    /// `content_keys = None`) **fails closed** rather than degrade to the
    /// plaintext branch — so "is this shared?" must be answerable independently
    /// of whether the key material is in hand.
    mls_group_id: Option<Vec<u8>>,
    /// The per-set **M2 content-key generation history** for a bound (cross-user
    /// shared) set — the `chunk_crypto` root source under the M2 mechanism
    /// (`mls-group-key-material.md` § M2 content-key mechanism). The
    /// rotate-on-removal orchestration (Slice-3 piece 5) loads it from the
    /// owner's `fauna.state.folder-keys` custody, or — for a member — by opening the
    /// group content-key envelope; it is **not** derived from the epoch secret
    /// (each generation is an independent CSPRNG key). `Some` ⇒
    /// uploads seal under [`FolderContentKeys::current_key`] and stamp
    /// [`FolderContentKeys::current_version`]; reads select
    /// [`FolderContentKeys::key_for`] and **fail closed** on a missing
    /// generation. `None` for an owner-only set (the production default) — and,
    /// for a *bound* set, the fail-closed-trigger above (never a plaintext
    /// fall-through). The replacement, under M2, for the Slice-2 current-epoch
    /// `export_chunk_key`-as-chunk-root (which rotated on every commit and so
    /// could not open a chunk sealed before a member joined).
    content_keys: Option<fauna_core::folder_keys::FolderContentKeys>,
    /// Retired M2 generations of a set this engine no longer runs bound/served
    /// to — read candidates beside [`Self::content_keys`], never a seal root
    /// ([`fauna_core::file_download::FileDownloadKeys::retired_content_keys`]
    /// carries the full contract; the same shape as
    /// [`Self::predecessor_backup_keys`] on the owner-key axis).
    ///
    /// The WebDAV serve-toggle case this exists for (`webdav-server.md` §
    /// Key model, Revocation): `serve_disable` rotates a group-less set's
    /// content key rather than forgetting it, so the owner's own custody
    /// still holds every generation a served window sealed chunks under —
    /// even once this set's binding degrades to owner-only
    /// (`mls_group_id = None`, `content_keys = None`). Set through
    /// [`Self::set_retired_content_keys`] for the same arity reason
    /// [`Self::predecessor_backup_keys`]'s doc records.
    retired_content_keys: Option<fauna_core::folder_keys::FolderContentKeys>,
    /// Rule (5)'s re-seal hold (`writer-signed-change-records.md` ruling
    /// (7)(b)(ii)): set when the build found a keyed, stamp-less serve custody
    /// the roster flags served — [`Self::reseal_predecessor_sealed`] waits for
    /// the owner's flip ON instead of walking the served-era back-catalogue
    /// onto the owner root and back.
    served_era_hold: bool,
    /// **Part (D)'s enabler** (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (5)): the manifests whose change
    /// records this reader VERIFIED as this account's own, under this set's
    /// nonce, on a set this account owns. Only such a manifest's **unstamped**
    /// (pre-bind) record opens under the owner root
    /// ([`fauna_core::file_download::FileDownloadKeys::owner_signed_record`]) —
    /// the retired nest-projected `set_owner` flag answered "is this account
    /// the owner?", which a nest could lie about, and could not tell a copied
    /// record of another set from this one's. Filled by the verify step of
    /// every fetch ([`Self::fetch_change_batch`]); in memory, so a restart
    /// opens nothing unstamped until the next fetch (the fail-closed
    /// direction). No seal site reads it.
    owner_signed_manifests: std::sync::RwLock<HashSet<ContentHash>>,
    /// The identity each manifest's verified record was **signed as** —
    /// ruling (8)(c)'s per-signer bound (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*): every open of that manifest — its
    /// bytes, its chunks — is offered only what that signer may open
    /// ([`fauna_core::file_download::FileDownloadKeys::record_signer`]). A
    /// record signed as the current identity wins: once one names a manifest,
    /// it reads as the current identity's whatever another row says
    /// ([`Self::note_manifest_signer`]). Filled beside
    /// [`Self::owner_signed_manifests`]. A **cache** over the per-entry
    /// `head_signed_as` column since `writer-signed-change-records.md` ruling
    /// (11)(d): what it does not hold is read from the entries whose head the
    /// manifest is ([`Self::manifest_signer`]).
    manifest_signers: std::sync::RwLock<
        std::collections::HashMap<ContentHash, fauna_core::file_download::RecordSigner>,
    >,
    /// Per path (its `path_hash`, lowercase hex): the manifest and identity of
    /// the verified row the reader last admitted for it — what the fold doors
    /// persist onto the entry they wrote ([`Self::persist_head_signers`]).
    pending_head_signers:
        std::sync::Mutex<std::collections::HashMap<String, (ContentHash, [u8; 32])>>,
    /// The pre-bind pass's verified heads for the current step-wise drive
    /// ([`Self::reseal_next_pending_under_current`]).
    prebind_heads_cache: Mutex<Option<PrebindHeads>>,
    /// Unit tests only — [`Self::into_verified_owner_for_test`].
    #[cfg(test)]
    test_owner_signed_all: std::sync::atomic::AtomicBool,
    /// The share leg's serve-side chunk index (slice E): recently served
    /// manifests' chunk coordinates, so [`Self::share_chunk_body`] re-derives
    /// one chunk from a plaintext range read. Manifest-anchored — see
    /// [`crate::peer_share_store::ShareServeMemo`].
    #[cfg(feature = "p2p-share")]
    share_serve_memo: std::sync::Mutex<crate::peer_share_store::ShareServeMemo>,
    /// Per-set conflict policy (file-sync.md § Conflicts, ratified
    /// 2026-07-10) — how [`Self::download_and_write_file`] auto-resolves a
    /// detected divergence. Read off the authoritative nest folder row by
    /// the constructing host; defaults to [`ConflictPolicy::Auto`].
    conflict_policy: fauna_core::format::ConflictPolicy,
    /// Pluggable format registry for semantic chunking and 3-way merge.
    format_registry: fauna_core::format::FormatRegistry,
    /// Ignore matcher: the built-in defaults + `.faunaignore` (loaded once at
    /// construction, from disk) **plus** the folder row's selective-sync
    /// `include_paths`/`exclude_paths`, which are re-installed **live** by
    /// [`Self::refresh_sync_mode`] on every posture refresh — the same
    /// row-refresh that carries the mode, the audience, the residency and the
    /// accepts gate (`file-sync.md` § Config: the row is the single
    /// authoritative source and *every device reads the row and applies it*).
    ///
    /// Interior-mutable for exactly that reason: a construction-time-only
    /// matcher is what made selective sync inert on both live desktop
    /// deployments — neither builder holds a control plane, so neither can read
    /// the row it would need (`engine_lifecycle::build_engine`,
    /// `fauna-sync-agent`'s `bridge.rs` hydration host).
    ignore: std::sync::RwLock<crate::ignore::IgnoreMatcher>,
    /// Number of concurrent chunk downloads.
    #[allow(dead_code)] // retained for planned adaptive parallelism
    parallel_downloads: usize,
    /// Transfer pool for semaphore-bounded chunk upload/download.
    transfer_pool: crate::transfer::TransferPool,
    /// WS-RPC client used to report conflicts (with candidate versions) via
    /// `fauna.sync.conflicts.report` — the engine's only outward channel for
    /// the rich conflict flow (`file-sync.md` § Conflicts). Shares the host's
    /// `AuthClient` with [`SyncEngine::client`].
    nest_client: Arc<fauna_client::NestClient>,
    /// The WS-RPC **control plane** ([`crate::nest_api::SyncControlApi`]) — the
    /// two kinds `fauna.sync.conflicts.report` and `fauna.folders.update`.
    /// Defaults in [`Self::new`] to the production
    /// [`crate::nest_api::WsRpcSyncControl`] over `nest_client`, so every caller
    /// keeps the transport it always had and `new()` keeps its arity (the same
    /// `set_tunnel_url` seam pattern `foreign_routing` follows).
    ///
    /// It exists as a seam because the branch below it is otherwise untestable:
    /// [`Self::auto_resolve_conflict`] yields `ResolvedApply::KeepLocal` only
    /// when the resolved report SUCCEEDS, and this crate's stateful-wiremock
    /// harness serves plain HTTP, which no WS-RPC call can reach — so before
    /// this seam every in-crate test of that path degraded to `Unresolved`.
    /// Swap in [`crate::nest_api::FakeSyncControl`] via
    /// [`Self::set_control_api`] to drive either arm.
    control: std::sync::RwLock<Arc<dyn crate::nest_api::SyncControlApi>>,
    /// Cross-nest control-plane routing (
    /// `federation.md` § Cross-nest shared folders).
    /// `Some((home_nest_url, channel_id_hex))` ⇒ this engine is bound to a set
    /// homed on **another** nest: both control-plane kinds carry
    /// `nest_url`+`channel_id` so this (the member's own) nest relays them to the
    /// set's home nest instead of answering locally, where a foreign set has no
    /// row — `changes.record` (the write) and `changes.list` (the change-log
    /// read, without which a bound foreign folder polls an empty local log
    /// forever and silently never pulls). The byte plane is pointed at the home
    /// nest by the caller (the [`SyncClient`]'s `AuthClient` base + a write-token
    /// `BearerSource`); only the control-plane relay lives here. `None` ⇒ an
    /// own-nest set, the production default. Set post-construction via
    /// [`Self::set_foreign_routing`] (the `set_tunnel_url` seam pattern —
    /// `new()` keeps its arity).
    foreign_routing: std::sync::RwLock<Option<(String, String)>>,
    /// Terminal `access-revoked` park state (D4, `file-sync.md` § Multi-writer
    /// shared sets). Flipped by whichever seam meets the authoritative nest's
    /// write-grant refusal first — the control-plane `changes.record` here, or
    /// the byte-plane [`crate::write_token_bearer::WriteTokenBearer`] mint, which
    /// is built *before* the engine and therefore shares this by `Arc`. Once
    /// set, the engine performs no further remote work for the set and touches
    /// nothing on disk. `new()` installs a private live gate; a host that needs
    /// to share one (the cross-nest byte plane, or its own status surface)
    /// swaps it in via [`Self::set_access_gate`].
    access_gate: Arc<crate::access_gate::AccessGate>,
    /// The IN-FLIGHT GUARD (`conflicts.md` clause 5, 2026-08-03): paths whose
    /// local content holds NOVEL published content whose own row has not yet
    /// echoed back — armed by the resolved-report outcomes (the report's
    /// loser/winner rows carry this device's candidate at seqs it cannot
    /// know) and by the apply pass at partition time for own non-resolution
    /// rows; cleared when an own row's echo matches local. While armed, the
    /// licence's settled conjunct is false: no fast-forward/adopt fires and
    /// divergences merge — the safe direction. (Plain edits need no arming
    /// here: `record_change` returns the seq, so the edit-frontier advances
    /// at the ack itself.)
    own_novel_in_flight: crate::own_novel_in_flight::OwnNovelInFlight,
    /// **`public`-audience folder** (folders re-model phase 4 —
    /// `folders.md` § Target re-model; the invariant exception is owned by
    /// `principles.md` § The user always controls their data): the owner
    /// explicitly declassified this folder, so its content is world-readable
    /// *by design* and every upload this engine performs rests **unsealed** —
    /// plaintext chunks (`stored_hashes = None`, the shape the serve path and
    /// every reader already accept) and plaintext labels
    /// ([`Self::label_seal_root`] → `None`).
    ///
    /// Deliberately a **setter-armed opt-in, default `false`**
    /// ([`Self::with_public_audience`]): a construction path that never heard
    /// of audiences keeps sealing — over-sealing a public folder is an
    /// availability lag (it serves after the next re-record), while
    /// under-sealing a private one is an unrecoverable disclosure. This flag is
    /// the ONLY thing that may legalise a plaintext upload of folder content —
    /// the keyless bails at the seal sites stay for every non-public engine.
    ///
    /// **Live, not once-per-build** (phase 4 slice 4c): armed at build off the
    /// folder list, then re-installed by [`Self::refresh_sync_mode`] on every
    /// resident tick off the SAME list read the sync mode rides — so a
    /// flip-to-private reaches a RUNNING seat's write path within one tick
    /// (the confidentiality-relevant direction), and a declassify within one
    /// tick likewise. Atomic because the drive loop refreshes while upload
    /// futures read.
    public_audience: std::sync::atomic::AtomicBool,
    /// Whether the last folder list this seat read carried a `public` claim it
    /// holds no trusted owner for (`SeatResolution::unanchored_public_claim`).
    /// Kept only so [`Self::refresh_sync_mode`] can log that claim's two EDGES
    /// rather than every tick; nothing reads it to decide anything.
    unanchored_public_claim: std::sync::atomic::AtomicBool,
    /// Phase 5 (`file-sync.md` § Content residency): `true` = the folder's
    /// owner opted into metadata-only residency, so this seat SKIPS uploading
    /// chunk bytes — the manifest and the change record still land (they are
    /// the metadata the nest keeps). Armed at build off the same folder list
    /// the audience rides ([`Self::with_metadata_only_residency`]) and
    /// re-installed live by [`Self::refresh_sync_mode`]; `false` is the
    /// fail-closed default — a construction path that never heard of
    /// residency uploads as always.
    metadata_only_residency: std::sync::atomic::AtomicBool,
    /// **Website toggle** (`folders.website_enabled` — `web-content-hosting.md`
    /// § Content model): `true` = this folder serves a website. Unlike the
    /// three postures above it steers **no** write-path decision; it is read by
    /// exactly one consumer, [`Self::converge_corpus_to_website`], whose whole
    /// job is to notice it turning ON for a SEALED folder and re-record the
    /// back-catalogue the nest structurally cannot backfill (sealed heads rest
    /// no plaintext name, S9, so they are outside the enable-time projection
    /// rebuild `folder_handlers`' `update_handler` runs).
    ///
    /// **Default `false`, installed live** by [`Self::refresh_sync_mode`] off
    /// the SAME `fauna.folders.list` read the mode rides — never armed at
    /// build, because the pass that reads it runs only *after* a refresh (the
    /// entry + tick of `always_resident::run_watch_loop`, and the one-shot
    /// pass). A default that never refreshes therefore means exactly "no work",
    /// which is the honest answer for an engine that never heard of the toggle.
    website_enabled: std::sync::atomic::AtomicBool,

    /// Whether this seat's place **accepts** remote changes — the
    /// `PlaceFlags::accepts` gate made real (folders re-model § Places: *"a
    /// seat only pulls folders where its place accepts"*; phase 2 slice c).
    /// `false` skips the remote-delivery rails whole:
    /// [`Self::pull_remote_changes`] and
    /// [`Self::populate_placeholders_from_nest`] return without fetching, so
    /// nothing remote is ever written to a source-only seat's disk — which is
    /// also what finally makes such a seat's `applies_deletes: false`
    /// non-vacuous (no delete arrives to decline).
    ///
    /// **Default `true`** — every seat's behavior before the flag became real,
    /// and the posture an unknown answer keeps ([`SeatResolution::accepts`] is
    /// `None` on an unreadable seat; the install below skips it). Live like
    /// the audience above: armed at build, re-installed by
    /// [`Self::refresh_sync_mode`] each resident tick off the same roster
    /// read the mode rides. Atomic for the same reason.
    accepts_remote: std::sync::atomic::AtomicBool,

    /// This seat's **exclusive-editing** state for the folder — the per-folder
    /// opt-in, the projection's reading of who holds the lease, and this
    /// device's own hold (`file-sync.md` § Exclusive editing).
    ///
    /// Live like the postures above: **default un-governed** (the flag fails
    /// OPEN, so an engine that never heard of exclusive editing writes exactly
    /// as it always did, and a lease-governed folder costs an un-governed one
    /// nothing), re-installed by [`Self::refresh_sync_mode`] off the SAME
    /// `fauna.folders.list` read everything else rides. The hold itself is NOT
    /// a posture — it is taken and given back by [`Self::open_lease_window`] /
    /// [`Self::close_lease_window`] around an upload pass, never per file.
    lease: crate::folder_lease::LeasePosture,
    /// The set's content-key floor as last read — decision 2's **pre-seal hold**
    /// (`on-demand-files.md` § Shared sets on a capability host):
    /// [`Self::content_seal_root`] refuses to seal under a generation the owner
    /// has rotated past, on every host alike. Armed at build from the row the
    /// binding was resolved from ([`Self::with_seal_floor`]) and re-installed by
    /// [`Self::refresh_sync_mode`] off the same list read everything else rides
    /// (an unreadable list keeps the armed posture). [`crate::binding_edge::SealFloor::Unarmed`]
    /// — a host with no row to arm it from — never holds. It also gates what is
    /// **published** ([`Self::publish_hold`], decision 2′): a failed pre-seal
    /// read keeps the last floor but marks it unread.
    seal_floor: std::sync::RwLock<crate::binding_edge::SealFloor>,
    /// A reader's engine: built over a set the account may only read, by a
    /// control-inverted host (`on-demand-files.md` § Shared sets on a
    /// capability host, decision 3). It pulls, lists and opens; the provider
    /// face refuses every write on it ([`crate::provider_face::ReadOnlyHost`]).
    /// Fixed at build ([`Self::with_read_only`]) — a grant change is a basis
    /// change, which rebuilds the engine.
    read_only: bool,
    /// The resident host's refresh-edge hook ([`Self::with_binding_edge`]):
    /// `None` on a host that drives its own edges (the control-inverted File
    /// Provider host re-reads the row before every seal) or none at all.
    binding_edge: Option<crate::binding_edge::BindingEdge>,
    /// The row basis the refresh edge last saw — seeded from the edge's build-time
    /// basis, advanced by every read. A basis MOVE asks the host to re-resolve
    /// once; an engine whose keys the re-resolve left unchanged (an access edit,
    /// say) is not asked again for the same row on every later read.
    seen_basis: std::sync::RwLock<Option<crate::binding_edge::BindingBasis>>,
    /// **Writer signing** (`mls-group-key-material.md` § M2 → *Writer-signed
    /// change records* (1)): the host's signer and this set's nonce, which
    /// every record [`Self::record_change`] sends is signed under. `None` — a
    /// host with no signer, or a set whose nonce custody does not hold yet —
    /// records unsigned and logs once; the nest refuses that once it enforces.
    /// Installed by [`Self::with_change_signer`], refreshed by
    /// [`Self::set_change_signer`] when a re-push carries a new nonce.
    change_signing: std::sync::RwLock<Option<ChangeSigning>>,
    /// Whether the unsigned-record warning was already logged (once per
    /// engine, not per file).
    warned_unsigned: std::sync::atomic::AtomicBool,
    /// Which half of [`Self::change_signing`] the last install lacked
    /// ([`UnsignedCause`] as a `u8`) — what the unsigned-record warning names,
    /// because the signer (the machine's enrollment) and the nonce (the set's
    /// custody) are absent for unrelated reasons.
    unsigned_cause: std::sync::atomic::AtomicU8,
    /// The set's other nonces from this incarnation (ruling (g)) — what
    /// [`Self::rerecord_under_live_nonce`] re-signs this device's heads off.
    retired_set_nonces: std::sync::RwLock<Vec<[u8; 32]>>,
    /// This device's adoption marker for the set
    /// (`writer-signed-change-records.md` ruling (11)(d)): the nonce this
    /// device's own re-mint replaced, carried on `FolderEngineKeys` — the
    /// take-over's licence to adopt the nest's history heads once.
    adoption_marker: std::sync::RwLock<Option<[u8; 32]>>,
    /// How this engine asks for the set's content-key envelope to be
    /// re-fetched ([`Self::set_custody_refetch_request`]); `None` on an
    /// engine whose host installed none.
    custody_refetch_request: std::sync::RwLock<Option<CustodyRefetchRequest>>,
    /// **Writer-signed change records, the reader half** (ruling (3)): the
    /// set's binding, the carried delegation certs and the writer roster every
    /// row [`Self::fetch_changes`] serves is judged against
    /// ([`fauna_protocol::sync_row_verify`]). Held apart from [`Self::change_signing`]: a
    /// member engine with no signer still verifies.
    row_reader: std::sync::RwLock<fauna_protocol::sync_row_verify::RowReader>,
    /// What this identity proved its own by the statement walk
    /// (`writer-signed-change-records.md` ruling (8)(b), source (ii), the
    /// half for a host handed no attested ids — [`Self::prove_own_links`]).
    /// A host that builds engines repeatedly shares one memory across them
    /// ([`Self::set_learned_predecessors`]), so a proven link is neither asked
    /// for again nor read as a fresh gain by the next build.
    learned_predecessors: fauna_client_sync::row_judge::LearnedPredecessors,
    /// The set's inbox of parked third-party deposits, armed by the build for
    /// a set the account owns ([`Self::set_deposit_inbox`]); `None` adopts
    /// nothing (`deposit_adoption`).
    deposit_inbox: std::sync::RwLock<Option<Arc<deposit_adoption::DepositInbox>>>,
}

/// An engine's signer bound to its one set's nonce.
#[derive(Clone)]
struct ChangeSigning {
    signer: std::sync::Arc<fauna_protocol::sync_writer_sig::ChangeSigner>,
    set_nonce: [u8; 32],
}

/// Which half of an engine's [`ChangeSigning`] its last install lacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum UnsignedCause {
    /// Both present: the engine signs.
    None = 0,
    /// The host passed no signer (no principal writer key, or a grant
    /// without `SyncWrite`) — an enrollment question.
    Signer = 1,
    /// Custody holds no nonce for the set — a custody question.
    Nonce = 2,
    /// Neither.
    Neither = 3,
}

impl UnsignedCause {
    fn of(has_signer: bool, has_nonce: bool) -> Self {
        match (has_signer, has_nonce) {
            (true, true) => Self::None,
            (false, true) => Self::Signer,
            (true, false) => Self::Nonce,
            (false, false) => Self::Neither,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::None,
            1 => Self::Signer,
            2 => Self::Nonce,
            _ => Self::Neither,
        }
    }

    /// What the unsigned-record warning names as missing.
    fn missing(self) -> &'static str {
        match self {
            Self::None => "nothing",
            Self::Signer => "signer",
            Self::Nonce => "set-nonce",
            Self::Neither => "signer+set-nonce",
        }
    }
}

#[cfg(test)]
mod unsigned_cause_tests {
    use super::UnsignedCause;

    /// The warning names the half that is absent, and the stored byte reads
    /// back as the cause it was written from.
    #[test]
    fn the_cause_names_the_absent_half() {
        for (signer, nonce, missing) in [
            (true, true, "nothing"),
            (false, true, "signer"),
            (true, false, "set-nonce"),
            (false, false, "signer+set-nonce"),
        ] {
            let cause = UnsignedCause::of(signer, nonce);
            assert_eq!(cause.missing(), missing);
            assert_eq!(UnsignedCause::from_u8(cause as u8), cause);
        }
    }
}

/// Install a fresh **authoritative** per-folder posture bit read from the
/// nest rows (`config::SeatResolution`'s audience / residency / accepts
/// fields) into an `AtomicBool`, logging only on an actual change — the one
/// failure discipline every such posture shares: `None` (the list was
/// unreadable) keeps the armed value, never resets it. `description` names
/// the posture in the log line, e.g. `"folder audience"`. Shared by
/// [`SyncEngine::refresh_sync_mode`]'s three installs.
pub fn install_authoritative_posture(
    cell: &std::sync::atomic::AtomicBool,
    new: Option<bool>,
    description: &str,
) {
    if let Some(new) = new {
        let was = cell.load(std::sync::atomic::Ordering::Relaxed);
        if was != new {
            tracing::info!(
                was,
                now = new,
                "{description} (re)resolved from the nest rows"
            );
        }
        cell.store(new, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The [`install_authoritative_posture`] twin for the folder row's
/// **selective-sync** filter lists — the posture that is a pair of path lists
/// rather than a flag.
///
/// Each of `include_paths`/`exclude_paths` substitutes **independently**:
/// `Some(list)` installs it; `None` — the row said nothing definitive for
/// that dimension (`crate::config::SelectiveSyncResolution` owns why) —
/// reapplies whatever this matcher is ALREADY armed with for that dimension
/// (`IgnoreMatcher::include_paths`/`exclude_paths`), unchanged. This is the
/// data guard (`path-sealing.md` § `folders.include_paths`/`exclude_paths`):
/// installing a blank filter over a dimension this reader merely could not
/// open — corrupted, or a nest-side edit that deleted the seal outright —
/// would sync precisely what the user excluded. A dimension that has never
/// been configured reapplies its own untouched-since-`Default` empty list,
/// which is exactly "no filter on that dimension" — so a row that only ever
/// configures one of the two (every production deployment today: the app's
/// two editors save independently) still arms correctly. Per-field, not
/// per-tick-all-or-nothing, is what makes both true at once.
///
/// The matcher's `.faunaignore` / built-in half is untouched — the config lists
/// are rebuilt wholesale, so this is idempotent tick after tick.
fn install_selective_sync(
    cell: &std::sync::RwLock<crate::ignore::IgnoreMatcher>,
    res: &crate::config::SelectiveSyncResolution,
) {
    let mut guard = cell.write().unwrap();
    if res.include_paths.is_none() || res.exclude_paths.is_none() {
        tracing::warn!(
            "the folder row's selective-sync lists were unreadable for at least one \
             dimension; keeping that dimension's armed filter"
        );
    }
    let include = res
        .include_paths
        .clone()
        .unwrap_or_else(|| guard.include_paths().to_vec());
    let exclude = res
        .exclude_paths
        .clone()
        .unwrap_or_else(|| guard.exclude_paths().to_vec());
    let was_filtering = !guard.is_empty();
    guard.apply_config_patterns(&include, &exclude);
    let now_filtering = !guard.is_empty();
    if was_filtering != now_filtering {
        tracing::info!(
            includes = include.len(),
            excludes = exclude.len(),
            "selective-sync filter (re)resolved from the nest rows"
        );
    }
}

impl SyncEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        watch_dir: PathBuf,
        db: SyncDb,
        client: SyncClient,
        folder: Option<String>,
        device_id: [u8; 32],
        mls: Option<Arc<MlsEngine>>,
        epoch_secret: Option<[u8; 32]>,
        backup_key: Option<fauna_core::crypto::OwnerSealKey>,
        mls_group_id: Option<Vec<u8>>,
        content_keys: Option<fauna_core::folder_keys::FolderContentKeys>,
        conflict_policy: fauna_core::format::ConflictPolicy,
        format_registry: fauna_core::format::FormatRegistry,
        ignore: crate::ignore::IgnoreMatcher,
        parallel_downloads: usize,
        transfer_pool: crate::transfer::TransferPool,
        nest_client: Arc<fauna_client::NestClient>,
        mode: crate::config::SyncMode,
    ) -> Self {
        // The engine's byte client reads for exactly this folder, so it names
        // it on every chunk GET — the relay hint that lets a store miss be
        // served from a holding seat (`file-sync.md` § Content residency).
        let mut client = client;
        client.set_folder_hint(folder.clone());
        let base_dir =
            Self::resolve_state_dir(&watch_dir, folder.as_deref(), &device_id, ".fauna-bases");
        let causal_dir =
            Self::resolve_state_dir(&watch_dir, folder.as_deref(), &device_id, ".fauna-causal");
        Self {
            watch_dir,
            base_dir,
            causal_dir,
            db,
            client,
            folder,
            device_id,
            mode: std::sync::RwLock::new(mode.into()),
            mls,
            recent_writes: Mutex::new(HashSet::new()),
            recent_removals: Mutex::new(HashSet::new()),
            placeholders_off_disk: std::sync::atomic::AtomicBool::new(false),
            epoch_secret,
            backup_key,
            predecessor_backup_keys: Vec::new(),
            mls_group_id,
            content_keys,
            retired_content_keys: None,
            served_era_hold: false,
            owner_signed_manifests: std::sync::RwLock::new(HashSet::new()),
            manifest_signers: std::sync::RwLock::new(std::collections::HashMap::new()),
            pending_head_signers: std::sync::Mutex::new(std::collections::HashMap::new()),
            prebind_heads_cache: Mutex::new(None),
            #[cfg(test)]
            test_owner_signed_all: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "p2p-share")]
            share_serve_memo: std::sync::Mutex::new(Default::default()),
            conflict_policy,
            format_registry,
            ignore: std::sync::RwLock::new(ignore),
            parallel_downloads,
            transfer_pool,
            control: std::sync::RwLock::new(Arc::new(crate::nest_api::WsRpcSyncControl::new(
                Arc::clone(&nest_client),
            ))),
            nest_client,
            foreign_routing: std::sync::RwLock::new(None),
            access_gate: crate::access_gate::AccessGate::new(),
            own_novel_in_flight: crate::own_novel_in_flight::OwnNovelInFlight::default(),
            public_audience: std::sync::atomic::AtomicBool::new(false),
            unanchored_public_claim: std::sync::atomic::AtomicBool::new(false),
            metadata_only_residency: std::sync::atomic::AtomicBool::new(false),
            accepts_remote: std::sync::atomic::AtomicBool::new(true),
            website_enabled: std::sync::atomic::AtomicBool::new(false),
            // Un-governed, unheld, unrefused — the flag's fail-open default, so
            // an engine that never refreshes writes exactly as it always did.
            lease: crate::folder_lease::LeasePosture::default(),
            seal_floor: std::sync::RwLock::new(crate::binding_edge::SealFloor::Unarmed),
            read_only: false,
            binding_edge: None,
            seen_basis: std::sync::RwLock::new(None),
            change_signing: std::sync::RwLock::new(None),
            warned_unsigned: std::sync::atomic::AtomicBool::new(false),
            unsigned_cause: std::sync::atomic::AtomicU8::new(UnsignedCause::Neither as u8),
            retired_set_nonces: std::sync::RwLock::new(Vec::new()),
            adoption_marker: std::sync::RwLock::new(None),
            custody_refetch_request: std::sync::RwLock::new(None),
            row_reader: std::sync::RwLock::new(fauna_protocol::sync_row_verify::RowReader::new()),
            learned_predecessors: Default::default(),
            deposit_inbox: std::sync::RwLock::new(None),
        }
    }

    /// Sign every record this engine writes with `signer` under this set's
    /// `set_nonce` (see the field). Chainable beside [`Self::new`] like
    /// [`Self::with_public_audience`]; a `None` nonce leaves the engine
    /// recording unsigned.
    pub fn with_change_signer(
        self,
        signer: std::sync::Arc<fauna_protocol::sync_writer_sig::ChangeSigner>,
        set_nonce: Option<[u8; 32]>,
    ) -> Self {
        self.set_change_signer(Some(signer), set_nonce);
        self
    }

    /// Install (or clear) the signer and nonce on a live engine — a re-push
    /// that carries a re-minted nonce, or a signer that became available.
    pub fn set_change_signer(
        &self,
        signer: Option<std::sync::Arc<fauna_protocol::sync_writer_sig::ChangeSigner>>,
        set_nonce: Option<[u8; 32]>,
    ) {
        let cause = UnsignedCause::of(signer.is_some(), set_nonce.is_some());
        self.unsigned_cause
            .store(cause as u8, std::sync::atomic::Ordering::Relaxed);
        let signing = signer
            .zip(set_nonce)
            .map(|(signer, set_nonce)| ChangeSigning { signer, set_nonce });
        *self
            .change_signing
            .write()
            .unwrap_or_else(|e| e.into_inner()) = signing;
    }

    /// Swap the signer on a live engine, keeping its set nonce — a capability
    /// host whose provisioned principal was re-minted since the build (ruling
    /// (1), *The capability host*). `false` when the engine holds no signing
    /// to swap into (built without a signer, or no nonce resolved): its caller
    /// rebuilds, which is what loads the custody the nonce lives in.
    pub fn replace_change_signer(
        &self,
        signer: std::sync::Arc<fauna_protocol::sync_writer_sig::ChangeSigner>,
    ) -> bool {
        let mut signing = self
            .change_signing
            .write()
            .unwrap_or_else(|e| e.into_inner());
        match signing.as_mut() {
            Some(s) => {
                s.signer = signer;
                true
            }
            None => false,
        }
    }

    /// Install the set's retired nonces (ruling (g); see the field) — refreshed
    /// with the live nonce whenever a re-push carries custody.
    pub fn set_retired_set_nonces(&self, retired: Vec<[u8; 32]>) {
        *self
            .retired_set_nonces
            .write()
            .unwrap_or_else(|e| e.into_inner()) = retired;
    }

    /// Install this device's adoption marker for the set (see the field) —
    /// refreshed whenever a re-push carries custody.
    ///
    /// A marker this store has never seen is judged **as it arrives**
    /// (`writer-signed-change-records.md` ruling (11)(d)): a device that held
    /// local state for the set already vouches for what it holds and adopts
    /// nothing from the nest — the marker is spent unspent; one that held none
    /// begins an adoption the take-over completes ([`AdoptionState`]). Judged
    /// here, before any fold of the set, so an adoption that skipped every
    /// head never reads as a fresh device the next time the app re-pushes the
    /// keys.
    pub fn set_adoption_marker(&self, marker: Option<[u8; 32]>) {
        if let Some(nonce) = marker.as_ref() {
            let judged = match self.db.adoption_state(nonce) {
                Ok(Some(_)) => Ok(()),
                Ok(None) => self.db.has_any_entry().and_then(|held| {
                    self.db.set_adoption_state(
                        nonce,
                        if held {
                            crate::db::AdoptionState::Spent
                        } else {
                            crate::db::AdoptionState::Begun
                        },
                    )
                }),
                Err(e) => Err(e),
            };
            if let Err(e) = judged {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "judging the adoption marker failed; the take-over adopts nothing from the \
                     nest until it is judged"
                );
            }
        }
        *self
            .adoption_marker
            .write()
            .unwrap_or_else(|e| e.into_inner()) = marker;
    }

    /// The adoption marker [`Self::set_adoption_marker`] last installed.
    pub fn adoption_marker(&self) -> Option<[u8; 32]> {
        *self
            .adoption_marker
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Install how this engine asks for the set's content-key envelope to be
    /// re-fetched (`writer-signed-change-records.md` ruling (11)(b)): called
    /// when a batch of served rows holds one refused `signature_invalid` on a
    /// set this account does not own — the nonce this engine was built with
    /// is not the one the set's writers sign under, and only the process that
    /// holds the MLS group can open the envelope that names the new one. Must
    /// not block: the host spawns or notifies.
    pub fn set_custody_refetch_request(&self, request: CustodyRefetchRequest) {
        *self
            .custody_refetch_request
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(request);
    }

    /// Install what this engine's reader judges served rows against — the
    /// set's live nonce, its owner and serve flag
    /// (`fauna_protocol::sync_row_verify::ReaderBinding`). Re-installed with the
    /// signer whenever a re-push carries custody; the certs and the roster
    /// already read stand.
    pub fn set_reader_binding(&self, binding: fauna_protocol::sync_row_verify::ReaderBinding) {
        self.row_reader
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .install_binding(binding);
    }

    /// The binding [`Self::set_reader_binding`] last installed.
    #[cfg(test)]
    pub(crate) fn reader_binding(&self) -> fauna_protocol::sync_row_verify::ReaderBinding {
        self.row_reader
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .binding()
            .clone()
    }

    /// Install a writer roster as [`Self::refresh_reader_roster`] would after
    /// a successful read.
    #[cfg(test)]
    pub(crate) fn install_reader_roster(
        &self,
        writers: fauna_protocol::sync_row_verify::WriterRoster,
    ) {
        self.row_reader
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .install_roster(writers);
    }

    /// Share the host's statement-walk memory with this engine
    /// ([`Self::learned_predecessors`]). The binding is the builder's to seed
    /// from it (`engine_lifecycle::assemble_engine`).
    pub fn set_learned_predecessors(
        &mut self,
        learned: fauna_client_sync::row_judge::LearnedPredecessors,
    ) {
        self.learned_predecessors = learned;
    }

    /// **The statement walk, on the engine** (`writer-signed-change-records.md`
    /// ruling (8)(b), source (ii)): a row that verified cryptographically but
    /// whose signed actor resolved to no writer — on a set this account may
    /// write — may be a retired identity of this account that no host attested
    /// to this engine (the in-process FFI hosts, a capability host). Ask the
    /// own nest for the succession path forward from each such actor
    /// ([`LearnedPredecessors::prove_links`](fauna_client_sync::row_judge::LearnedPredecessors::prove_links),
    /// the one walk the projection readers run) and bind what it proves ends
    /// at this identity: the reader's `account_predecessors` and, on a set this
    /// account owns, the ordered `owner_chain` ruling (11)(c) places a nonce's
    /// minter on. `true` when the binding gained (the caller judges again).
    async fn prove_own_links<R>(
        &self,
        links: &R,
        changes: &[fauna_protocol::sync::SyncChange],
        verdicts: &[fauna_protocol::sync_row_verify::RowVerdict],
    ) -> bool
    where
        R: fauna_protocol::RpcRequester,
        R::Error: fauna_protocol::RpcErrorClass,
    {
        let own = self.own_actor_id().0;
        let unplaced: Vec<[u8; 32]> = {
            let reader = self.row_reader.read().unwrap_or_else(|e| e.into_inner());
            if !reader.is_writer(&own) {
                return false;
            }
            let mut unplaced = Vec::new();
            for (change, verdict) in changes.iter().zip(verdicts) {
                if verdict.unattributed()
                    && let Some(signed_as) = reader.signed_actor(change)
                    && !unplaced.contains(&signed_as)
                {
                    unplaced.push(signed_as);
                }
            }
            unplaced
        };
        if unplaced.is_empty()
            || !self
                .learned_predecessors
                .prove_links(links, own, unplaced)
                .await
        {
            return false;
        }
        let mut reader = self.row_reader.write().unwrap_or_else(|e| e.into_inner());
        let mut binding = reader.binding().clone();
        binding.account_predecessors = self
            .learned_predecessors
            .chain_with(&binding.account_predecessors);
        if binding.owner == Some(own) {
            binding.owner_chain = binding.account_predecessors.clone();
        }
        reader.install_binding(binding);
        true
    }

    /// **The reader's writer-roster leg** (ruling (3)) — ungated, unlike the
    /// share leg's [`Self::refresh_share_writer_roster`]: one
    /// `fauna.folders.members.list_actors` read replaces the reader's roster
    /// with the set's `writer`-access members. `not_shared` is a successful
    /// read of an owner-only set (the roster is the owner, who is always a
    /// writer); a failed read keeps the last roster (offline, the last roster
    /// stands; never read ⇒ owner-only, and a member's signed rows hold the
    /// pull until a read succeeds). A cross-nest member's engine reads the SAME
    /// roster relayed to the set's home nest
    /// (`fauna.folders.members.list_actors_remote`, `list_changes`'s routing),
    /// under the same writer filter — the home nest's `role == "owner"` row
    /// keeps the owner in the installed roster beside the MLS-recorded owner
    /// seed. Any failed read (a transport error, `unknown_kind`,
    /// `peer_nest_outdated`) leaves the roster unread and the member's rows
    /// hold. Rides [`Self::refresh_sync_mode`]'s cadence and runs
    /// on demand before a batch that needs it.
    pub async fn refresh_reader_roster(&self) {
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let client = fauna_client_folders::FoldersClient::new(self.control_plane());
        let read = match self.foreign_routing() {
            Some((home_nest_url, channel_id_hex)) => {
                client
                    .actor_members_list_remote(channel_id_hex, home_nest_url)
                    .await
            }
            None => client.actor_members_list(&folder).await,
        };
        let writers = match read {
            // Each writer with the predecessors its carried succession
            // statements prove (ruling (8)(b), source (i)) — a member's rows
            // signed under a retired identity resolve to the member.
            Ok(reply) => fauna_protocol::sync_row_verify::writer_roster(&reply.members),
            Err(e)
                if fauna_protocol::RpcErrorClass::as_rpc_error(&e).is_some_and(|r| {
                    r.code == fauna_protocol::sync_row_verify::ROSTER_NOT_SHARED
                }) =>
            {
                fauna_protocol::sync_row_verify::WriterRoster::default()
            }
            Err(e) => {
                tracing::debug!(
                    folder = %fauna_core::log_redact::log_folder_name(&folder),
                    error = %e,
                    "reader writer roster: read failed; the last roster stands"
                );
                return;
            }
        };
        self.row_reader
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .install_roster(writers);
    }

    /// The thin record client for the same-nest plane, carrying this engine's
    /// signing so [`fauna_client_sync::SyncClient::changes_record`] signs
    /// what it sends (unsigned, logged once, when the engine has none).
    fn record_client(&self) -> fauna_client_sync::SyncClient<Arc<fauna_client::NestClient>> {
        let client = fauna_client_sync::SyncClient::new(self.nest_client.clone());
        let signing = self
            .change_signing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match signing {
            Some(s) => {
                let nonce = s.set_nonce;
                client.with_record_signing(fauna_client_sync::RecordSigning {
                    signer: s.signer,
                    set_nonce: fauna_client_sync::SetNonceSource::Fixed(nonce),
                })
            }
            None => {
                self.warn_unsigned_once();
                client
            }
        }
    }

    /// The unsigned-record warning, once per engine.
    fn warn_unsigned_once(&self) {
        if !self
            .warned_unsigned
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            let missing = UnsignedCause::from_u8(
                self.unsigned_cause
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
            .missing();
            tracing::warn!(
                folder = self.folder().unwrap_or("?"),
                missing,
                "this engine has no change signer or set nonce: its records go out \
                 unsigned, which a nest enforcing writer signatures refuses"
            );
        }
    }

    /// Sign `req` with this engine's signer, or log (once) that it goes out
    /// unsigned. The one signing point both record planes share.
    fn sign_record(&self, req: &mut fauna_protocol::sync::SyncChangeRecordRequest) {
        let signing = self
            .change_signing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match signing {
            Some(s) => {
                if let Err(e) = s.signer.sign_record(req, s.set_nonce) {
                    tracing::warn!(folder = %req.folder, "change record left unsigned: {e}");
                }
            }
            None => self.warn_unsigned_once(),
        }
    }

    /// Sign a retained own row as the share leg will SERVE it
    /// ([`wire_change`] — the one field map both use)
    /// under this engine's signer and nonce, keeping the delegated cert beside
    /// it so the row stays self-contained on a peer hop
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (3)). A host with no signing leaves the row unsigned — every
    /// receiver then refuses it.
    fn sign_own_row(&self, row: &mut crate::db::OwnChangeRow) {
        let signing = self
            .change_signing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(s) = signing else {
            return;
        };
        let mut wire = wire_change(row, row.seq.unwrap_or(0));
        if let Err(e) = s.signer.sign_row(&mut wire, s.set_nonce) {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(&row.path),
                "retained own row left unsigned: {e}"
            );
            return;
        }
        row.signature = wire.signature.map(|b| b.into_vec());
        row.signer_key = wire.signer_key.map(|b| b.into_vec());
        row.signer_cert = s
            .signer
            .carried_cert()
            .and_then(|c| fauna_protocol::encode_canonical(c).ok())
            .map(|b| b.to_vec());
    }

    /// Sign a resolved report's winner head row with this engine's signer
    /// (ruling (1)(ii) — the reporter signs the row the nest mints), or log
    /// (once) that it goes out unsigned. An unresolved report is untouched.
    fn sign_conflict_report(&self, req: &mut ConflictReportRequest) {
        if req.resolution.is_none() {
            return;
        }
        let signing = self
            .change_signing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match signing {
            Some(s) => {
                if let Err(e) = s.signer.sign_report(req, s.set_nonce) {
                    tracing::warn!(folder = %req.folder, "conflict report left unsigned: {e}");
                }
            }
            None => self.warn_unsigned_once(),
        }
    }

    /// Arm the **`public`-audience** write path (see the field's doc): uploads
    /// rest unsealed — plaintext chunks and labels. Chainable builder beside
    /// [`Self::new`] so the 25+ existing construction sites keep their arity;
    /// the default (`false`) is the fail-safe direction (over-seal, never
    /// under-seal). The constructing host arms it from the **owner-attested
    /// verdict** over the folder's row (`FolderSummary::judge_declassification`
    /// — never the bare `audience` field), and the resident tick re-judges it
    /// ([`Self::refresh_sync_mode`]).
    pub fn with_public_audience(self, public_audience: bool) -> Self {
        self.public_audience
            .store(public_audience, std::sync::atomic::Ordering::Relaxed);
        self
    }

    /// This seat's own actor id as the declassification anchor's `own`
    /// ([`Self::resolve_sync_mode`]): [`Self::owner_actor_id_hex`] decoded. The
    /// nest client's actor id is the hex of a 32-byte key by construction; an
    /// unparseable one is a zero id that matches no signer, so the seat would
    /// seal its own folders — loud, never silently armed.
    fn own_actor_id(&self) -> fauna_core::identity::ActorId {
        fauna_core::identity::ActorId::from_hex(&self.owner_actor_id_hex()).unwrap_or_else(|e| {
            tracing::error!(error = %e, "the seat's own actor id is not a 32-byte hex id");
            fauna_core::identity::ActorId([0u8; 32])
        })
    }

    /// Unit tests only: this engine is the set's owner and every record it
    /// reads verified as its own — the key selection and the pass's byte plane
    /// under test, not the verify step (pinned against a real nest).
    #[cfg(test)]
    pub(crate) fn into_verified_owner_for_test(self) -> Self {
        self.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
            owner: Some(self.own_actor_id().0),
            ..Default::default()
        });
        self.test_owner_signed_all
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self
    }

    /// Note a manifest whose record verified as this account's own on a set
    /// it owns ([`Self::owner_signed_manifests`]). The verify step's, and a
    /// unit test's that exercises the key selection alone.
    pub(crate) fn note_owner_signed(&self, manifest: ContentHash) {
        self.owner_signed_manifests
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(manifest);
    }

    /// Whether this account owns the set, per the reader binding (this
    /// account for an owned set's row; the MLS-recorded owner for a
    /// member's). The binding alone: an owner the judge reads off the
    /// roster's owner row never makes this account the owner. Divides work
    /// only — no open is ever decided by it alone.
    fn owns_set(&self) -> bool {
        self.row_reader
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .binding()
            .owner
            == Some(self.own_actor_id().0)
    }

    /// Whether `manifest`'s record verified as this account's own on a set it
    /// owns — part (D)'s per-record gate.
    fn record_is_owner_signed(&self, manifest: &ContentHash) -> bool {
        #[cfg(test)]
        if self
            .test_owner_signed_all
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return true;
        }
        self.owner_signed_manifests
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(manifest)
    }

    /// The identity a row SIGNED AS `signed_as` is to the per-signer bound
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(c)): the current identity is
    /// [`RecordSigner::Current`](fauna_core::file_download::RecordSigner::Current),
    /// a retired identity of this account — one of the reader binding's proven
    /// `account_predecessors` — is
    /// [`RecordSigner::Predecessor`](fauna_core::file_download::RecordSigner::Predecessor),
    /// offered only its own chain's roots, and any other writer is
    /// [`RecordSigner::Other`](fauna_core::file_download::RecordSigner::Other),
    /// offered none of the owner family.
    pub(crate) fn record_signer_of(
        &self,
        signed_as: &[u8; 32],
    ) -> fauna_core::file_download::RecordSigner {
        use fauna_core::file_download::RecordSigner;
        if *signed_as == self.own_actor_id().0 {
            RecordSigner::Current
        } else if self.is_own_account(signed_as) {
            RecordSigner::Predecessor(fauna_core::identity::ActorId(*signed_as))
        } else {
            RecordSigner::Other
        }
    }

    /// Whether `signed_as` is this account — its current identity, or one of
    /// the reader binding's proven `account_predecessors` (ruling (8)(b),
    /// source (ii)).
    fn is_own_account(&self, signed_as: &[u8; 32]) -> bool {
        *signed_as == self.own_actor_id().0
            || self
                .row_reader
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .binding()
                .account_predecessors
                .contains(signed_as)
    }

    /// The bound for a row as this engine holds it after the verify step,
    /// which rewrote an admitted row's `author_actor_id` to the identity it
    /// was signed as ([`Self::verify_served_rows`]). An exempt row keeps its
    /// served stamp and no signature names it: it reads as the current
    /// identity's, today's offer — an exempt row is a served set's, whose
    /// names and bytes are content-keyed.
    pub(crate) fn record_signer_of_row(
        &self,
        change: &fauna_protocol::sync::SyncChange,
    ) -> fauna_core::file_download::RecordSigner {
        change
            .author_actor_id
            .as_deref()
            .and_then(|a| fauna_core::hex32::decode(a).ok())
            .map_or(fauna_core::file_download::RecordSigner::Current, |a| {
                self.record_signer_of(&a)
            })
    }

    /// Note who `manifest`'s verified record was signed as
    /// ([`Self::manifest_signers`]). Of several rows naming one manifest the
    /// widest offer wins
    /// ([`RecordSigner::wider`](fauna_core::file_download::RecordSigner::wider)).
    pub(crate) fn note_manifest_signer(
        &self,
        manifest: ContentHash,
        signer: fauna_core::file_download::RecordSigner,
    ) {
        let mut signers = self
            .manifest_signers
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let kept = match signers.get(&manifest) {
            Some(held) => held.wider(signer, &self.predecessor_backup_keys),
            None => signer,
        };
        signers.insert(manifest, kept);
    }

    /// Who `manifest`'s record was signed as, for the bound on every open of
    /// it ([`Self::download_keys_for_record`]).
    ///
    /// The verdict this process admitted first (the cache); else the signer
    /// **persisted on the entries** whose head the manifest is
    /// (`writer-signed-change-records.md` ruling (11)(d)) — the widest of
    /// them — so a placeholder a predecessor's row planted does not read as
    /// the current identity's after a restart. A manifest no entry holds and
    /// no verified row named reads as the current identity's, the ordinary
    /// offer. An entry holding the manifest with **no signer recorded** opens
    /// under no owner root (fail closed): every door that moves an entry's
    /// manifest names its signer (`SyncDb::update_recorded_head`,
    /// `SyncDb::set_head_signed_as`), so an unknown one is a row this device
    /// cannot vouch for.
    fn manifest_signer(&self, manifest: &ContentHash) -> fauna_core::file_download::RecordSigner {
        use fauna_core::file_download::RecordSigner;
        if let Some(held) = self
            .manifest_signers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(manifest)
            .copied()
        {
            return held;
        }
        match self.db.head_signers_of_manifest(manifest) {
            Ok(entries) if entries.is_empty() => RecordSigner::default(),
            Ok(entries) => entries
                .into_iter()
                .map(|signed_as| {
                    signed_as.map_or(RecordSigner::Other, |a| self.record_signer_of(&a))
                })
                .reduce(|held, next| held.wider(next, &self.predecessor_backup_keys))
                .unwrap_or(RecordSigner::Other),
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "reading a head's persisted signer failed; offering no owner root"
                );
                RecordSigner::Other
            }
        }
    }

    /// Unit tests only: forget every signer this process admitted — what a
    /// restart does to the cache, leaving the persisted column to answer.
    #[cfg(test)]
    pub(crate) fn forget_admitted_signers_for_test(&self) {
        self.manifest_signers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Unit tests only: stamp `path`'s head as signed by this engine's own
    /// identity — what every record this device makes writes — for a test that
    /// seeds an entry by hand (an entry with no signer opens under no owner
    /// root).
    #[cfg(test)]
    pub(crate) fn stamp_own_head_for_test(&self, path: &str) {
        let entry = self
            .db
            .get_entry(path)
            .expect("read the seeded entry")
            .expect("the seeded entry");
        let manifest = entry.manifest_hash.expect("a seeded head");
        assert!(
            self.db
                .set_head_signed_as(path, &manifest, &self.own_actor_id().0)
                .expect("stamp the seeded head")
        );
    }

    /// Persist who each folded row's head was signed as onto the entry the
    /// fold wrote for it (ruling (11)(d): durable per local entry, at the
    /// fold). Stamps only an entry that holds the very manifest the verified
    /// row named; a row the fold did not apply stamps nothing.
    fn persist_head_signers(&self, changes: &[fauna_protocol::sync::SyncChange]) {
        let mut pending = self
            .pending_head_signers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending.is_empty() {
            return;
        }
        for change in changes {
            let Some(path) = change.path.as_deref().filter(|p| !p.is_empty()) else {
                continue;
            };
            let key = change.path_hash.to_ascii_lowercase();
            let Some((manifest, signed_as)) = pending.get(&key).copied() else {
                continue;
            };
            match self.db.set_head_signed_as(path, &manifest, &signed_as) {
                Ok(true) => {
                    pending.remove(&key);
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    error = %format!("{e:#}"),
                    "persisting a head's signer failed; the entry reads as unknown after a restart"
                ),
            }
        }
    }

    /// Arm (or disarm) the phase-5 metadata-only residency posture at build —
    /// the [`Self::with_public_audience`] sibling, seeded from the same
    /// folder-list read (`SeatResolution::metadata_only_residency`).
    pub fn with_metadata_only_residency(self, metadata_only: bool) -> Self {
        self.install_residency(Some(metadata_only));
        self
    }

    /// Install the residency reading a binding resolved at build: `Some` is
    /// [`Self::with_metadata_only_residency`]; `None` — a cross-nest set whose
    /// custody record no home nest has stamped — is *unknown*: the live
    /// posture stays off (the seat uploads; nothing unparseable may stop bytes
    /// resting) and any persisted reading is CLEARED, so the holder-keeps gate
    /// keeps every own-record body rather than read a stale *full*.
    pub fn with_residency_reading(self, metadata_only: Option<bool>) -> Self {
        match metadata_only {
            Some(metadata_only) => self.with_metadata_only_residency(metadata_only),
            None => {
                if let Err(e) = self.db.clear_residency_reading() {
                    tracing::warn!(
                        error = %format!("{e:#}"),
                        "clearing the folder's residency reading failed; the dehydration gate keeps the previous one"
                    );
                }
                self
            }
        }
    }

    /// Install a residency reading — the live posture the upload gates read,
    /// and its persisted copy the dehydration gate reads when a body is freed
    /// ([`Self::is_dehydration_safe_in`]). `None` (the list was unreadable)
    /// keeps both, the discipline of every authoritative posture. A failed
    /// write leaves the previous persisted reading; one that was never written
    /// keeps every own-record body, the safe direction.
    pub(crate) fn install_residency(&self, metadata_only: Option<bool>) {
        install_authoritative_posture(
            &self.metadata_only_residency,
            metadata_only,
            "folder content residency",
        );
        if let Some(metadata_only) = metadata_only
            && let Err(e) = self.db.set_residency_reading(metadata_only)
        {
            tracing::warn!(
                error = %format!("{e:#}"),
                "persisting the folder's residency reading failed; the dehydration gate keeps the previous one"
            );
        }
    }

    /// Arm the pre-seal hold at build from the floor the set's row carried
    /// (see the `seal_floor` field). The builder the File Provider host's
    /// [`crate::engine_lifecycle::build_engine`] takes; a resident host arms it
    /// through [`Self::with_binding_edge`].
    pub fn with_seal_floor(self, floor: crate::binding_edge::SealFloor) -> Self {
        *self.seal_floor.write().unwrap() = floor;
        self
    }

    /// Mark this engine a reader's (see the `read_only` field) — set by
    /// [`crate::engine_lifecycle::assemble_engine`] from the resolved binding.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Whether this engine is a reader's — it has no write half.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Install a resident host's refresh edge (`on-demand-files.md` § Shared sets
    /// on a capability host, decision 2): the pre-seal hold is armed from the
    /// edge's basis, and every [`Self::refresh_sync_mode`] tick that reads the
    /// set's row compares it against that basis and calls the edge's
    /// `on_rebuild` when the keys no longer answer it. The desktop sync agent's
    /// resident engines take this; the control-inverted host drives its own.
    pub fn with_binding_edge(self, edge: crate::binding_edge::BindingEdge) -> Self {
        let floor = crate::binding_edge::SealFloor::Floor(edge.basis.content_key_floor());
        let mut this = self.with_seal_floor(floor);
        *this.seen_basis.write().unwrap() = Some(edge.basis.clone());
        this.binding_edge = Some(edge);
        this
    }

    /// The content-key generation this engine seals under — `None` for an
    /// owner-only set and for a bound set built keyless.
    pub fn held_content_key_version(&self) -> Option<u64> {
        self.content_keys
            .as_ref()
            .map(fauna_core::folder_keys::FolderContentKeys::current_version)
    }

    /// Why a seal now would be held rather than sealed under the generation this
    /// engine holds — decision 2's pre-seal hold, the ONE implementation every
    /// host runs: [`Self::content_seal_root`] refuses on it. `None` = seal. A
    /// floor the last read could not refresh still binds here (decision 2′
    /// (a)); what that seal may *send* is [`Self::publish_hold`]'s.
    pub fn seal_hold(&self) -> Option<crate::binding_edge::SealHold> {
        crate::binding_edge::seal_hold(
            *self.seal_floor.read().unwrap(),
            self.held_content_key_version(),
        )
    }

    /// Why sealed content may not be **sent to a nest** now — decision 2′ (c):
    /// every [`Self::seal_hold`], plus a pass whose pre-seal row read failed.
    /// Asked between the local seal (and its own-pending mint, which the peer
    /// plane serves) and the first byte to a nest: the in-memory and streaming
    /// upload halves, the windowed re-seal's chunk puts, the queued-upload
    /// drain and [`Self::record_change`]. The File Provider host asks it before
    /// a write, which it acknowledges only once recorded. A `public`-audience
    /// set seals nothing, so nothing of it is held. `None` = publish.
    pub fn publish_hold(&self) -> Option<crate::binding_edge::SealHold> {
        if self.is_public_audience() {
            return None;
        }
        crate::binding_edge::publish_hold(
            *self.seal_floor.read().unwrap(),
            self.held_content_key_version(),
        )
    }

    /// Decision 2's **pre-seal row read** for a resident host (the File Provider
    /// host reads before every write on its own): re-read the set's row and
    /// re-install the floor, so the seals that follow are held whenever the
    /// owner has rotated past the generation this engine holds, however
    /// recently. When the list cannot be read the last floor read is kept and
    /// marked unread ([`crate::binding_edge::SealFloor::after_failed_read`],
    /// decision 2′): the batch seals under it and mints its own-pending rows,
    /// and [`Self::publish_hold`] keeps all of it off the nest. Called once per upload batch (a watcher flush, an
    /// upload pass), the same granularity as the lease window. A moved row also
    /// asks the host to re-resolve ([`Self::with_binding_edge`]). A no-op for an
    /// engine with no edge installed, and for a cross-nest set (no row here).
    pub async fn refresh_seal_floor(&self) {
        let Some(edge) = self.binding_edge.as_ref() else {
            return;
        };
        if !matches!(
            edge.folder_ref,
            fauna_core::folder_keys::FolderRef::Local(_)
        ) {
            return;
        }
        let control = self.control_plane();
        // A freshly built engine's control plane is still connecting when its
        // start-up catch-up pass seals, so wait a bounded moment for it rather
        // than hold that whole pass until the next tick: only a row that truly
        // cannot be read holds the write.
        let mut state = control.connection_state();
        let connected = tokio::time::timeout(SEAL_FLOOR_CONNECT_WAIT, async {
            loop {
                if *state.borrow_and_update() == fauna_client::ConnectionState::Connected {
                    return true;
                }
                if state.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false);
        let read = if connected {
            fauna_client_folders::FoldersClient::new(control)
                .list_owned_and_shared_wire()
                .await
                .ok()
        } else {
            None
        };
        match read {
            Some(reply) => {
                let bases: Vec<(i64, crate::binding_edge::BindingBasis)> = reply
                    .folders
                    .iter()
                    .map(|fs| (fs.id, crate::binding_edge::BindingBasis::of(fs)))
                    .collect();
                self.apply_binding_read(&bases);
            }
            None => {
                let mut floor = self.seal_floor.write().unwrap();
                *floor = floor.after_failed_read();
            }
        }
    }

    /// Apply one successful folder-list read to the pre-seal hold and the
    /// refresh edge: re-install the floor from the set's row (or hold every seal
    /// when the row is gone), and ask the host to re-resolve when the row moved
    /// since the last read, when it is gone, or — at every read, until the
    /// generation arrives — while its floor is ahead of the generation held
    /// (custody arriving is not visible on the row; decision 2). No edge
    /// installed ⇒ nothing to compare: the posture was armed at build and stays.
    pub(crate) fn apply_binding_read(&self, bases: &[(i64, crate::binding_edge::BindingBasis)]) {
        let Some(edge) = self.binding_edge.as_ref() else {
            return;
        };
        let fauna_core::folder_keys::FolderRef::Local(id) = edge.folder_ref else {
            // A cross-nest set has no row on this nest; its edges are the host's.
            return;
        };
        let now = bases.iter().find(|(row, _)| *row == id).map(|(_, b)| b);
        *self.seal_floor.write().unwrap() = match now {
            Some(basis) => crate::binding_edge::SealFloor::Floor(basis.content_key_floor()),
            None => crate::binding_edge::SealFloor::RowGone,
        };
        // Decision 2′ (d): an own-pending row sealed below the floor just read
        // is behind, so it leaves the peer door. Its path is still
        // `LocallyModified`; the re-seal under the new generation re-mints it.
        if let Some(floor) = now.and_then(crate::binding_edge::BindingBasis::content_key_floor)
            && let Err(e) = self.db.retire_own_pending_below(floor)
        {
            tracing::warn!(error = %e, floor, "withdrawing own-pending rows below the floor failed");
        }
        let held = self.held_content_key_version();
        let rebuild = match now {
            None => true,
            Some(now) => {
                let moved = {
                    let mut seen = self.seen_basis.write().unwrap();
                    let moved = seen.as_ref() != Some(now);
                    *seen = Some(now.clone());
                    moved
                };
                moved || crate::binding_edge::floor_ahead(held, now.content_key_floor())
            }
        };
        if rebuild {
            (edge.on_rebuild)();
        }
    }

    /// Whether this seat skips chunk-byte uploads (phase 5 metadata-only
    /// residency — the folder's bytes deliberately never rest on the nest).
    pub fn is_metadata_only_residency(&self) -> bool {
        self.metadata_only_residency
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The live `public`-audience write-arm flag (see the field's doc). One
    /// accessor so every seal-decision site reads the same ordering.
    pub fn is_public_audience(&self) -> bool {
        self.public_audience
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Log the two edges of an **unanchored public claim** — the nest row
    /// naming this folder `public` on a seat that holds no trusted owner to
    /// verify the owner's attestation against, so it stays sealed
    /// (`encryption-at-rest.md` § Implementation status today: a member seat on
    /// a host with no MLS state). One line when the claim appears, one when it
    /// is withdrawn, nothing on the ticks between; `None` (the list was
    /// unreadable) read nothing and logs nothing.
    ///
    /// Changes no behaviour. It exists because this verdict is otherwise
    /// silent: a sealed seat that stays sealed records nothing, so without the
    /// line nothing distinguishes "read the public window and judged it" from
    /// "has not ticked yet" — for an admin reading the log, and for the
    /// real-seat test that asserts the seat stayed sealed.
    fn note_unanchored_public_claim(&self, read: Option<bool>) {
        let Some(now) = read else {
            return;
        };
        let was = self
            .unanchored_public_claim
            .swap(now, std::sync::atomic::Ordering::Relaxed);
        if was == now {
            return;
        }
        // A row was read, so this engine has a folder; the empty label is
        // unreachable and only keeps the line total.
        let folder = self.folder().unwrap_or_default();
        if now {
            tracing::info!(
                folder = %fauna_core::log_redact::log_folder_name(folder),
                "folder audience: the nest row claims public but this seat holds no trusted owner for it; staying sealed"
            );
        } else {
            tracing::info!(
                folder = %fauna_core::log_redact::log_folder_name(folder),
                "folder audience: the unanchored public claim was withdrawn; this seat stayed sealed throughout"
            );
        }
    }

    /// Report [`Self::is_public_audience`] outward as a
    /// [`crate::progress::ProgressEvent::PublicAudience`] — called wherever the
    /// arm is (re)read: the end of [`Self::refresh_sync_mode`] and an on-demand
    /// root's populate.
    pub fn report_public_audience(&self) {
        self.emit_progress(crate::progress::ProgressEvent::PublicAudience {
            armed: self.is_public_audience(),
        });
    }

    /// Arm (or disarm) the website toggle at build — the
    /// [`Self::with_public_audience`] sibling, seeded from the same folder-list
    /// read (`SeatResolution::website_enabled`). Production seats install it
    /// live through [`Self::refresh_sync_mode`]; this exists for hosts and
    /// tests that drive [`Self::converge_corpus_to_website`] directly.
    pub fn with_website_enabled(self, website_enabled: bool) -> Self {
        self.website_enabled
            .store(website_enabled, std::sync::atomic::Ordering::Relaxed);
        self
    }

    /// Whether this folder's website toggle is on (see the field's doc). Read
    /// by [`Self::converge_corpus_to_website`] alone — no write-path decision
    /// keys on it.
    pub fn is_website_enabled(&self) -> bool {
        self.website_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Raw install of the accepts gate (see the `accepts_remote` field's doc)
    /// — the [`Self::set_sync_mode`] shape: production seats resolve through
    /// [`Self::refresh_sync_mode`]; this is the underneath, kept public for
    /// hosts that install postures directly and tests.
    pub fn set_accepts_remote(&self, accepts: bool) {
        self.accepts_remote
            .store(accepts, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether this seat's place accepts remote changes (see the field's doc).
    /// One accessor so every delivery rail reads the same ordering.
    pub fn accepts_remote_changes(&self) -> bool {
        self.accepts_remote
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// This engine's exclusive-editing posture — the cell every lease question
    /// is answered from (`file-sync.md` § Exclusive editing).
    ///
    /// Public so a host can render what it finds without the engine growing a
    /// pass-through per fact, and so the raw installs stay reachable for
    /// hosts that install postures directly and tests, exactly as
    /// [`Self::set_accepts_remote`] is.
    pub fn lease_posture(&self) -> &crate::folder_lease::LeasePosture {
        &self.lease
    }

    /// Whether this seat must treat its folder as **read-only** because another
    /// device holds the exclusive-edit lease — the sentence `file-sync.md`
    /// § Folders promises (*"while a lease is held, other devices treat the
    /// folder as read-only"*).
    ///
    /// Always `false` for an un-governed folder, which is every folder until an
    /// owner turns exclusive editing on. An **offline** seat is never read-only
    /// by this reading: being unable to reach the nest is not being locked out,
    /// and the projection's last reading lapses on the lease's own TTL rather
    /// than hardening into a freeze.
    pub fn is_read_only_for_lease(&self) -> bool {
        self.lease
            .is_read_only(self.device_id_hex(), crate::folder_lease::now_secs())
    }

    /// Open this pass's **write window** on a lease-governed folder: acquire the
    /// lease if this device does not already hold one with headroom, and answer
    /// what the pass may do (`file-sync.md` § Exclusive editing).
    ///
    /// Called **once per upload pass, never per file** — that is the whole rule
    /// the lease design turns on. An un-governed folder (every folder by
    /// default) returns [`LeaseWindow::NotGoverned`] without touching the nest,
    /// so this costs the ordinary case exactly nothing; a governed folder pays
    /// one `fauna.folders.lease.acquire` per pass, and a re-acquire by the same
    /// device IS the renewal, so a long pass costs one more kind per half-TTL
    /// and no new state anywhere.
    ///
    /// **Both refusal arms defer, neither drops.** The pass that gets one
    /// uploads nothing and leaves every entry `LocallyModified` on disk, which
    /// is precisely where the offline arm leaves it and precisely what the next
    /// converge pass re-drives. The iron rule that a tracked file's local
    /// modification MUST be uploaded is deferred by a lease, never breached by
    /// one.
    pub async fn open_lease_window(&self) -> crate::folder_lease::LeaseWindow {
        use crate::folder_lease::{LeaseHold, LeaseWindow, now_secs};

        if !self.lease.is_governed() {
            return LeaseWindow::NotGoverned;
        }
        // A set-less or cross-nest-foreign engine has no row in this actor's
        // own folder projection, so nothing could have marked it governed and
        // nothing can acquire against it. Belt-and-braces with the flag above,
        // which is exactly why it is cheap.
        let Some(folder) = self.folder().map(str::to_string) else {
            return LeaseWindow::NotGoverned;
        };

        let now = now_secs();
        let mut hold = self.lease.lock_hold().await;
        if let Some(existing) = *hold
            && existing.has_headroom_at(now)
        {
            return LeaseWindow::Held;
        }

        // The acquire needs a connected control plane by construction — the
        // kind is classed online-only. Skipping the RPC when the plane is down
        // is not merely an optimization: letting it wait out its full deadline
        // would stall an offline seat's every pass for nothing, and the answer
        // is the same either way.
        let control = self.control_plane();
        if !matches!(
            *control.connection_state().borrow(),
            fauna_client::ConnectionState::Connected
        ) {
            *hold = None;
            self.lease.mark_unheld();
            tracing::debug!(
                folder = %fauna_core::log_redact::log_folder_name(&folder),
                "exclusive editing: control plane not connected; this pass defers its uploads \
                 (the edits stay on disk and upload on the reconnect that can acquire)"
            );
            return LeaseWindow::Unavailable;
        }

        let client = fauna_client_folders::FoldersClient::new(std::sync::Arc::clone(&control));
        let req = fauna_protocol::folders::LeaseAcquireRequest {
            name: folder.clone(),
            device_id: self.device_id_hex().to_string(),
            ..Default::default()
        };
        match client.lease_acquire(req).await {
            Ok(reply) if reply.acquired => {
                *hold = Some(LeaseHold { acquired_at: now });
                self.lease.mark_held(now);
                self.lease.clear_refused();
                tracing::debug!(
                    folder = %fauna_core::log_redact::log_folder_name(&folder),
                    "exclusive editing: lease acquired for this upload pass"
                );
                LeaseWindow::Held
            }
            // `acquired: false` is not a shape today's nest produces — it
            // answers the typed `conflict` below instead — but the field is on
            // the wire and an older or future nest may use it, so reading it as
            // anything but "somebody else has it" would write straight through
            // a lease (I2: any client × any nest within a major).
            Ok(_) => {
                *hold = None;
                self.lease.mark_unheld();
                self.lease.mark_refused(now);
                tracing::info!(
                    folder = %fauna_core::log_redact::log_folder_name(&folder),
                    "exclusive editing: another device holds this folder's lease; \
                     deferring this pass's uploads (nothing local is lost)"
                );
                LeaseWindow::Refused
            }
            Err(e) if crate::folder_lease::is_lease_conflict(&e) => {
                *hold = None;
                self.lease.mark_unheld();
                self.lease.mark_refused(now);
                tracing::info!(
                    folder = %fauna_core::log_redact::log_folder_name(&folder),
                    "exclusive editing: another device holds this folder's lease; \
                     deferring this pass's uploads (nothing local is lost)"
                );
                LeaseWindow::Refused
            }
            Err(e) => {
                // NOT a refusal: the nest could not be asked. Deferring is the
                // same action, but the state is different and must stay
                // different — an offline or erroring seat is not a locked-out
                // one, and rendering it read-only would tell the user their own
                // folder belongs to someone else.
                *hold = None;
                self.lease.mark_unheld();
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&folder),
                    error = %e,
                    "exclusive editing: could not ask for this folder's lease; \
                     deferring this pass's uploads"
                );
                LeaseWindow::Unavailable
            }
        }
    }

    /// Renew this pass's window if the hold has run past half the lease TTL —
    /// the *"renews while the flush runs"* half of the rule.
    ///
    /// Safe and cheap to call before every file in a pass: it is a lock and a
    /// comparison, and it reaches the nest only once per half-TTL. Answers
    /// whether the pass may still write, so a renewal that was refused (the
    /// hold lapsed and another device took over mid-pass) stops the pass
    /// instead of letting it write through the new holder's lease.
    pub async fn renew_lease_if_due(&self) -> bool {
        if !self.lease.is_governed() {
            return true;
        }
        self.open_lease_window().await.may_write()
    }

    /// Close this pass's write window: release the lease this device holds, if
    /// it holds one.
    ///
    /// Idempotent and never fatal. A release that fails to reach the nest costs
    /// the next writer a wait of at most the TTL — the nest expires the row on
    /// its own — so failing loudly here would turn a self-healing hiccup into a
    /// pass failure. The release is **holder-scoped** (this device's id), so it
    /// can only ever drop this device's own lease: a writer member must never
    /// be able to free another actor's active one.
    pub async fn close_lease_window(&self) {
        let mut hold = self.lease.lock_hold().await;
        let held = hold.take().is_some();
        self.lease.mark_unheld();
        if !held {
            return;
        }
        let Some(folder) = self.folder().map(str::to_string) else {
            return;
        };
        let control = self.control_plane();
        let client = fauna_client_folders::FoldersClient::new(std::sync::Arc::clone(&control));
        if let Err(e) = client
            .lease_release(folder.clone(), self.device_id_hex().to_string())
            .await
        {
            tracing::debug!(
                folder = %fauna_core::log_redact::log_folder_name(&folder),
                error = %e,
                "exclusive editing: releasing this folder's lease failed; it lapses on the \
                 nest's own TTL, so the next writer waits at most that long"
            );
        }
    }

    /// Offer the retired owner keys of the identities this account succeeded
    /// from as **read** candidates for every chunk and manifest this engine
    /// opens (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus
    /// row).
    ///
    /// A setter rather than a [`Self::new`] parameter for the reason the field's
    /// own doc records. Idempotent and additive: an engine never told about a
    /// predecessor behaves exactly as it did before this existed, which is what
    /// every engine that is not a successor's does.
    ///
    /// ⚠ **Read-only.** The value reaches [`Self::download_keys`] and nothing
    /// else; `content_seal_root` reads `backup_key` alone, so no upload this
    /// engine performs can land under a retired root — including the re-seal
    /// pass, whose entire purpose is to move bytes off these keys.
    pub fn set_predecessor_backup_keys(
        &mut self,
        keys: Vec<fauna_core::file_download::PredecessorSealKey>,
    ) {
        self.predecessor_backup_keys = keys;
    }

    /// Offer the retired M2 generations of a set this engine's binding no
    /// longer claims — [`Self::predecessor_backup_keys`]'s WebDAV serve-toggle
    /// twin (`webdav-server.md` § Key model, Revocation).
    ///
    /// ⚠ **Read-only**, exactly like the predecessor keys: the value reaches
    /// [`Self::download_keys`] and nothing else. `content_seal_root`/
    /// `effective_backup_key` read [`Self::content_keys`] alone, so no upload
    /// this engine performs can land under a retired generation — including
    /// the re-seal pass ([`Self::reseal_predecessor_sealed`]), whose entire
    /// purpose is to move bytes off it and onto the owner root.
    pub fn set_retired_content_keys(
        &mut self,
        keys: Option<fauna_core::folder_keys::FolderContentKeys>,
    ) {
        self.retired_content_keys = keys;
    }

    /// Arm rule (5)'s re-seal hold ([`Self::served_era_hold`]).
    pub fn set_served_era_hold(&mut self, hold: bool) {
        self.served_era_hold = hold;
    }

    /// The hex actor id this engine's owner root belongs to — the identity the
    /// `current_root_sealed` sentinels are stamped under
    /// ([`crate::succession_drain`]).
    ///
    /// Read from the nest client rather than stored, so it cannot drift from the
    /// account the engine is actually serving.
    pub fn owner_actor_id_hex(&self) -> String {
        self.nest_client.actor_id_hex()
    }

    /// Record what a completed pull means for this engine's two "we are caught
    /// up" stamps — the freshness heartbeat and the succession fold evidence.
    ///
    /// **One function for both, taking the deferred flag as its argument**, so
    /// the two can never disagree about what a pass proved and neither call site
    /// carries a condition of its own. A `deferred` batch stamps **nothing**: it
    /// did not drain the feed, so it says nothing about consistency — and for
    /// the fold evidence in particular, a change the engine could not resolve
    /// may be exactly a predecessor-sealed label, i.e. the very evidence that
    /// the retired keys are still needed (`crate::succession_drain`, ruling 3).
    ///
    /// Best-effort on both counts: the stamps are device-local, and losing one
    /// costs another pass before the retired keys may be dropped — the
    /// fail-closed direction, which is the only safe one here.
    pub(crate) fn note_pull_outcome(&self, deferred: bool) {
        if deferred {
            return;
        }
        if let Err(e) = self.db.mark_clean_pass_if_drained() {
            tracing::warn!(error = ?e, "failed to stamp last_clean_pass_at");
        }
        if let Err(e) = self.db.mark_succession_fold_evidence() {
            tracing::warn!(error = ?e, "failed to stamp succession fold evidence");
        }
    }

    /// **The root-generation guard** (`sync-agent.md` § Credential model →
    /// *Bound (3)'s enforcement design*, ruling 5). Bind this engine's
    /// `current_root_sealed` sentinels to the identity now serving them,
    /// clearing them (and the fold evidence) when that identity changed.
    ///
    /// Drive it before either re-seal pass reads a sentinel — a second
    /// succession otherwise leaves the previous successor's entries stamped
    /// "sealed under the current root" while their bytes rest under the root
    /// that identity retired, so the pass skips exactly the corpus that needs it
    /// and the drain observable becomes vacuously true.
    pub fn adopt_sentinel_root_generation(&self) {
        let actor = self.owner_actor_id_hex();
        match self.db.adopt_sentinel_root_actor(&actor) {
            Ok(true) => tracing::info!(
                actor = %actor,
                "owner root generation changed — cleared current_root_sealed sentinels \
                 and fold evidence; the corpus will be re-examined"
            ),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = ?e, "failed to adopt sentinel root generation"),
        }
    }

    /// Replace the WS-RPC control plane ([`crate::nest_api::SyncControlApi`]).
    ///
    /// Test-only by construction — production builds the real
    /// [`crate::nest_api::WsRpcSyncControl`] in [`Self::new`] and never calls
    /// this. Its reason to exist is that `ResolvedApply::KeepLocal` is otherwise
    /// unreachable in-crate (see the field's own doc).
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_control_api(&self, control: Arc<dyn crate::nest_api::SyncControlApi>) {
        *self.control.write().unwrap() = control;
    }

    /// Share this engine's terminal park state with the host that built it
    /// (D4). The cross-nest byte plane needs it: its `WriteTokenBearer` is
    /// constructed *before* the engine (it is the engine's own `SyncClient`'s
    /// bearer), so the host makes one gate, hands a clone to the bearer, and
    /// installs it here — after which a mint refusal and a record refusal park
    /// the same set. A host that only wants to *observe* the park (to persist it
    /// and drop the set from its running plan) can install a gate it kept a
    /// handle on, or read [`Self::access_gate`].
    pub fn set_access_gate(&mut self, gate: Arc<crate::access_gate::AccessGate>) {
        self.access_gate = gate;
    }

    /// This engine's park state — `true` once the authoritative nest has refused
    /// its write grant. Hosts surface it (agent `EngineInfo` → the client's
    /// folder row) so a revoked binding is *visibly* stopped rather than
    /// silently idle.
    pub fn access_gate(&self) -> &Arc<crate::access_gate::AccessGate> {
        &self.access_gate
    }

    /// Bind this engine to a set homed on **another** nest:
    /// `home_nest_url` is the set's home nest, `channel_id_hex` its
    /// derived `ChannelId`. Both control-plane kinds — `changes.record` (write)
    /// and `changes.list` (change-log read) — then relay through this actor's own
    /// nest to the home nest (`nest_url`-carrying); the caller must also point
    /// the byte-plane [`SyncClient`] at `home_nest_url` with a write-token
    /// `BearerSource` (`fauna.folders.write_token.get`). `set_tunnel_url` seam
    /// pattern — `&self`, so it composes with an `Arc`-held engine.
    pub fn set_foreign_routing(&self, home_nest_url: String, channel_id_hex: String) {
        *self.foreign_routing.write().unwrap() = Some((home_nest_url, channel_id_hex));
    }

    /// Install this set's sync-mode resolution directly. Production hosts
    /// resolve through [`Self::refresh_sync_mode`] (the resident loop calls it
    /// at entry and per tick); this setter is the raw install underneath it,
    /// kept public for hosts that install it directly and tests. Accepts a bare
    /// [`crate::config::SyncMode`] via `Into` (it installs as `Resolved`).
    ///
    /// ⚠ **Must be installed before the first pull.** After it, a `backup`-mode
    /// or unresolved host stops applying peers' deletes
    /// ([`crate::config::ModeResolution::applies_remote_deletes`]); before it,
    /// the constructor's value governs. `always_resident::run_watch_loop`
    /// refreshes at entry, above its eager first pull.
    pub fn set_sync_mode(&self, mode: impl Into<crate::config::ModeResolution>) {
        *self.mode.write().unwrap() = mode.into();
    }

    /// This engine's current sync-mode resolution (see the `mode` field).
    pub fn sync_mode_resolution(&self) -> crate::config::ModeResolution {
        *self.mode.read().unwrap()
    }

    /// Whether this engine applies a peer's delete to local disk — the
    /// no-user-data-loss guard's read side (`file-sync.md` § 4). `false` for a
    /// resolved backup seat and for an **unresolved** one.
    pub fn applies_remote_deletes(&self) -> bool {
        self.mode.read().unwrap().applies_remote_deletes()
    }

    /// Re-resolve this seat's sync mode from the authoritative nest rows and
    /// install the answer — **the one production resolver** every in-process
    /// host drives (`file-sync.md` § 4: the resident loop at entry + per rescan
    /// tick, the one-shot pass at entry). Replaces the retired once-per-process
    /// `HydrationHost::sync_mode` install, whose once-ness was leg 1:
    /// a role the user changed never reached a running engine.
    ///
    /// Failure posture (the leg-2 fix, direction ratified 2026-08-02): a failed
    /// read falls back to the **persisted last authoritative answer**
    /// (`SyncDb::get_cached_sync_mode`), so a transient `members.list` failure
    /// keeps the last thing the nest actually said instead of silently
    /// disarming a backup seat for the process lifetime; a seat that never had
    /// an authoritative answer resolves `Unresolved` — decline deletes, hold
    /// the anchor ([`crate::config::ModeResolution`]). The resolution logic
    /// itself is the pure [`crate::config::resolve_device_mode`], so no two
    /// readers can disagree about one seat.
    pub async fn refresh_sync_mode(&self) {
        let res = self.resolve_sync_mode().await;
        // Decision 2's refresh edge rides the same read: the floor re-installs
        // and a moved row asks the host to re-resolve. `None` (list unreadable,
        // or never read) keeps the armed posture — the same discipline as every
        // install below.
        if let Some(bases) = res.folder_bases.as_deref() {
            self.apply_binding_read(bases);
        }
        let new = res.mode;
        let old = *self.mode.read().unwrap();
        if new != old {
            tracing::info!(?old, ?new, "sync mode (re)resolved from the nest rows");
        }
        *self.mode.write().unwrap() = new;
        // Phase 4: the audience rides the same read (config::SeatResolution) —
        // install it live so a flip reaches this RUNNING seat's write path
        // within one tick. `None` (list unreadable) keeps the armed posture,
        // exactly the mode's failure discipline.
        install_authoritative_posture(
            &self.public_audience,
            res.public_audience,
            "folder audience",
        );
        self.report_public_audience();
        self.note_unanchored_public_claim(res.unanchored_public_claim);
        // Phase 5: the residency posture rides the same read — `None` keeps
        // the armed posture, the same failure discipline throughout.
        self.install_residency(res.metadata_only_residency);
        // Phase 2 slice c: the accepts gate rides the same read. `None`
        // (unreadable seat / failed reads) keeps the armed posture — the same
        // failure discipline as the two installs above.
        install_authoritative_posture(
            &self.accepts_remote,
            res.accepts,
            "place accepts-remote-changes",
        );
        // The website toggle rides the same read — the signal
        // `converge_corpus_to_website` observes turning ON. `None` keeps the
        // armed posture, the same failure discipline throughout.
        install_authoritative_posture(&self.website_enabled, res.website_enabled, "website toggle");
        // Selective sync rides the same read — the folder row's
        // `include_paths`/`exclude_paths`, rendered sealed-first upstream. This
        // is the ONLY place either live desktop deployment learns them: both
        // engine builders are sync and hold no control plane, so a
        // construction-time matcher could never carry the row
        // (`file-sync.md` § Config).
        install_selective_sync(&self.ignore, &res.selective_sync);
        // Exclusive editing rides the same read (`file-sync.md` § Exclusive
        // editing). Both halves install together because they were read
        // together: the flag says whether the folder is governed at all, the
        // holder says who may write it right now, and a seat that composed them
        // from two different moments could read "governed" from one list and
        // "unheld" from a later one and write straight through a lease taken in
        // between. `None` on either keeps the armed value — the same failure
        // discipline as every install above.
        self.lease.install_from_read(
            res.exclusive_editing.governed,
            res.exclusive_editing.lease,
            self.device_id_hex(),
            crate::folder_lease::now_secs(),
        );
        // The reader's writer roster rides the same cadence once a batch
        // needed it (the first read is on demand, before the batch that meets
        // a non-owner's row), so a demotion reaches the reader within a tick.
        let roster_read = self
            .row_reader
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .roster_read();
        if roster_read {
            self.refresh_reader_roster().await;
        }
    }

    /// Refresh the share leg's cached WRITER roster from the nest's actor
    /// roster (B2 — `p2p-shared-set-build.md` § *Build design — the row half*): the
    /// fail-closed consult peer-row ingest reads (`SyncDb::cached_share_writer`)
    /// is only as fresh as the last successful read this performs. Rides the
    /// same cadence as [`Self::refresh_sync_mode`] (lifecycle build + resident
    /// tick). Skips — keeping the stale cache, the ruling's posture — when
    /// the engine has no set, is a cross-nest foreign member (v1 carve-out,
    /// same as `resolve_sync_mode`'s — it may lift on the relayed roster read
    /// [`Self::refresh_reader_roster`] now uses, once p2p cross-nest is
    /// scoped), or the control plane is not currently connected (an offline
    /// tick must not wait out RPC deadlines).
    #[cfg(feature = "p2p-share")]
    pub async fn refresh_share_writer_roster(&self) {
        // Each skip below leaves the cache exactly as it was — and an EMPTY
        // cache refuses every row every peer serves, forever and silently
        // (`judge_peer_row`'s fail-closed arm). A skip is therefore not a
        // no-op but a decision with a lasting consequence, and it must be
        // legible: three silent `return`s are indistinguishable from a
        // refresh that ran and found nothing.
        //
        // All four lines of the roster's story (these three skips and the read
        // itself) carry the `peer_share_store` target — the module that OWNS
        // the cached writer roster — rather than this one's, which is the whole
        // engine and far too broad to raise to debug. One target turns the
        // roster on and off as a single subject, which is what let the first
        // attempt at these lines land invisibly: the e2e journey scopes its
        // share-plane debug to named targets, and this module was not among
        // them.
        let Some(folder) = self.folder() else {
            tracing::debug!(
                target: "fauna_sync_engine::peer_share_store",
                "share writer roster: no folder on this engine; cache untouched"
            );
            return;
        };
        if self.foreign_routing().is_some() {
            tracing::debug!(
                target: "fauna_sync_engine::peer_share_store",
                folder = %fauna_core::log_redact::log_folder_name(folder),
                "share writer roster: cross-nest foreign member (v1 carve-out); cache untouched"
            );
            return;
        }
        let control = self.control_plane();
        if !matches!(
            *control.connection_state().borrow(),
            fauna_client::ConnectionState::Connected
        ) {
            tracing::debug!(
                target: "fauna_sync_engine::peer_share_store",
                folder = %fauna_core::log_redact::log_folder_name(folder),
                "share writer roster: control plane not connected; cache untouched"
            );
            return;
        }
        let folder = folder.to_string();
        crate::peer_share_store::refresh_writer_roster_via(&self.db, &folder, control).await;
    }

    /// The read half of [`Self::refresh_sync_mode`] — resolve without
    /// installing. Public so a host that wants the answer before deciding to
    /// install (tests, diagnostics) can take it. Delegates the reads + cache
    /// protocol to the shared [`crate::config::resolve_device_mode_from_nest`]
    /// — one I/O implementation, so no two production readers can compose the
    /// reads differently.
    pub async fn resolve_sync_mode(&self) -> crate::config::SeatResolution {
        use crate::config::{ModeResolution, SeatResolution, SelectiveSyncResolution, SyncMode};

        // Cross-nest foreign carve-out (unchanged from the pre-refresh design):
        // a foreign set has no row in this member's own projection and the
        // pushed engine-key blob carries no mode — keep the delete-applying
        // default, exactly as before; the audience likewise keeps its armed
        // posture (a foreign public-folder write arm is a later federation
        // leg — `FolderEngineKeys::public_audience`). A set-less engine
        // likewise.
        if self.foreign_routing().is_some() {
            return SeatResolution {
                mode: ModeResolution::Resolved(SyncMode::Sync),
                public_audience: None,
                unanchored_public_claim: None,
                accepts: None,
                metadata_only_residency: None,
                website_enabled: None,
                // No row to read: a foreign set's projection carries none, and
                // a set-less engine has no row at all. Both keep whatever
                // filter the builder installed, the postures' `None` discipline.
                selective_sync: SelectiveSyncResolution::default(),
                // Likewise for exclusive editing: no row means no governance
                // answer and no holder reading, so the armed posture stands.
                // A folder nobody could read a flag for is un-governed by the
                // flag's own fail-open rule, which is what the builder's
                // default already is.
                exclusive_editing: crate::config::ExclusiveEditingResolution::default(),
                folder_bases: None,
            };
        }
        let Some(folder) = self.folder() else {
            return SeatResolution {
                mode: ModeResolution::Resolved(SyncMode::Sync),
                public_audience: None,
                unanchored_public_claim: None,
                accepts: None,
                metadata_only_residency: None,
                website_enabled: None,
                // No row to read: a foreign set's projection carries none, and
                // a set-less engine has no row at all. Both keep whatever
                // filter the builder installed, the postures' `None` discipline.
                selective_sync: SelectiveSyncResolution::default(),
                // Likewise for exclusive editing: no row means no governance
                // answer and no holder reading, so the armed posture stands.
                // A folder nobody could read a flag for is un-governed by the
                // flag's own fail-open rule, which is what the builder's
                // default already is.
                exclusive_editing: crate::config::ExclusiveEditingResolution::default(),
                folder_bases: None,
            };
        };
        let folder = folder.to_string();
        // A control plane that is not currently connected fails the reads by
        // definition — skip them rather than let each RPC wait out its full
        // deadline (a never-connected host would stall the loop entry ~30 s
        // per read). The persisted answer still governs, exactly as for a
        // failed read; the next tick re-resolves once the supervisor
        // reconnects.
        let control = self.control_plane();
        let nest = matches!(
            *control.connection_state().borrow(),
            fauna_client::ConnectionState::Connected
        )
        .then_some(&control);
        // This seat's anchor for the owner-attested declassification verdict
        // (`encryption-at-rest.md` § Readable classes → *The declassification
        // is owner-ATTESTED*): its own actor id for a folder its account owns;
        // for a member seat, the channel's MLS-recorded owner where this engine
        // holds MLS state — and nothing, which seals, on a bearer-only engine
        // (the sync agent, the File Provider helper).
        let mls_owners = self.mls.as_deref().map(crate::config::MlsChannelOwners);
        let anchor = fauna_client_folders::DeclassificationAnchor {
            own: self.own_actor_id(),
            channel_owners: mls_owners
                .as_ref()
                .map(|o| o as &dyn fauna_client_folders::FolderChannelOwners),
        };
        // The row is this binding's by REF (`on-demand-files.md` § Hosting
        // multiple on-demand folders): a member seat on a shared `docs` whose
        // user also owns a `docs` must read the shared row, never the own one
        // the list carries first. An engine built without a binding edge
        // holds no ref, and reads no row — the sealed direction.
        let key = self
            .binding_edge
            .as_ref()
            .map_or(crate::binding_edge::SeatRowKey::Unbound, |edge| {
                crate::binding_edge::SeatRowKey::Ref(edge.folder_ref)
            });
        crate::config::resolve_device_mode_from_nest(
            nest,
            key,
            &folder,
            self.device_id_hex(),
            &self.db,
            // The reader half of the sealed selective-sync lists — the owner
            // root plus the predecessor read candidates, the same assembly
            // every other sealed read on this engine uses.
            &self.download_keys(),
            &anchor,
        )
        .await
    }

    /// This engine's cross-nest routing pair, or `None` for an own-nest set —
    /// i.e. whether its control plane relays. Public because "which nest does
    /// this engine answer to" is a question its host has to be able to ask: the
    /// agent stamps it into the engine identity (a set that becomes foreign must
    /// rebuild, not keep talking to the old nest), and the revocation surface
    /// needs it to tell a foreign mint refusal from a same-nest one.
    pub fn foreign_routing(&self) -> Option<(String, String)> {
        self.foreign_routing.read().unwrap().clone()
    }

    /// The nest this engine's **byte plane** (chunk + manifest transfers) targets.
    /// The set's home nest for a cross-nest set, the caller's own nest otherwise —
    /// bytes never ride the control-plane relay, so this diverges from the nest
    /// the control plane talks to exactly when [`Self::foreign_routing`] is `Some`.
    pub fn byte_plane_nest_url(&self) -> String {
        self.client.auth().nest_url()
    }

    /// The 32-byte `chunk_crypto` root a **fresh** upload seals each chunk under,
    /// plus the M2 **generation version** to stamp into the snapshot's change
    /// record (`None` when there is no generation to stamp).
    ///
    /// - **Bound (cross-user shared) set with content keys** — `mls_group_id =
    ///   Some`, `content_keys = Some`: returns `(current_key, Some(current_version))`
    ///   ([`FolderContentKeys::current_key`] / `current_version`). Every member
    ///   holding that generation derives the identical per-chunk keys, and the
    ///   stamped version lets a reader pick the right generation back out
    ///   ([`Self::content_open_roots`]). This is the M2 content key — **not** the
    ///   raw epoch secret (which rotates on every commit); it rotates only on a
    ///   member removal (`mls-group-key-material.md` § M2 content-key mechanism).
    /// - **Owner-only set** — `mls_group_id = None`: `(epoch_secret, None)` (no
    ///   generation; today `epoch_secret` is always `None` in production — those
    ///   sets seal under `backup_key`, which the call sites prefer over this).
    ///   `None` ⇒ no `chunk_crypto` seal (plaintext / the `backup_key` branch).
    ///
    /// **FS-BIND-5 — fail closed, keyed on the
    /// `mls_group_id` bound-marker.** A **bound** set (`mls_group_id = Some`) whose
    /// content keys are not in hand (`content_keys = None` — a removed member with
    /// stale config, or a startup race before the rotate-on-removal orchestration
    /// loaded them) returns **`Err`**, never `Ok(None)`: the seal call sites
    /// propagate it and refuse to store anything rather than fall through to the
    /// plaintext branch and upload **unencrypted** chunks of a shared, cross-user,
    /// private folder to the **untrusted** nest. (Leaked plaintext is
    /// irrecoverable; an unkeyed seal is a re-uploadable availability fault — so
    /// confidentiality wins.) Only an **unbound** set yields `Ok(None)`.
    fn content_seal_root(&self) -> anyhow::Result<Option<([u8; 32], Option<u64>)>> {
        if let Some(content_keys) = self.content_keys.as_ref() {
            // Decision 2's pre-seal hold: a floor ahead of the generation held
            // means the owner rotated past it (a member removal above all), so
            // sealing now would hand the removed member what comes next. Held,
            // never sealed under the older generation; the caller leaves the
            // write un-acked and it is sealed once an edge brings the generation.
            // A floor the last read could not refresh holds only here, on what
            // it last said (decision 2′ (a)); sending is `publish_hold`'s.
            if let Some(hold) = self.seal_hold() {
                anyhow::bail!("content_seal_root: {hold}");
            }
            // Bound shared set with its M2 generation history loaded: seal under
            // the current generation and stamp its version.
            return Ok(Some((
                *content_keys.current_key(),
                Some(content_keys.current_version()),
            )));
        }
        if self.mls_group_id.is_some() {
            // Bound but no content keys → FAIL CLOSED; never degrade to plaintext.
            anyhow::bail!(
                "content_seal_root: engine is bound to a shared folder (mls_group_id) but has \
                 no M2 content keys loaded (removed member / startup race?) — refusing to seal a \
                 shared folder in plaintext"
            );
        }
        // Unbound owner-only set — the configured epoch_secret (None in
        // production; those sets seal under backup_key at the call sites). No
        // generation to stamp.
        Ok(self.epoch_secret.map(|secret| (secret, None)))
    }

    // The chunk-**open** root selection (`content_open_roots`) moved to
    // `fauna_core::file_download::FileDownloadKeys` on 2026-07-16, with the walk
    // that is its only consumer — see [`Self::download_keys`]. The seal-side
    // roots below stay here: they serve the upload/drain paths, which are
    // native-only (they queue through the `SyncDb`).

    /// The candidate `chunk_crypto` roots the **drain/resume worker** re-seals
    /// under to reproduce the store keys queued uploads were enqueued by —
    /// one per retained content-key generation, current-first
    /// ([`FolderContentKeys::generations`]).
    ///
    /// A `transfer_queue` entry is keyed by the ciphertext store key under the
    /// generation **current at seal time**, and a rotate-on-removal can land
    /// between the enqueue and the drain (the owner's own removal — the
    /// sole-writer model never bounded this). Re-sealing only under the new
    /// current generation would produce different ciphertext, every queued
    /// lookup would miss, and the drain's stale-entry cleanup would silently
    /// drop the never-uploaded chunks (the failure mode
    /// via generation mismatch) — so the drain map must hold a candidate per
    /// retained generation; store-key equality disambiguates. Fail-closed
    /// posture identical to [`Self::content_seal_root`] (FS-BIND-5): a bound
    /// engine with no content keys refuses to drain.
    fn content_drain_roots(&self) -> anyhow::Result<Option<Vec<[u8; 32]>>> {
        if let Some(content_keys) = self.content_keys.as_ref() {
            return Ok(Some(content_keys.generations().map(|g| *g.key).collect()));
        }
        if self.mls_group_id.is_some() {
            anyhow::bail!(
                "content_drain_roots: engine is bound to a shared folder (mls_group_id) but \
                 has no M2 content keys loaded (removed member / startup race?) — refusing to \
                 re-seal a shared folder's drained chunks in plaintext"
            );
        }
        Ok(self.epoch_secret.map(|secret| vec![secret]))
    }

    /// The owner `BackupKey` to seal/open **owner-only** chunks with — deliberately
    /// `None` for a **bound** (cross-user shared) set and for a **content-keyed**
    /// group-less one (a WebDAV-served set), whose chunks MUST take the M2
    /// content-key path ([`Self::content_seal_root`]/[`Self::content_open_roots`])
    /// instead, which fails closed if the generation isn't loaded rather than
    /// degrade to the owner key or plaintext.
    ///
    /// Delegates rather than restates: FS-5DC is decided once, in
    /// [`fauna_core::crypto::effective_owner_key`], which carries the rule's full
    /// rationale and the two live shadowing failures that bought it.
    /// `FileDownloadKeys` reaches the same function with the same triple on the
    /// read side. (Thumbnails — `maybe_upload_thumbnail*` — stay on the raw
    /// `backup_key`: a shared-set thumbnail is a separate deferred concern, and
    /// that path is upload-only, never reached by the download-only bearer
    /// service.)
    fn effective_backup_key(&self) -> Option<&fauna_core::crypto::OwnerSealKey> {
        fauna_core::crypto::effective_owner_key(
            self.mls_group_id.as_deref(),
            self.content_keys.is_some(),
            self.backup_key.as_ref(),
        )
    }

    /// The label-seal root for **every** user-chosen string this engine seals —
    /// paths ([`Self::seal_recorded_path`]) and the set name
    /// ([`Self::sealed_set_name`]) alike. One function, not two matching ones:
    /// a set whose name sealed under a different root than its paths renders for
    /// a different audience than its file list, and because both wrong roots
    /// degrade to `Omit` that divergence is **silent**. Sharing the selection
    /// makes it unrepresentable rather than merely asserted ("one funnel per crate").
    ///
    /// The root is the one that already seals this set's chunks, never a new key
    /// category: a bound/served set's M2 content-key generation via
    /// [`Self::content_seal_root`], else the owner's `convergent_chunk_root()`
    /// via [`Self::effective_backup_key`]. `Ok(None)` = this engine holds no seal
    /// root at all (an unbound owner-only set with neither `epoch_secret` nor
    /// `backup_key` — test fixtures and the plaintext-era shape). Propagates
    /// `content_seal_root`'s FS-BIND-5 `Err`, so a bound keyless engine refuses
    /// rather than sealing under a root the nest could read.
    fn label_seal_root(&self) -> Result<Option<fauna_core::path_crypto::LabelRoot>> {
        use fauna_core::path_crypto::LabelRoot;

        // A `public`-audience folder's names and paths are world-readable by
        // ratified design — they are URLs (phase 4; the same arm as the chunk
        // seal sites). `None` already means "record plaintext only"
        // (`seal_recorded_path`), so the label funnel needs no new shape.
        if self.is_public_audience() {
            return Ok(None);
        }
        Ok(Some(match self.content_seal_root()? {
            Some((secret, Some(version))) => LabelRoot::content_key(secret, version),
            Some((secret, None)) => LabelRoot::owner(secret),
            None => match self.effective_backup_key() {
                Some(key) => LabelRoot::owner(key.convergent_chunk_root()),
                None => return Ok(None),
            },
        }))
    }

    /// Seal a recorded file path for the wire's `path_sealed` sibling — the
    /// single place a client turns a user-chosen path into a
    /// [`fauna_core::path_crypto::SealedLabel`].
    ///
    /// Implements `docs/goal/behavior/file-sync.md` § Sealed names & paths
    /// (the *paths-are-content* ruling): the root is [`Self::label_seal_root`]'s
    /// — **the one that already seals this set's chunks**, never a new key
    /// category — so exactly the audience that can open the set's bytes can
    /// render its names, and nobody weaker.
    ///
    /// **Convergent nonce** (salt = the path's own `path_hash`): the salt
    /// determines the plaintext, so re-recording an unchanged path reproduces a
    /// byte-identical blob — an idempotent retry does not churn the column, and
    /// two devices seal the same path identically.
    ///
    /// `Ok(None)` means this engine holds **no** seal root at all — an unbound
    /// owner-only set with neither `epoch_secret` nor `backup_key` (test
    /// fixtures and the plaintext-era shape). That records plaintext-only,
    /// exactly as before this slice; it is not a silent downgrade of a keyed
    /// engine, because the two arms above are the same ones the chunk seal
    /// takes.
    ///
    /// **Fails closed by inheritance, not by re-implementation:** the FS-BIND-5
    /// posture lives in [`Self::content_seal_root`], whose `Err` (bound set,
    /// content keys not loaded) propagates through here and out of
    /// [`Self::record_change`] — a bound keyless engine refuses to record at
    /// all rather than record a name the nest can read.
    pub(crate) fn seal_recorded_path(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let Some(root) = self.label_seal_root()? else {
            return Ok(None);
        };
        // Root selection above is this engine's; the salt, field tag, nonce mode
        // and encoding all live in the shared funnel, so this writer cannot
        // drift from media's, snapshots' or the conflict plane's
        // ("one funnel per
        // crate" wart).
        Ok(Some(
            fauna_core::label_custody::seal_path(&root, path).context("seal recorded path")?,
        ))
    }

    /// Stamp this engine's folder's **sealed name** on the nest, under the
    /// engine's current seal root — the set-name half of the *paths-are-content*
    /// ruling (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
    ///
    /// **Why the engine as well as the create.** A set is sealed from birth by
    /// the shared keyed create (`fauna_client_folders::create_set`, under the
    /// owner root), but a client wired without label custody creates it
    /// unsealed, and a bind or serve moves the name to the set's M2 audience.
    /// The engine is the component that both knows the set and holds its current
    /// root, so it is the keyed writer that re-stamps on bind — convergent, so a
    /// name already sealed under the same root is a no-op.
    ///
    /// Root selection is [`Self::seal_recorded_path`]'s, reached through the one
    /// shared funnel `fauna_core::label_custody::seal_set_name` — a bound/served
    /// set's M2 content-key generation, else the owner's
    /// `convergent_chunk_root()`. So the audience that opens the set's bytes
    /// opens its name, and no third root selection is introduced ("one funnel per crate" caution).
    ///
    /// Idempotent and cheap to repeat: the seal is convergent, so re-stamping an
    /// unchanged name writes byte-identical bytes. Returns `Ok(false)` — never an
    /// error — when there is nothing to stamp: no folder (the single-root
    /// shape), a reserved `__` rail, or no seal root at all. **The nest
    /// call is best-effort by design:** a name is a display label, so a
    /// transient failure must never fail
    /// the bind it is riding on. It re-stamps on the next pass.
    pub async fn stamp_sealed_set_name(&self) -> Result<bool> {
        let Some(folder) = self.folder.as_deref() else {
            return Ok(false);
        };
        let Some(sealed) = self.sealed_set_name()? else {
            return Ok(false);
        };

        // By hash: a sealed set rests no plaintext name on the nest, so a
        // by-name update finds nothing (`path-sealing.md` § the set-name plane).
        let req =
            fauna_protocol::folders::addressed(fauna_protocol::folders::FolderUpdateRequest {
                name: folder.to_string(),
                name_sealed: Some(fauna_protocol::ByteBuf::from(sealed)),
                ..Default::default()
            });
        let control = Arc::clone(&*self.control.read().unwrap());
        match control.update_folder(req).await {
            Ok(ok) => Ok(ok),
            Err(e) => {
                tracing::debug!(
                    folder = %fauna_core::log_redact::log_folder_name(folder),
                    "sealed set-name stamp deferred (retries next pass): {e}"
                );
                Ok(false)
            }
        }
    }

    /// This engine's folder name, sealed under the engine's current seal root
    /// — the pure half of [`Self::stamp_sealed_set_name`], separated from the push
    /// so the root selection is testable without a nest.
    ///
    /// Root selection is **the same call** [`Self::seal_recorded_path`] makes —
    /// [`Self::label_seal_root`] — so name and paths cannot seal under different
    /// roots even if one of the two is edited later; that used to be two matching
    /// `match` blocks, and its divergence would have been silent (both wrong roots
    /// degrade to `Omit`). Everything downstream of the root (salt, field tag,
    /// nonce mode, encoding) is `label_custody::seal_set_name`'s.
    ///
    /// `Ok(None)` = nothing to seal: no folder, a reserved `__` rail (refused
    /// inside the funnel), or no seal root at all. Propagates
    /// [`Self::content_seal_root`]'s FS-BIND-5 `Err` — a bound keyless engine
    /// refuses rather than sealing a name the nest could read.
    pub(crate) fn sealed_set_name(&self) -> Result<Option<Vec<u8>>> {
        let Some(folder) = self.folder.as_deref() else {
            return Ok(None);
        };
        let Some(root) = self.label_seal_root()? else {
            return Ok(None);
        };
        fauna_core::label_custody::seal_set_name(&root, folder)
    }

    /// Access the sync database.
    pub fn db(&self) -> &SyncDb {
        &self.db
    }

    /// Check if a relative path should be ignored — never folder content.
    /// Dotfile components are categorically excluded at any depth (the
    /// scanner's rule, `watcher::scan_recursive_filtered`, expressed for write
    /// paths that never scan — the File Provider callbacks, the watcher event
    /// filter), then the ignore matcher (built-in defaults + `.faunaignore`).
    pub fn is_ignored(&self, relative_path: &str) -> bool {
        crate::ignore::has_hidden_component(relative_path)
            || self.ignore.read().unwrap().is_ignored(relative_path)
    }

    /// Emit a progress event if the sender is active.
    pub fn emit_progress(&self, event: crate::progress::ProgressEvent) {
        crate::progress::emit(&self.transfer_pool.progress_tx, event);
    }

    /// Get the device sync channel ID hex for this actor, or `None` for a
    /// bearer-only hydration host that has no `MlsEngine` (and never device-syncs).
    pub fn device_sync_channel_id(&self) -> Option<String> {
        let actor_id = self.mls.as_ref()?.identity_actor_id();
        let ch = fauna_mls::channel::DeviceSyncChannel::expected_channel_id(&actor_id);
        Some(hex::encode(ch.0))
    }

    /// Arm the download-echo suppression for `relative_path` (see
    /// [`Self::recent_writes`]). Called immediately before the bytes hit disk.
    pub fn note_recent_download(&self, relative_path: &str) {
        self.recent_writes
            .lock()
            .unwrap()
            .insert(relative_path.to_string());
    }

    /// Returns true if this path was recently written by a download (and clears the flag).
    pub fn was_recent_download(&self, relative_path: &str) -> bool {
        self.recent_writes.lock().unwrap().remove(relative_path)
    }

    /// Arm the removal-echo suppression for `relative_path` (see
    /// [`Self::recent_removals`]). Called immediately before an applied remote
    /// delete unlinks the file.
    pub fn note_recent_removal(&self, relative_path: &str) {
        self.recent_removals
            .lock()
            .unwrap()
            .insert(relative_path.to_string());
    }

    /// Returns true if this path was recently removed by an applied remote
    /// delete (and clears the flag).
    pub fn was_recent_removal(&self, relative_path: &str) -> bool {
        self.recent_removals.lock().unwrap().remove(relative_path)
    }

    /// Put this engine under the **off-disk placeholder posture** — its root's
    /// placeholders are rows only, never files on its disk (the linux FUSE root;
    /// see the `placeholders_off_disk` field). Called by the serving host before
    /// its boot sweep; there is no way back, since a root does not change its OS
    /// binding under a running engine.
    pub fn set_placeholders_off_disk(&self) {
        self.placeholders_off_disk
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Is this engine under the off-disk placeholder posture
    /// ([`Self::set_placeholders_off_disk`])?
    pub fn placeholders_off_disk(&self) -> bool {
        self.placeholders_off_disk
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Upload an in-memory byte buffer through the chunker + `BackupKey`
    /// encrypt + `check_chunks` + `upload_chunks` + `upload_manifest`
    /// core path.
    ///
    /// `upload_bytes` skips both ends of [`SyncEngine::upload_file`]:
    ///
    /// - **The `tokio::fs::read` prologue** — the buffer is already
    ///   in memory, supplied by the caller.
    /// - **The `SyncDb` / `record_change` / device-sync-channel-post
    ///   epilogue** — segment uploads track their own state via the
    ///   backup coordinator's `segment_backup_state` table
    ///   (Plan 5 T8), and they intentionally don't show up in the
    ///   per-actor folder sync feed or the device-sync MLS channel.
    ///
    /// `relative_path` is passed to the transfer pool for progress
    /// events only (it tags `ChunkDone`); the destination's manifest
    /// is content-addressed and the folder routing happens via the
    /// bearer's folder membership, not via path-prefix dispatch.
    /// `_folder` is reserved for future per-path metadata — today
    /// the destination derives the folder from the bearer + manifest.
    ///
    /// Caller obligation: only feed buffers <= the segment-store
    /// size cap (Plan 5: 64 MB), so the in-memory path is fine — there
    /// is intentionally no streaming variant of this helper (Plan 5
    /// § Gotcha #2).
    ///
    /// Plan 5 T7: used by the nest's `NestBackupCoordinator` for both
    /// segment uploads and the kind-manifest mirror upload.
    ///
    /// Returns the uploaded path's **manifest content hash** — the value the
    /// segment-backup coordinator records on the destination via
    /// [`Self::record_change`] so the custodian's GC keeps the backup
    /// (`docs/goal/architecture/message-segment-store.md` § GC-safety —
    /// custodian-authoritative custody). The hash is a 32-byte content address
    /// of the chunk manifest; the per-path `record_change` epilogue is the
    /// coordinator's job (the *destination* must record, unlike the source-nest
    /// segment uploads this helper is shared with), so `upload_bytes` itself
    /// still skips `record_change`.
    pub async fn upload_bytes(
        &self,
        bytes: Vec<u8>,
        relative_path: &str,
        _folder: &str,
    ) -> Result<ContentHash> {
        let uploaded = self
            .upload_chunked_bytes(&bytes, relative_path, /* enqueue_resume = */ false)
            .await?;
        tracing::info!(
            path = %fauna_core::log_redact::log_path(relative_path),
            chunks = uploaded.manifest.chunk_hashes.len(),
            size = bytes.len(),
            "bytes uploaded"
        );
        Ok(uploaded.manifest_hash)
    }

    // There is deliberately NO `publish_reserved_bytes` here — no "upload the
    // blob AND record it" sibling for reserved (`__`) folders. One existed
    // briefly (added and removed 2026-08-02, zero callers throughout) for the
    // content-index builder's publisher seam; it was **dead on arrival**, and
    // the two reasons are worth stating here so the next session that reaches
    // for it stops at this comment instead of re-deriving them:
    //
    // 1. **The nest refuses the record.** `fauna.sync.changes.record` takes a
    //    reserved set only as a custody-copy destination
    //    (`bins/fauna-nest/src/sync_handlers.rs`, `record_change_core`), and a
    //    rail minted by `get_or_create_reserved_folder` is never a custody copy
    //    — nor may it be: a custody record lands in `backup_custody` with
    //    `seq = 0` and never enters the `sync_changes` device-pull feed, which
    //    is the exact replication `__index` needs. Pinned by
    //    `bins/fauna-nest/tests/conformance_folders.rs`
    //    `changes_record_refuses_a_reserved_rail_so_no_client_writer_can_use_it`.
    // 2. **Chunks under a reserved set are GC-unsafe.** [`Self::upload_bytes`]
    //    yields a *chunk manifest* hash, but the nest's GC classifies any hash
    //    referenced only by reserved sets as a **direct blob** — pinned as the
    //    bytes themselves, never walked for chunks
    //    (`bins/fauna-nest/src/db/sync_storage.rs`, `fold_direct_blob_refs`).
    //    The segment's live chunks would be swept: silent, unrecoverable loss
    //    on a user at-rest store.
    //
    // A reserved rail's real shape is the whole blob in the blob store plus a
    // `sync_changes` row written **at the DB layer** — the `__drafts`
    // structure (`bins/fauna-nest/src/db/drafts.rs`,
    // `record_drafts_blob_change`), which is also exactly what S2's standing pin
    // stages (`bins/fauna-nest/tests/index_survives_nest_restart.rs`). A client
    // reaches it through that rail's own dedicated kind, never through this
    // engine. Keeping the sync engine out of it is also what keeps it from
    // learning what an index is (`content-index.md` § Architectural rules 5).

    /// Shared chunker → compress → encrypt → check → upload → manifest
    /// pipeline — [`Self::seal_for_upload`] + [`Self::upload_sealed`] in one
    /// call, for [`Self::upload_bytes`] (in-memory buffer, no SyncDb
    /// prologue/epilogue — Plan 5 T8 segment uploads). The file path
    /// ([`Self::upload_file`]) calls the two halves itself: the own-PENDING
    /// retention mint sits between them (B2.5), and segment uploads must
    /// never mint file rows.
    ///
    /// When `enqueue_resume` is `true`, missing chunks are enqueued
    /// into `transfer_queue` for resume support and completed entries
    /// are deleted on success — that path is exclusive to
    /// `upload_file` (segment uploads resume by the coordinator
    /// re-driving its next pass).
    ///
    /// Returns the chunk manifest + its content hash + the count of
    /// chunks actually uploaded (the rest were destination-side hits).
    async fn upload_chunked_bytes(
        &self,
        bytes: &[u8],
        relative_path: &str,
        enqueue_resume: bool,
    ) -> Result<UploadedManifest> {
        let sealed = self.seal_for_upload(bytes)?;
        self.upload_sealed(sealed, relative_path, enqueue_resume)
            .await
    }

    /// The LOCAL half of an in-memory upload — resolve the seal root and run
    /// the chunk → compress → encrypt → manifest pipeline, no network. Split
    /// from [`Self::upload_sealed`] so `upload_file_inner` can mint the
    /// own-PENDING retention row at the exact seam the row-half design names
    /// (B2.5 — after the manifest exists locally, before the first network
    /// call): offline, the sealed manifest this returns IS the one the
    /// pending row serves and the eventual record carries — `seal_blob` is
    /// deterministic over `(bytes, seal_root)`.
    fn seal_for_upload(&self, bytes: &[u8]) -> Result<crate::seal::SealedBlob> {
        crate::seal::seal_blob(bytes, self.upload_seal_root()?)
    }

    /// The root (and generation to stamp) every upload of this engine seals
    /// under — `None` only for the deliberate `public_audience` plaintext arm.
    /// One resolver for the three upload shapes (in-memory
    /// [`Self::seal_for_upload`], the streaming local upload, and the windowed
    /// nest-sourced re-seal), so none of them can pick a root the others would
    /// not; fails closed for a keyless owner-only folder engine and, through
    /// `content_seal_root`, for a bound-but-keyless one.
    fn upload_seal_root(&self) -> Result<Option<([u8; 32], Option<u64>)>> {
        // Which root the chunks seal under. `backup_key` (owner-only) takes
        // priority over the M2 content key; only the content-key path carries a
        // generation version to stamp. Resolving it needs engine state, which is
        // why it stays here while the pipeline itself lives in `crate::seal` —
        // shared verbatim with the client-device custodian's local sink, so the
        // two produce byte-identical artifacts by construction rather than by
        // two implementations agreeing (`message-segment-store.md`
        // § Client-device custodian (pull)).
        // The ONE principled plaintext arm (folders re-model phase 4): the
        // owner explicitly declassified this folder, so its content rests
        // unsealed by ratified design (`principles.md` § The user always
        // controls their data, the one deliberate exception). `seal_root =
        // None` is already the pipeline's plaintext shape (`crate::seal`), and
        // an unsealed manifest (`stored_hashes = None`) is the shape every
        // reader passes through — no new representation, just the deliberate
        // selection of it. This flag, never a missing key, is what legalises
        // plaintext: the keyless bails below stay for every non-public engine.
        let seal_root: Option<([u8; 32], Option<u64>)> = if self.is_public_audience() {
            None
        } else if let Some(key) = self.effective_backup_key() {
            Some((key.convergent_chunk_root(), None))
        } else {
            self.content_seal_root()?
        };
        // Owner-only folder content is NEVER uploaded plaintext — the seal is
        // not a knob (`encryption-at-rest.md` § The sealed posture: "content
        // rests sealed on every nest") — the
        // sole exception being the deliberate `public_audience` arm above. An
        // unbound folder engine with no `BackupKey` fails closed here, the
        // owner-only twin of `content_seal_root`'s bound-but-keyless bail
        // (FS-BIND-5) — a mis-wired embedder surfaces at first upload instead
        // of silently resting the user's synced files in cleartext.
        if seal_root.is_none() && !self.is_public_audience() && self.folder.is_some() {
            anyhow::bail!(
                "seal_for_upload: owner-only folder engine holds no BackupKey — \
                 refusing to upload user content in plaintext (the embedder must derive \
                 BackupKey from the identity seed; encryption-at-rest.md § Folder files)"
            );
        }
        Ok(seal_root)
    }

    /// The NETWORK half of an in-memory upload: dedup-check, upload the
    /// missing chunks (optionally enqueueing resume), upload the manifest.
    async fn upload_sealed(
        &self,
        sealed: crate::seal::SealedBlob,
        relative_path: &str,
        enqueue_resume: bool,
    ) -> Result<UploadedManifest> {
        let crate::seal::SealedBlob {
            manifest,
            manifest_bytes,
            manifest_hash,
            chunks,
            content_key_version,
        } = sealed;

        // Phase 5 (`file-sync.md` § Content residency): a metadata-only
        // folder's chunk bytes never rest on the nest — skip the check and
        // the byte upload whole. The manifest still posts and the caller's
        // change record still lands: they ARE the metadata the nest keeps,
        // and a reader hydrates by-hash from a holding seat. (A reserved-rail
        // engine can never be armed — the nest refuses the flip there — so
        // the segment-backup caller below is untouched by construction.)
        if self.is_metadata_only_residency() {
            self.client
                .upload_manifest(&manifest_bytes)
                .await
                .context("upload_sealed: uploading manifest (metadata-only)")?;
            return Ok(UploadedManifest {
                manifest,
                manifest_hash,
                uploaded_count: 0,
                content_key_version,
            });
        }

        // Check which chunks the destination needs.
        let all_hashes: Vec<ContentHash> = chunks.iter().map(|(h, _)| *h).collect();
        let missing = self
            .client
            .check_chunks(&all_hashes)
            .await
            .context("upload_sealed: checking chunks with destination")?;

        let missing_chunks: Vec<(ContentHash, Vec<u8>)> = chunks
            .iter()
            .filter(|(hash, _)| missing.contains(hash))
            .map(|(h, d)| (*h, d.clone()))
            .collect();

        // Optionally enqueue missing chunks for resume (upload_file path).
        // Segment uploads (upload_bytes) intentionally skip this — the
        // backup coordinator re-drives `upload_bytes` on its own
        // schedule instead of replaying SyncDb's transfer_queue.
        let queue_ids: Vec<(i64, ContentHash)> = if enqueue_resume {
            let mut v = Vec::with_capacity(missing_chunks.len());
            for (hash, _) in &missing_chunks {
                let id = self
                    .db
                    .enqueue_transfer(relative_path, "upload", *hash, 0)?;
                v.push((id, *hash));
            }
            v
        } else {
            Vec::new()
        };

        let uploaded_count = missing_chunks.len();

        if !missing_chunks.is_empty() {
            let results = self
                .transfer_pool
                .upload_chunks(&self.client, &missing_chunks, relative_path)
                .await;

            if enqueue_resume {
                for ur in &results {
                    if ur.success
                        && let Some((qid, _)) = queue_ids.iter().find(|(_, h)| *h == ur.hash)
                        && let Err(e) = self.db.complete_transfer(*qid)
                    {
                        tracing::warn!(error = ?e, qid = *qid, "failed to mark transfer complete in queue (may re-upload)");
                    }
                }
            } else {
                // Fail LOUD on the `upload_bytes` (segment-backup) path: it has no
                // `transfer_queue` resume — the backup coordinator re-drives
                // its next pass — so returning `Ok` with rejected chunks would let the
                // coordinator advance `segment_backup_state` and report a backup
                // that stored nothing (the FS-BIND FOLLOW-ON A silent swallow — a
                // no-user-data-loss landmine). Bail *before* the manifest upload so
                // nothing downstream records the pass as done.
                let failed = results.iter().filter(|ur| !ur.success).count();
                if failed > 0 {
                    anyhow::bail!(
                        "upload_sealed: destination rejected {failed}/{} chunk upload(s) \
                         for {} — refusing to report the upload as complete",
                        results.len(),
                        fauna_core::log_redact::log_path(relative_path)
                    );
                }
            }
        }

        // Upload the manifest.
        self.client
            .upload_manifest(&manifest_bytes)
            .await
            .context("upload_sealed: uploading manifest")?;

        Ok(UploadedManifest {
            manifest,
            manifest_hash,
            uploaded_count,
            content_key_version,
        })
    }

    /// Process a locally modified file: hash, chunk, deduplicate, upload, update state.
    pub async fn upload_file(&self, relative_path: &str) -> Result<UploadOutcome> {
        self.upload_file_inner(relative_path, false)
            .await
            .map(|r| UploadOutcome {
                recorded: r.map(|u| u.recorded).unwrap_or(false),
                ..Default::default()
            })
    }

    /// [`Self::upload_file`] body. `force_reseal` bypasses the
    /// "already synced, skipping" short-circuit — the re-seal migration
    /// re-uploads files whose **content is unchanged** (the short-circuit's
    /// exact trigger) because their at-rest *sealing* is stale; without the
    /// bypass the pass silently uploads nothing for precisely the files it
    /// targets. The (non-forced) skip fires only when the record provably
    /// landed for exactly these bytes (`recorded_content_hash == local_hash`)
    /// and returns the recorded head with `recorded: true`; a `Synced` row
    /// without that proof re-drives the upload + record instead of skipping
    /// (a failed record must be retryable, not sticky). Returns the uploaded
    /// manifest + whether the change record reached the nest — the re-seal
    /// verify + supersede step consumes both (the local row deliberately keeps
    /// the merge-base manifest, so the receipt is the only way the caller
    /// learns the new manifest hash).
    async fn upload_file_inner(
        &self,
        relative_path: &str,
        force_reseal: bool,
    ) -> Result<Option<ResealUpload>> {
        let full_path = self.watch_dir.join(relative_path);

        // Check file size; route large files through the streaming path.
        let meta = tokio::fs::metadata(&full_path)
            .await
            .with_context(|| format!("stat {}", fauna_core::log_redact::log_path(relative_path)))?;

        // ⛔ THE CHOKE POINT. Never upload a file whose bytes are not on this disk.
        //
        // Every upload in the engine funnels through here — `upload_file`, `upload_pending`,
        // and the re-seal passes alike — so refusing a cloud-only placeholder *here* makes the
        // failure unrepresentable no matter which caller reaches for it, rather than relying on
        // each one to remember the guard. (`reconcile` also skips them earlier, so in a correct
        // run this never fires; it exists because the one that does fire is the one nobody
        // predicted.) A watcher on a cfapi root is the live example: the OS reports the
        // provider's OWN `CfExecute(TRANSFER_PLACEHOLDERS)` writes as ordinary file creations,
        // and `was_recent_download` does not cover placeholder *creation* — so without this
        // line a newly-populated cloud-only file walks straight into `upload_file`.
        //
        // Reading it would not silently corrupt the nest (the read fails rather than returning
        // zeros — measured), but it would block for cfapi's 60 s timeout on the engine's single
        // driving thread first. Refuse in microseconds instead, and say why.
        if crate::placeholder::is_cloud_placeholder(&meta) {
            anyhow::bail!(
                "refusing to upload {}: it is a cloud-only placeholder (its bytes \
                 live on the nest, not on this disk). Uploading it is never correct — there is \
                 nothing local to upload — and reading it would stall this thread for 60 s on a \
                 cfapi fetch that, by cfapi's own rule, is never delivered to the provider's own \
                 process.",
                fauna_core::log_redact::log_path(relative_path)
            );
        }

        // ⛔ The SIGNER_BOUND hold (`writer-signed-change-records.md` ruling
        // (11)(e)): a path whose head the succession take-over could not open
        // under its signer's roots stays on disk as noted history and is never
        // offered to the nest as new content — re-uploading it would launder
        // exactly the row the note refused. Here, so no caller (the scanner's
        // new-file path, a host's direct `upload_file`, a re-seal pass) has a
        // door beside it.
        if self.db.is_signer_bound(relative_path)? {
            anyhow::bail!(
                "refusing to upload {}: its head opened under none of its signer's roots at the \
                 succession take-over, so it is held as history and never offered as new content",
                fauna_core::log_redact::log_path(relative_path)
            );
        }

        // ⛔ THE SECOND CHOKE POINT — exclusive editing (`file-sync.md`
        // § Exclusive editing). A folder whose owner turned exclusive editing
        // on admits one writing device at a time, and this is where that
        // becomes true for every caller at once rather than for the two upload
        // passes that remember to ask. `upload_file` is public and hosts call
        // it directly (the cfapi hydration host, the apple in-process host),
        // so a guard that lived only in the passes would be a guard with a
        // door beside it.
        //
        // It is a **local** read — the posture the folder list installed, plus
        // this device's own hold — and never an acquire: `lease.acquire` takes
        // a free lease as a side effect of asking, so using it here would turn
        // the one rule this design has (*per pass, never per file*) inside out.
        // A pass that legitimately holds the lease sails through; a caller that
        // reaches past the passes on a locked folder is refused, which leaves
        // the entry `LocallyModified` for the next pass exactly as an offline
        // failure would.
        if self.lease.is_governed() {
            let now = crate::folder_lease::now_secs();
            if !self.lease.holds_lease_now(now)
                && self.lease.is_read_only(self.device_id_hex(), now)
            {
                anyhow::bail!(
                    "deferring the upload of {}: this folder is under exclusive \
                     editing and another device holds its write lease. The local edit is \
                     untouched on disk and uploads on the first pass after the lease frees.",
                    fauna_core::log_redact::log_path(relative_path)
                );
            }
        }

        let file_size = meta.len();

        if file_size >= fauna_core::chunker_stream::STREAMING_THRESHOLD {
            return self
                .upload_file_streaming_inner(relative_path, &full_path, file_size, force_reseal)
                .await;
        }

        let data = tokio::fs::read(&full_path).await.with_context(|| {
            format!(
                "reading {}",
                fauna_core::log_redact::log_path(relative_path)
            )
        })?;

        let file_hash = ContentHash::of_raw(&data);

        // Check if already synced with same hash, and preserve the last-received
        // manifest hash for 3-way merge base reconstruction. The local entry's
        // `content_key_version` pairs with that base manifest (the last *remote*
        // version), so preserve it alongside — the *new* upload's generation goes
        // only to the change record, never overwriting the merge-base pair.
        let mut base_manifest: Option<ContentHash> = None;
        let mut base_content_key_version: Option<u64> = None;
        // Set when these exact bytes provably reached the nest as a change
        // record (`commit_recorded_head` stamped the proof): the row about to
        // be recorded is then a PROVEN REISSUE — no novel content — and is
        // stamped `is_resolution = true` so receivers whose frontier passed it
        // skip it instead of merging it from a stale ancestor (the gap-1
        // ruling, `conflicts.md` § Concurrent resolution). The gap-3 ruling
        // widened the proof onto the causal ledger: bytes held at seq ≤ the
        // path frontier provably rest in the log too (a genuine revert to
        // held bytes then also stamps no-novel-content — if it races a
        // peer's concurrent edit it is skipped and re-reverted once caught
        // up, the content rung's already-recorded trade moved to the stamp).
        // A re-upload WITHOUT a proof (the lost-ack retry) keeps the edit
        // stamp: its record may never have landed, making its bytes the only
        // carrier of the edit. The BASE witness (loser-row ruling,
        // 2026-08-05) is the third proof: bytes equal to the live merge base
        // rest in the log by construction (the base advances only on an
        // echo, an applied remote, or a fail-closed merge write whose report
        // landed) — which is what keeps a reconcile re-upload of a merge
        // RESULT (no row's bytes, never ledger-held) from wearing an edit
        // stamp and stranding union-carrying winners behind an inflated
        // edit-frontier.
        let mut proven_reissue = self
            .read_base(relative_path)
            .is_some_and(|b| ContentHash::of_raw(&b) == file_hash);
        if let Some(entry) = self.db.get_entry(relative_path)? {
            proven_reissue = proven_reissue
                || entry.recorded_content_hash == Some(file_hash)
                || self.causal().frontier(relative_path).is_some_and(|f| {
                    self.causal()
                        .holds_content_at_or_below(relative_path, f, &file_hash)
                });
            if !force_reseal
                && entry.local_hash == Some(file_hash)
                && entry.state == SyncState::Synced
            {
                // Skip only when the record provably landed for exactly these
                // bytes (`commit_recorded_head` stamped the proof) — then the
                // skip is an honest "already recorded" and reports as such, so a
                // retried write whose ack was lost converges to an ack. A
                // `Synced` row WITHOUT the matching proof is the residue of a
                // failed record (the state flips to Synced before the record is
                // attempted, below): fall through and re-drive the upload —
                // chunk dedup makes the re-upload cheap and the record gets its
                // retry, instead of every retry skipping forever with the
                // change record never landing.
                if let Some(recorded_manifest) = entry.manifest_hash
                    && entry.recorded_content_hash == Some(file_hash)
                {
                    tracing::debug!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        "already synced and recorded, skipping"
                    );
                    return Ok(Some(ResealUpload {
                        manifest_hash: recorded_manifest,
                        recorded: true,
                    }));
                }
                tracing::debug!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    "synced row without a recorded-head proof — re-driving upload + record"
                );
            }
            base_manifest = entry.manifest_hash;
            base_content_key_version = entry.content_key_version;
        }

        // Compute mtime from file metadata
        let mtime = std::fs::metadata(&full_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(fauna_core::data::Timestamp::secs_or_zero)
            .unwrap_or(0);

        // Resolve the seal root BEFORE the row leaves its pending state: a seal
        // this engine may not make right now — held behind the set's content-key
        // floor (decision 2), or a bound engine with no keys — must leave the
        // write pending for the next pass to re-drive, never parked in
        // `Uploading`, which no pass picks up again.
        self.upload_seal_root()?;
        // A seal that may be made but not sent (decision 2′ (c)) leaves the row
        // pending too: it is sealed and minted below, then stops short of the
        // nest.
        let publish_hold = self.publish_hold();

        // Mark as uploading (preserve base manifest + its generation for merge)
        self.db.upsert_entry(
            relative_path,
            Some(file_hash),
            None,
            base_manifest,
            if publish_hold.is_some() {
                SyncState::LocallyModified
            } else {
                SyncState::Uploading
            },
            mtime,
            0,
            data.len() as i64,
            1,
            base_content_key_version,
        )?;

        // The LOCAL half of the shared pipeline first (chunk → compress →
        // encrypt → manifest, no network) — its manifest sizes the progress
        // event and is the own-PENDING mint's source.
        let sealed = self.seal_for_upload(&data)?;

        // Own-PENDING retention mint (B2.5) — after the local seal, before
        // the first network call: offline, this row is exactly what the share
        // leg serves; online the record's `Ok(seq)` upgrades it in place.
        self.mint_pending_own_change(
            relative_path,
            &sealed.manifest_hash,
            data.len() as i64,
            sealed.content_key_version,
            proven_reissue,
        );
        // Manifest retention for the share leg's serve-side byte half (slice
        // E) — the streaming path's twin; see that site's comment.
        if let Err(e) = self.db.retain_manifest(
            &hex::encode(sealed.manifest_hash.digest()),
            &sealed.manifest_bytes,
            relative_path,
            sealed.content_key_version,
        ) {
            tracing::warn!(
                error = %e,
                path = %fauna_core::log_redact::log_path(relative_path),
                "manifest retention failed; upload continues"
            );
        }
        // The serve's store-key index — here, before the network half, so a
        // metadata-only folder (whose upload posts no chunk) still serves.
        self.index_held_body(relative_path, &sealed.manifest, sealed.content_key_version);

        // Decision 2′ (c): the peer plane serves what was just minted; the
        // nest gets nothing until a pass reads the floor again.
        if let Some(hold) = publish_hold {
            anyhow::bail!(
                "upload of {} held back from the nest: {hold}",
                fauna_core::log_redact::log_path(relative_path)
            );
        }

        // Emit progress event before the network half runs so the UI shows
        // the file as in-flight while the upload happens.
        crate::progress::emit(
            &self.transfer_pool.progress_tx,
            crate::progress::ProgressEvent::FileStarted {
                path: relative_path.to_string(),
                size: data.len() as u64,
                chunk_count: sealed.manifest.chunk_hashes.len(),
            },
        );

        // The NETWORK half: check → upload → manifest, with
        // resume-via-`transfer_queue` enqueue.
        let uploaded = self
            .upload_sealed(sealed, relative_path, /* enqueue_resume = */ true)
            .await?;
        let manifest = uploaded.manifest;
        let manifest_hash = uploaded.manifest_hash;
        let uploaded_count = uploaded.uploaded_count;

        crate::progress::emit(
            &self.transfer_pool.progress_tx,
            crate::progress::ProgressEvent::FileDone {
                path: relative_path.to_string(),
            },
        );
        // Content moved: stamp the honest `last_transfer_at` (best-effort — the
        // status projection must never fail a completed upload).
        if let Err(e) = self.db.mark_transfer_completed() {
            tracing::warn!(error = ?e, "failed to stamp last_transfer_at");
        }

        // Update local state to synced (preserve base manifest + its generation
        // for merge — manifest_hash is only updated in download_and_write_file
        // when we receive a remote version, so it always points to the common
        // ancestor).
        self.db.upsert_entry(
            relative_path,
            Some(file_hash),
            Some(file_hash),
            base_manifest,
            SyncState::Synced,
            mtime,
            mtime,
            data.len() as i64,
            1,
            base_content_key_version,
        )?;

        tracing::info!(
            path = %fauna_core::log_redact::log_path(relative_path),
            chunks = manifest.chunk_hashes.len(),
            uploaded = uploaded_count,
            size = data.len(),
            "file uploaded"
        );

        // Generate + upload a thumbnail for an image file (client-side: the
        // plaintext is in `data`), so `fauna.media.list` can surface it. Runs
        // before the record so the hash rides on the change. Best-effort — see
        // `maybe_upload_thumbnail`.
        let thumbnail_hash = self.maybe_upload_thumbnail(&data).await;

        // Record change on node for folder sync, stamping the generation the
        // chunks were sealed under so a reader selects the right content key.
        let mut recorded = false;
        if let Some(ref folder) = self.folder {
            let manifest_hash_hex = hex::encode(manifest_hash.digest());
            match self
                .record_change(
                    folder,
                    relative_path,
                    Some(&manifest_hash_hex),
                    data.len() as i64,
                    "create",
                    uploaded.content_key_version,
                    thumbnail_hash.as_deref(),
                    // A fresh local edit derives from everything this engine
                    // has honestly incorporated (lower-bound law,
                    // `conflicts.md` clause 5). A PROVEN reissue (the re-seal
                    // migration re-uploading recorded bytes) carries the
                    // no-novel-content stamp instead, so receivers skip it
                    // rather than merge it (gap-1 ruling) — at the same
                    // floor-reduced claim.
                    if proven_reissue {
                        self.resolution_stamp(relative_path)
                    } else {
                        self.edit_stamp(relative_path)
                    },
                )
                .await
            {
                Ok(seq) => {
                    recorded = true;
                    // The record landed: the new manifest IS the nest head, so it
                    // becomes the row's manifest (hydration anchor + merge base).
                    // Leaving the pre-upload base in place made every
                    // freshly-synced local edit classify as a stale hydrated copy
                    // on the next fold (live 2026-07-17). On a FAILED record the
                    // base deliberately stays — the nest never saw this head.
                    self.commit_recorded_head(
                        relative_path,
                        manifest_hash,
                        data.len() as i64,
                        uploaded.content_key_version,
                    )?;
                    // The thumbnail pointer is part of the head we just
                    // recorded, so it is cached with the rest of it — a later
                    // re-seal on a build that cannot regenerate moves this blob
                    // instead of dropping the pointer (`SyncEntry::thumbnail_hash`).
                    self.db
                        .set_thumbnail_hash(relative_path, thumbnail_hash.as_deref())?;
                    // The ack names the row's seq, so the edit-frontier can
                    // count this device's own novelty IMMEDIATELY — waiting
                    // for the echo leaves the window in which a same-batch
                    // covering resolution adopts over the un-counted edit
                    // (`conflicts.md` clause 5, the in-flight guard;
                    // measured live by the 3-seat cell, 2026-08-03).
                    if !proven_reissue {
                        self.causal().advance_edit_frontier(relative_path, seq);
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %fauna_core::log_redact::log_path(relative_path), error = %e, "failed to record change on node");
                }
            }
        }

        Ok(Some(ResealUpload {
            manifest_hash,
            recorded,
        }))
    }

    /// Is every known row under `dir_rel/` clean, with at least one live one?
    ///
    /// The folder-✅ predicate: a directory may flip to the platform's in-sync
    /// state only when its known subtree is fully synced — `Synced` and
    /// cloud-only `Placeholder` rows are clean (a placeholder's content IS the
    /// nest's), `Deleted` tombstones are neutral (the file is gone everywhere),
    /// and any in-flight/diverged row poisons the subtree. No rows at all is
    /// `false`: folders carry directories implicitly via child paths, so an
    /// empty directory exists on no other device and must keep its honest
    /// pending state rather than claim a sync the fleet will never perform.
    pub fn subtree_fully_synced(&self, dir_rel: &str) -> Result<bool> {
        let prefix = format!("{}/", dir_rel.trim_end_matches('/'));
        let mut live = false;
        for entry in self.db.list_all()? {
            if !entry.path.starts_with(&prefix) {
                continue;
            }
            match entry.state {
                SyncState::Deleted => {}
                SyncState::Synced | SyncState::Placeholder => live = true,
                _ => return Ok(false),
            }
        }
        Ok(live)
    }

    /// The `fauna.sync.changes.record` for `relative_path` landed on the nest:
    /// stamp the row with the recorded head (manifest + size + generation). From
    /// this moment the recorded manifest is both the hydration anchor and the
    /// merge base — local content, nest head, and row agree — so the next fold's
    /// head comparison (`record_placeholders_from_changes`) sees its own echoed
    /// record as current instead of queueing the user's fresh edit for
    /// dehydration as a "stale hydrated copy".
    pub(crate) fn commit_recorded_head(
        &self,
        relative_path: &str,
        manifest_hash: ContentHash,
        size_bytes: i64,
        content_key_version: Option<u64>,
    ) -> Result<()> {
        self.db.update_recorded_head(
            relative_path,
            &manifest_hash,
            size_bytes,
            content_key_version,
            &self.own_actor_id().0,
        )?;
        // The recorded head now provably reassembles to the just-uploaded local
        // content (the upload path set `local_hash` to it before recording) —
        // stamp the dehydration gate's lossless-free proof. A FAILED record never
        // reaches here, so its row keeps the old/absent proof and the gate refuses
        // to free it (no data loss).
        self.db
            .stamp_recorded_content_from_local(relative_path, crate::db::ProofOrigin::OwnRecord)
    }

    /// Streaming upload path for files >= [`fauna_core::chunker_stream::STREAMING_THRESHOLD`].
    ///
    /// Chunks the file to a temporary directory (O(MAX_CHUNK) memory), reads
    /// each chunk file individually for upload, then cleans up.  The upload
    /// logic (dedup check, batched parallel upload, manifest, DB update) is
    /// identical to the in-memory path.
    ///
    /// `pub(crate)` so `upload_thumbnail_test` can drive it directly with a
    /// small real image (the streaming-path thumbnail logic without a 64 MiB
    /// fixture); `force_reseal` + the receipt as in [`Self::upload_file_inner`].
    pub(crate) async fn upload_file_streaming_inner(
        &self,
        relative_path: &str,
        full_path: &std::path::Path,
        file_size: u64,
        force_reseal: bool,
    ) -> Result<Option<ResealUpload>> {
        use fauna_core::chunker_stream::{chunk_file_streaming, content_hash_streaming};

        // Stream hash — no full-file allocation.
        let file_hash = content_hash_streaming(full_path).with_context(|| {
            format!(
                "streaming hash {}",
                fauna_core::log_redact::log_path(relative_path)
            )
        })?;

        // Check if already synced with same hash. Preserve the base manifest +
        // its generation (the merge-base pair), as in the in-memory path.
        let mut base_manifest: Option<ContentHash> = None;
        let mut base_content_key_version: Option<u64> = None;
        // Proven-reissue detection — same rules as the in-memory path (gap-1
        // ruling, widened by gap-3 onto the causal ledger and by the
        // 2026-08-05 loser-row ruling onto the BASE witness): recorded,
        // ledger-held, or base-matching bytes re-uploaded carry the
        // no-novel-content stamp.
        let mut proven_reissue = self
            .read_base(relative_path)
            .is_some_and(|b| ContentHash::of_raw(&b) == file_hash);
        if let Some(entry) = self.db.get_entry(relative_path)? {
            proven_reissue = proven_reissue
                || entry.recorded_content_hash == Some(file_hash)
                || self.causal().frontier(relative_path).is_some_and(|f| {
                    self.causal()
                        .holds_content_at_or_below(relative_path, f, &file_hash)
                });
            if !force_reseal
                && entry.local_hash == Some(file_hash)
                && entry.state == SyncState::Synced
            {
                // Same recorded-head-proof gate as the in-memory path: an
                // honest skip only when the record provably landed for these
                // bytes; a proof-less `Synced` row re-drives upload + record.
                if let Some(recorded_manifest) = entry.manifest_hash
                    && entry.recorded_content_hash == Some(file_hash)
                {
                    tracing::debug!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        "already synced and recorded (streaming), skipping"
                    );
                    return Ok(Some(ResealUpload {
                        manifest_hash: recorded_manifest,
                        recorded: true,
                    }));
                }
                tracing::debug!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    "synced row without a recorded-head proof (streaming) — re-driving upload + record"
                );
            }
            base_manifest = entry.manifest_hash;
            base_content_key_version = entry.content_key_version;
        }

        let mtime = std::fs::metadata(full_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(fauna_core::data::Timestamp::secs_or_zero)
            .unwrap_or(0);

        // The seal root before the row leaves its pending state — the in-memory
        // path's reason: a held or keyless seal must leave the write pending,
        // and so must one that may be made but not sent (decision 2′ (c)).
        self.upload_seal_root()?;
        let publish_hold = self.publish_hold();

        // Mark as uploading.
        self.db.upsert_entry(
            relative_path,
            Some(file_hash),
            None,
            base_manifest,
            if publish_hold.is_some() {
                SyncState::LocallyModified
            } else {
                SyncState::Uploading
            },
            mtime,
            0,
            file_size as i64,
            1,
            base_content_key_version,
        )?;

        // Chunk to a temp directory — O(MAX_CHUNK) memory.
        let temp_dir = tempfile::tempdir().context("creating temp dir for streaming chunks")?;
        let mut manifest = chunk_file_streaming(full_path, temp_dir.path()).with_context(|| {
            format!(
                "streaming chunk {}",
                fauna_core::log_redact::log_path(relative_path)
            )
        })?;

        // Pass 1 is already done: chunk_file_streaming wrote chunks to temp_dir
        // (keyed by plaintext hash) and returned the manifest with all hashes.

        // Resolve the seal root + the generation version to stamp once for the
        // whole file (the epoch is stable across this synchronous upload). The
        // owner `backup_key` maps to its convergent `chunk_crypto` root
        // (FS-BIND FOLLOW-ON A, user-ratified 2026-07-07); `?` fails closed
        // (FS-BIND-5): a bound-but-unkeyed engine refuses the streaming upload
        // rather than seal a shared set in plaintext.
        // The one upload resolver (`upload_seal_root`): the `public_audience`
        // plaintext arm (phase 4), else the owner root, else the content key —
        // failing closed for a keyless owner-only engine and a
        // bound-but-keyless one alike.
        let (chunk_root, content_key_version): (Option<[u8; 32]>, Option<u64>) =
            match self.upload_seal_root()? {
                Some((key, version)) => (Some(key), version),
                None => (None, None),
            };

        // FS-BIND (PIECE 6): for a sealed set (content key or owner backup root),
        // seal each chunk up front and re-key it in the blob store by its
        // **ciphertext** hash (recorded in `manifest.stored_hashes`), so the
        // F9-verifying route accepts the AEAD body (`blake3(body) ==
        // X-Content-Hash`). Each sealed chunk is written back into the temp dir
        // under its ciphertext hash, so check/enqueue/upload address the store
        // correctly while memory stays O(MAX_CHUNK). The plaintext hash stays the
        // AEAD salt + integrity anchor in `manifest.chunk_hashes`.
        let content_key_seal = chunk_root.is_some();
        if content_key_seal {
            let secret = chunk_root.expect("content_key_seal ⇒ chunk_root is Some");
            let mut stored = Vec::with_capacity(manifest.chunk_hashes.len());
            for hash in &manifest.chunk_hashes {
                let plain_path = temp_dir.path().join(hex::encode(hash.digest()));
                let data = std::fs::read(&plain_path)
                    .with_context(|| format!("reading chunk {}", hex::encode(hash.digest())))?;
                // The one per-chunk seal pipeline (compress → encrypt → re-key
                // by ciphertext hash) — shared with `seal_blob`'s batch form
                // and the share leg's serve-side re-derivation, so the three
                // cannot drift.
                let (store_key, ciphertext) = crate::seal::seal_chunk_body(hash, &data, &secret)?;
                std::fs::write(
                    temp_dir.path().join(hex::encode(store_key.digest())),
                    &ciphertext,
                )
                .context("writing sealed streaming chunk to temp dir")?;
                stored.push(store_key);
            }
            manifest.stored_hashes = Some(stored);
        }

        // The manifest is locally FINAL here (chunk hashes + stored keys; the
        // network half below never mutates it), so its canonical encoding —
        // the exact bytes uploaded at the end — exists before the first
        // network call. Compute it once: the own-PENDING retention mint
        // (B2.5) needs the hash now, and the manifest upload reuses the bytes.
        // The bytes are the WIRE form (a sealed set's plaintext hashes ride
        // only sealed — `ChunkManifest::wire_form`); `manifest` stays this
        // writer's plaintext view, which the upload loop below walks.
        let manifest_bytes =
            fauna_core::encoding::canonical_encode(&manifest.wire_form(chunk_root.as_ref())?)
                .context("serializing manifest (streaming)")?;
        let manifest_hash = ContentHash::of_raw(&manifest_bytes);
        self.mint_pending_own_change(
            relative_path,
            &manifest_hash,
            file_size as i64,
            content_key_version,
            proven_reissue,
        );
        // Manifest retention for the share leg's serve-side byte half (slice
        // E): the canonical bytes are in hand exactly here, self-verifying
        // (key == blake3(bytes)), so retention is unconditional and
        // best-effort like the row retention (B2.1) — losing the row costs
        // offline serveability only.
        if let Err(e) = self.db.retain_manifest(
            &hex::encode(manifest_hash.digest()),
            &manifest_bytes,
            relative_path,
            content_key_version,
        ) {
            tracing::warn!(
                error = %e,
                path = %fauna_core::log_redact::log_path(relative_path),
                "manifest retention failed (streaming); upload continues"
            );
        }
        // The serve's store-key index — the in-memory path's twin.
        self.index_held_body(relative_path, &manifest, content_key_version);

        // Decision 2′ (c) — the in-memory path's gate: served to peers, held
        // back from the nest.
        if let Some(hold) = publish_hold {
            anyhow::bail!(
                "upload of {} held back from the nest (streaming): {hold}",
                fauna_core::log_redact::log_path(relative_path)
            );
        }

        // Check which chunks the node needs — addressed by store key. A
        // metadata-only folder (phase 5) uploads no byte by design, so
        // nothing is ever "missing" at the nest: the empty set makes every
        // per-chunk stage below a no-op while the manifest still posts.
        let store_keys = manifest.store_keys();
        let missing: std::collections::HashSet<ContentHash> = if self.is_metadata_only_residency() {
            Default::default()
        } else {
            self.client
                .check_chunks(&store_keys)
                .await
                .context("checking chunks with node (streaming)")?
                .into_iter()
                .collect()
        };

        // Emit progress
        crate::progress::emit(
            &self.transfer_pool.progress_tx,
            crate::progress::ProgressEvent::FileStarted {
                path: relative_path.to_string(),
                size: file_size,
                chunk_count: manifest.chunk_hashes.len(),
            },
        );

        // Enqueue missing chunks (by store key) for resume support.
        let mut queue_ids: Vec<(i64, ContentHash)> = Vec::new();
        for store_key in &store_keys {
            if missing.contains(store_key) {
                let id = self
                    .db
                    .enqueue_transfer(relative_path, "upload", *store_key, 0)?;
                queue_ids.push((id, *store_key));
            }
        }

        // Pass 2: read, process, and upload each missing chunk individually
        // through the transfer pool. We read only one chunk at a time from the
        // temp dir, keeping memory at O(MAX_CHUNK) per in-flight transfer. A
        // sealed chunk (content key or owner backup root) was already sealed into
        // the temp dir under its store key (above), so it is just read back; a
        // plaintext chunk is read by its (== store) key and compressed. Each pair
        // is keyed by the store key the destination addresses it by.
        let missing_data: Vec<(ContentHash, Vec<u8>)> = manifest
            .chunk_hashes
            .iter()
            .zip(store_keys.iter())
            .filter(|(_, store_key)| missing.contains(*store_key))
            .map(|(plain_hash, store_key)| {
                if content_key_seal {
                    // Sealed ciphertext already on disk under its store key.
                    let path = temp_dir.path().join(hex::encode(store_key.digest()));
                    let data = std::fs::read(&path).with_context(|| {
                        format!("reading sealed chunk {}", hex::encode(store_key.digest()))
                    })?;
                    return Ok((*store_key, data));
                }
                let chunk_path = temp_dir.path().join(hex::encode(plain_hash.digest()));
                let data = std::fs::read(&chunk_path).with_context(|| {
                    format!("reading chunk {}", hex::encode(plain_hash.digest()))
                })?;

                // Frame through the one door (`fauna_core::chunk_seal`): the
                // plaintext arm stores the framed plaintext under its hash.
                let data =
                    fauna_core::chunk_seal::FramedChunk::frame(plain_hash, &data)?.into_body();

                Ok((*store_key, data))
            })
            .collect::<Result<Vec<_>>>()?;

        let results = self
            .transfer_pool
            .upload_chunks(&self.client, &missing_data, relative_path)
            .await;

        // Mark successful uploads in transfer queue
        for ur in &results {
            if ur.success
                && let Some((qid, _)) = queue_ids.iter().find(|(_, h)| *h == ur.hash)
                && let Err(e) = self.db.complete_transfer(*qid)
            {
                tracing::warn!(error = ?e, qid = *qid, "failed to mark transfer complete in queue (may re-upload)");
            }
        }

        crate::progress::emit(
            &self.transfer_pool.progress_tx,
            crate::progress::ProgressEvent::FileDone {
                path: relative_path.to_string(),
            },
        );

        // Upload the manifest (canonical bytes computed at the mint point
        // above — the manifest has not changed since).
        self.client
            .upload_manifest(&manifest_bytes)
            .await
            .context("uploading manifest (streaming)")?;

        // Content + manifest both landed: stamp `last_transfer_at`. Deliberately
        // *after* the manifest upload (unlike the FileDone emit above) — until
        // the manifest is up, the transfer hasn't completed.
        if let Err(e) = self.db.mark_transfer_completed() {
            tracing::warn!(error = ?e, "failed to stamp last_transfer_at");
        }

        // temp_dir is dropped here — chunk files are cleaned up automatically.

        // Update local state to synced (preserve the merge-base pair).
        self.db.upsert_entry(
            relative_path,
            Some(file_hash),
            Some(file_hash),
            base_manifest,
            SyncState::Synced,
            mtime,
            mtime,
            file_size as i64,
            1,
            base_content_key_version,
        )?;

        tracing::info!(
            path = %fauna_core::log_redact::log_path(relative_path),
            chunks = manifest.chunk_hashes.len(),
            size = file_size,
            "file uploaded (streaming)"
        );

        // Generate + upload a thumbnail for a large image, reading it
        // **incrementally from disk** (`full_path`) so the streaming path stays
        // O(decoded-thumbnail) memory — it never loads the whole >= 64 MiB file,
        // mirroring the O(MAX_CHUNK) chunk pipeline above. Runs before the record
        // so the hash rides on the change. Best-effort — see
        // `maybe_upload_thumbnail_from_path`.
        let thumbnail_hash = self.maybe_upload_thumbnail_from_path(full_path).await;

        // Record change on node for folder sync, stamping the seal generation.
        let mut recorded = false;
        if let Some(ref folder) = self.folder {
            let manifest_hash_hex = hex::encode(manifest_hash.digest());
            match self
                .record_change(
                    folder,
                    relative_path,
                    Some(&manifest_hash_hex),
                    file_size as i64,
                    "create",
                    content_key_version,
                    thumbnail_hash.as_deref(),
                    // Proven reissue → no-novel-content stamp; else a fresh
                    // edit — both at the honest claim (gap-1 ruling — see the
                    // in-memory path).
                    if proven_reissue {
                        self.resolution_stamp(relative_path)
                    } else {
                        self.edit_stamp(relative_path)
                    },
                )
                .await
            {
                Ok(seq) => {
                    recorded = true;
                    // Same head-commit as the in-memory path — see
                    // `commit_recorded_head`.
                    self.commit_recorded_head(
                        relative_path,
                        manifest_hash,
                        file_size as i64,
                        content_key_version,
                    )?;
                    // The head's thumbnail pointer, cached — see the in-memory path.
                    self.db
                        .set_thumbnail_hash(relative_path, thumbnail_hash.as_deref())?;
                    // Edit-frontier at the ack — see the in-memory path.
                    if !proven_reissue {
                        self.causal().advance_edit_frontier(relative_path, seq);
                    }
                }
                Err(e) => {
                    tracing::warn!(path = %fauna_core::log_redact::log_path(relative_path), error = %e, "failed to record change on node (streaming)");
                }
            }
        }

        Ok(Some(ResealUpload {
            manifest_hash,
            recorded,
        }))
    }

    /// Process a locally deleted file: record the delete on the nest, then
    /// tombstone the sync-database row.
    ///
    /// Record-FIRST, tombstone-on-success — the same ack honesty as
    /// [`Self::upload_file`]: `recorded` is true only when the delete change
    /// record reached the nest (or there was nothing to record — no tracked
    /// row, or no folder), so a caller acking a platform surface (the File
    /// Provider's `deleteItem` gate) keys on it. On a FAILED record the row
    /// deliberately survives untouched: the File Provider retry (the un-acked
    /// change stays pending OS-side) and the reconcile delete-detection pass (a
    /// `Synced` row whose file is gone) both re-drive it. Tombstoning first
    /// would erase the only local evidence that a delete is owed to the nest —
    /// the delete would be acked locally gone but silently never
    /// propagate.
    pub async fn handle_delete(&self, relative_path: &str) -> Result<UploadOutcome> {
        // Nothing tracked, or already tombstoned — vacuously complete. A retry
        // of a delete whose record already landed (and whose ack was lost)
        // resolves here and must ack, not spin.
        //
        // ⚠ The `Deleted` arm is load-bearing, not defensive tidiness.
        // `delete_entry` TOMBSTONES the row (`state = Deleted`) rather than
        // removing it, and `get_entry` returns tombstones — so an `is_none()`
        // test alone lets every later delete event for an already-deleted path
        // record a SECOND delete on the nest. And a tombstone means the first
        // record provably landed (record-FIRST: a failed record returns below
        // without tombstoning), so nothing is owed. Re-recording re-broadcasts
        // a delete for a path another device may have since RECREATED, erasing
        // the new file — the no-user-data-loss hazard in
        // `docs/goal/principles.md`. Pinned by
        // `delete_ack_test::an_already_tombstoned_delete_records_nothing`.
        let tracked = self.db.get_entry(relative_path)?;
        if tracked
            .as_ref()
            .is_none_or(|entry| entry.state == SyncState::Deleted)
        {
            return Ok(UploadOutcome {
                recorded: true,
                ..Default::default()
            });
        }

        // A placeholder found gone leaves `Placeholder` for `LocallyDeleted`
        // BEFORE the record is attempted (`delete-propagation.md` § *An offline
        // placeholder delete propagates*, decision (d)). Record-first still
        // stands — a failed record tombstones nothing — but on an on-demand root
        // the disk cannot hold the evidence for it: a `Placeholder` row whose
        // record failed would be listed straight back onto the disk by the next
        // population, and the user's delete silently reverted. `LocallyDeleted`
        // is never listed and never read as present; every later `reconcile`
        // retries it, and only the nest's ack below tombstones it. One site for
        // every caller — the boot sweep, the tick sweep, the live watcher, a File
        // Provider `deleteItem`.
        if tracked
            .as_ref()
            .is_some_and(|entry| entry.state == SyncState::Placeholder)
        {
            self.db
                .update_state(relative_path, SyncState::LocallyDeleted)?;
        }

        // Record the delete change on the node for folder sync (a delete
        // carries no manifest and no content-key generation).
        if let Some(ref folder) = self.folder
            && let Err(e) = self
                .record_change(
                    folder,
                    relative_path,
                    None,
                    0,
                    "delete",
                    None,
                    None,
                    self.edit_stamp(relative_path),
                )
                .await
        {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(relative_path),
                error = %e,
                "failed to record delete on node — keeping the row for retry"
            );
            return Ok(UploadOutcome::default());
        }

        self.db.delete_entry(relative_path)?;
        tracing::info!(path = %fauna_core::log_redact::log_path(relative_path), "file deleted from sync");
        Ok(UploadOutcome {
            recorded: true,
            ..Default::default()
        })
    }

    /// The delete universe — the rows with **evidence** that their file was on
    /// this disk, so that its absence from a scan means the user removed it
    /// (`delete-propagation.md` § *The floor on an on-demand root*, point (2), and
    /// § *An offline placeholder delete propagates*, decision (b)): every
    /// `Synced` row, every **seen** `Placeholder` row (this engine put it on the
    /// disk, or a scan observed it there — `SyncEntry::seen_on_disk`), and every
    /// `LocallyDeleted` row (a seen placeholder already found gone, its delete
    /// still owed to the nest). A never-seen `Placeholder` row is excluded: lazy
    /// population leaves most of them off the disk by design, and its absence is
    /// evidence of nothing. The one derivation both delete verbs read, so the
    /// hold, its count and the confirm cover the same rows.
    ///
    /// **Under the off-disk posture no `Placeholder` row is evidence, marked or
    /// not** ([`Self::set_placeholders_off_disk`]; `on-demand-files.md` § Linux
    /// FUSE binding, the dehydrate rule): such a root's placeholder is never on
    /// its disk, so its absence from a scan says nothing — whatever a mark that
    /// slipped through claims. `LocallyDeleted` rows stay: they are deletes the
    /// user made through the mount, still owed to the nest.
    fn rows_with_evidence(&self) -> Result<Vec<crate::db::SyncEntry>> {
        let mut rows = self.db.list_by_state(SyncState::Synced)?;
        if !self.placeholders_off_disk() {
            rows.extend(
                self.db
                    .list_by_state(SyncState::Placeholder)?
                    .into_iter()
                    .filter(|e| e.seen_on_disk),
            );
        }
        rows.extend(self.db.list_by_state(SyncState::LocallyDeleted)?);
        Ok(rows)
    }

    /// Derive which of `synced` — the rows with evidence
    /// ([`Self::rows_with_evidence`]) — are missing from `scan` (a row whose path
    /// wasn't found this scan pass) — the mass-delete floor's own input. Shared by
    /// `reconcile` and `apply_held_deletes`, which otherwise re-derived this
    /// identically. Takes `scan` and `synced` already fetched by the caller rather
    /// than fetching them itself: `reconcile` must re-fetch `synced` AFTER
    /// its own upsert/update loop (a file can flip out of `Synced` state
    /// mid-pass), so the fetch ORDER relative to other work genuinely
    /// differs between the two callers and isn't safe to fold in here.
    /// `missing` is cloned rather than borrowed from `synced` so it can
    /// outlive `synced` in the caller's own scope.
    ///
    /// **This is the single choke point where a read failure is stopped from
    /// becoming a delete**, and it is shared for exactly that
    /// reason: both verbs above derive their set here, so the
    /// unreadable-prefix rule cannot be applied to one and forgotten on the
    /// other — a split that would leave `apply_held_deletes` propagating the
    /// very rows `reconcile` had just withheld.
    fn missing_from_scan(
        scan: &crate::watcher::DirectoryScan,
        synced: Vec<crate::db::SyncEntry>,
    ) -> MissingFromScan {
        let scanned_paths: std::collections::HashSet<&str> = scan
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect();
        let mut missing = Vec::new();
        let mut withheld_unreadable = Vec::new();
        for entry in synced {
            if scanned_paths.contains(entry.path.as_str()) {
                continue;
            }
            // Not in the scan — but "not in the scan" is only evidence when the
            // scan could READ where the file would have been. Under an
            // unreadable prefix it is evidence of nothing.
            if scan.hides(&entry.path) {
                withheld_unreadable.push(entry.path);
            } else {
                missing.push(entry);
            }
        }
        MissingFromScan {
            missing,
            withheld_unreadable,
        }
    }

    /// Perform a full reconciliation scan: compare filesystem state against the sync database.
    pub async fn reconcile(&self) -> Result<ReconcileStats> {
        let scanned =
            crate::watcher::full_scan_filtered(&self.watch_dir, &self.ignore.read().unwrap())?;
        let mut stats = ReconcileStats::default();
        // Placeholders this scan observed present — marked seen after the loop.
        let mut observed_placeholders: Vec<&str> = Vec::new();

        // Find new/modified files
        for file in &scanned.files {
            // A cloud-only placeholder is PRESENT but its bytes are NOT HERE
            // (`crate::placeholder`). Never open one: on an on-demand root this process is the
            // provider, cfapi delivers no fetch callback for the provider's own I/O, and the
            // read below would therefore block for cfapi's full 60 s timeout before failing —
            // on the single driving thread that also serves hydration. N placeholders would
            // wedge the root for N × 60 s while logging "reconciliation complete".
            //
            // It is skipped here but NOT dropped from `scanned`: the delete-detection pass
            // below reads `scanned_paths`, and a placeholder missing from that set is read as a
            // file the user deleted — which would record a delete on the nest and erase it from
            // every other device (the state Explorer's "Free up space" leaves behind).
            if file.is_placeholder {
                stats.placeholder_files += 1;
                observed_placeholders.push(&file.relative_path);
                continue;
            }

            let full_path = self.watch_dir.join(&file.relative_path);

            match self.db.get_entry(&file.relative_path)? {
                None => {
                    // New file -- must hash to record
                    let hash = match fauna_core::chunker_stream::content_hash_streaming(&full_path)
                    {
                        Ok(h) => h,
                        Err(_) => continue,
                    };
                    self.db.upsert_entry(
                        &file.relative_path,
                        Some(hash),
                        None,
                        None,
                        SyncState::LocallyModified,
                        file.mtime_secs,
                        0,
                        file.size_bytes as i64,
                        1,
                        None, // brand-new local file: no remote manifest/generation yet
                    )?;
                    stats.new_files += 1;
                }
                Some(entry)
                    if entry.state == SyncState::Synced
                        && entry.local_mtime == file.mtime_secs
                        && entry.size_bytes == file.size_bytes as i64 =>
                {
                    // mtime+size unchanged on a synced file — skip expensive hash
                    stats.unchanged_files += 1;
                }
                Some(entry) => {
                    // mtime or size changed, or not synced — re-hash to confirm
                    let hash = match fauna_core::chunker_stream::content_hash_streaming(&full_path)
                    {
                        Ok(h) => h,
                        Err(_) => continue,
                    };
                    // Under the off-disk posture a `Placeholder` row over real
                    // bytes is a dehydrate cut short between its row flip and its
                    // unlink (`Self::dehydrate_off_disk`). Repair toward the
                    // bytes: the recorded head on the disk is `Synced` again —
                    // the user asked to free it, and asking again is cheap —
                    // and anything else is an ordinary local edit, uploaded by
                    // the arm below. The row is never left claiming the file
                    // is elsewhere while its bytes sit here.
                    if self.placeholders_off_disk()
                        && entry.state == SyncState::Placeholder
                        && entry.local_hash == Some(hash)
                        && entry.recorded_content_hash == Some(hash)
                    {
                        self.db.upsert_entry(
                            &file.relative_path,
                            Some(hash),
                            entry.remote_hash,
                            entry.manifest_hash,
                            SyncState::Synced,
                            file.mtime_secs,
                            entry.remote_mtime,
                            file.size_bytes as i64,
                            entry.version_num,
                            entry.content_key_version,
                        )?;
                        stats.unchanged_files += 1;
                        continue;
                    }
                    if (self.placeholders_off_disk() && entry.state == SyncState::Placeholder)
                        || entry.local_hash != Some(hash)
                    {
                        self.db
                            .update_state(&file.relative_path, SyncState::LocallyModified)?;
                        stats.modified_files += 1;
                    } else {
                        // Hash matches — update mtime+size in DB so next scan skips it
                        // (preserve the manifest + its generation untouched).
                        self.db.upsert_entry(
                            &file.relative_path,
                            Some(hash),
                            entry.remote_hash,
                            entry.manifest_hash,
                            entry.state,
                            file.mtime_secs,
                            entry.remote_mtime,
                            file.size_bytes as i64,
                            entry.version_num,
                            entry.content_key_version,
                        )?;
                        stats.unchanged_files += 1;
                    }
                }
            }
        }

        // The seen mark's self-heal (`delete-propagation.md` § *An offline
        // placeholder delete propagates*, decision (a)): every placeholder this scan
        // observed present is on the disk, whoever put it there, so its row is seen
        // — a lost mark write costs one pass, never a delete. A `LocallyDeleted` row
        // observed present again (its old directory restored while nothing ran) was
        // not deleted after all: back to `Placeholder`, no record owed.
        let mut to_mark = Vec::new();
        for rel in observed_placeholders {
            match self.db.get_entry(rel)? {
                Some(entry) if entry.state == SyncState::LocallyDeleted => {
                    self.db.update_state(rel, SyncState::Placeholder)?;
                    to_mark.push(rel);
                }
                Some(entry) if entry.state == SyncState::Placeholder && !entry.seen_on_disk => {
                    to_mark.push(rel);
                }
                _ => {}
            }
        }
        self.db.mark_seen(&to_mark)?;

        // Find deleted files (in DB but not on disk) and record deletes on nest.
        //
        // ⚠ `scanned_paths` is built from ALL of `scanned` — placeholders included. They are
        // present on disk; only their bytes are elsewhere. Filtering them out here (or in the
        // scan) would make every dehydrated file look deleted and propagate that delete to the
        // nest and every other device — so "free up space" would mean "erase". Pinned by
        // `a_dehydrated_file_is_never_recorded_as_a_delete`.
        let synced = self.rows_with_evidence()?;
        let synced_len = synced.len();
        let derived = Self::missing_from_scan(&scanned, synced);
        let observable_synced = derived.observable(synced_len);
        let missing = derived.missing;

        // Decision (d), in the same pass that found them gone and BEFORE the floor
        // decides: a seen placeholder missing from the scan is `LocallyDeleted`,
        // held or not. Under a hold this is what keeps the hold honest on an
        // on-demand root — a `Placeholder` row the population lists back onto the
        // disk would read PRESENT on the next pass, shrink the missing set below
        // totality, and release the held hydrated rows as ordinary per-file
        // deletes. `LocallyDeleted` stays missing until the nest acks or the file
        // is observed again.
        for entry in &missing {
            if entry.state == SyncState::Placeholder {
                self.db
                    .update_state(&entry.path, SyncState::LocallyDeleted)?;
            }
        }

        // Rows under a directory the scan could not enumerate are withheld
        // before the floor ever runs — see `MissingFromScan`. They
        // are reported (below, and on the progress channel) rather than merely
        // skipped, for the same reason placeholders are counted: a row that
        // appears in no count at all is exactly how a silent delete-side gap
        // survives.
        stats.deletes_skipped_unreadable = derived.withheld_unreadable.len();
        if !derived.withheld_unreadable.is_empty() {
            tracing::warn!(
                withheld = derived.withheld_unreadable.len(),
                prefixes = ?scanned
                    .unreadable_prefixes
                    .iter()
                    .map(|p| fauna_core::log_redact::log_path(p))
                    .collect::<Vec<_>>(),
                watch_dir = %fauna_core::log_redact::log_path(&self.watch_dir.to_string_lossy()),
                "unreadable is not absent: part of this folder could not be read, so its \
                 synced rows are NOT eligible for delete detection this pass — nothing was \
                 recorded for them; fix the permissions or the mount and they resume"
            );
        }

        // The mass-delete floor — see `is_mass_delete_floor`'s doc for the policy.
        // Hold: record nothing, report the count, log loudly. The hold is
        // DERIVED — nothing is stored, every reconcile re-evaluates — so
        // reappearing files (drive remounted) resume sync losslessly and a
        // crash mid-hold cannot strand state. A missing ROOT never reaches here
        // (`full_scan_filtered` errors above); an UNREADABLE root reaches here
        // with every row withheld, so `missing` is empty and the floor stays
        // quiet — which is the point: a floor hold is an offer to apply N
        // deletions, and an unreadable root must never make that offer. This closes the "root present, contents gone" window.
        // Propagating a held set later is an explicit user action (the app-side
        // confirm affordance), never this pass.
        if is_mass_delete_floor(missing.len(), observable_synced) {
            stats.deletes_held = missing.len();
            tracing::warn!(
                held = missing.len(),
                watch_dir = %fauna_core::log_redact::log_path(&self.watch_dir.to_string_lossy()),
                "mass-delete floor: every synced file is missing from the scan at once — \
                 holding all deletes (unmounted/vanished folder?); nothing was recorded"
            );
        } else {
            for entry in missing {
                // Record-first via `handle_delete`: on a failed record the row
                // survives as `Synced`-with-no-file, so THIS pass is the retry
                // path — the next reconcile re-detects and re-records instead
                // of the delete being tombstoned locally but never propagated.
                let outcome = self.handle_delete(&entry.path).await?;
                if outcome.recorded {
                    stats.deleted_files += 1;
                }
            }
        }
        // Report the verdict outward so the sync-agent status projection can
        // render it (`file-sync.md` § Files Appear Automatically: the captured
        // follow-on). Unconditional — a zero is what CLEARS a surface that was
        // showing a hold, and the hold is derived, so nothing else would.
        // `ReconcileStats` alone cannot carry this: the resident loop's
        // `converge` discards it (`always_resident.rs`), which is why a held
        // set was previously visible only in the warning above.
        self.emit_progress(crate::progress::ProgressEvent::DeletesHeld {
            held: stats.deletes_held as u64,
        });
        // Same contract, the other withholding: emitted on EVERY pass including
        // zero, because this is derived per pass too and a zero is what retracts
        // it once the folder reads again.
        self.emit_progress(crate::progress::ProgressEvent::DeletesSkippedUnreadable {
            skipped: stats.deletes_skipped_unreadable as u64,
        });

        // Purge tombstones older than 7 days
        const TOMBSTONE_TTL: i64 = 7 * 24 * 3600;
        let purged = self.db.purge_tombstones(TOMBSTONE_TTL)?;
        if purged > 0 {
            tracing::info!(purged, "purged old tombstones");
        }

        tracing::info!(
            new = stats.new_files,
            modified = stats.modified_files,
            unchanged = stats.unchanged_files,
            deleted = stats.deleted_files,
            placeholders = stats.placeholder_files,
            "reconciliation complete"
        );

        Ok(stats)
    }

    /// Apply a mass-delete-floor hold as real deletes — the explicit-user-action
    /// counterpart of [`Self::reconcile`]'s hold (`delete-propagation.md` § the
    /// mass-delete floor: *"propagation of a held set is an explicit user
    /// action, never this pass"*). The app-side *"your folder emptied — apply N
    /// deletions"* confirm affordance drives this; nothing else may.
    ///
    /// Four properties are the safety content — each is pinned:
    ///
    /// 1. **It re-derives the missing set NOW; it never consumes a displayed
    ///    count.** The user confirmed against a possibly-stale N. If the files
    ///    came back between render and click (the drive remounted), the floor
    ///    no longer holds and this applies **nothing** — the one race in which
    ///    honoring the click would destroy data that is demonstrably present.
    /// 2. **It applies only a set the floor is currently holding.** A partial
    ///    vanish is ordinary per-file sync semantics the next reconcile owns;
    ///    refusing it here keeps this from becoming a general force-delete API.
    /// 3. **Record-first, resumable.** Each row goes through the ordinary
    ///    [`Self::handle_delete`] (same tombstone + retry contract as
    ///    reconcile's per-file path). A failed record keeps its row `Synced`,
    ///    so a partial apply resumes: re-invoking applies the remainder, and a
    ///    crash mid-apply leaves the rest either still held (≥2 remaining, all
    ///    missing) or propagated by the next reconcile as ordinary deletes
    ///    (exactly the user-sanctioned outcome). Nothing new is stored — the
    ///    crash-safety shape the hold itself has.
    /// 4. **It re-derives through the same read-failure choke point**
    ///    ([`Self::missing_from_scan`]), so a row under a directory
    ///    this pass could not enumerate is never applied — the confirm the user
    ///    clicked cannot authorize deleting a file nobody has looked at. The
    ///    sharpest case is an **unreadable root**: every row is withheld, so
    ///    the floor is not active and this applies nothing, where the blind
    ///    scan made the root read as *"folder emptied"* and turned one click
    ///    into the loss of the whole set on the nest and on every device.
    ///
    /// The scan set includes placeholders exactly as reconcile's does: a
    /// dehydrated file is present (its bytes are elsewhere), so it can neither
    /// trip the floor nor be deleted by this verb.
    ///
    /// Emits a fresh [`ProgressEvent::DeletesHeld`] with the post-apply count so
    /// the status surface clears (or corrects) immediately instead of waiting
    /// out a rescan interval — the derived hold's zero-clears rule.
    pub async fn apply_held_deletes(&self) -> Result<AppliedHeldDeletes> {
        let scanned =
            crate::watcher::full_scan_filtered(&self.watch_dir, &self.ignore.read().unwrap())?;
        let synced = self.rows_with_evidence()?;
        let synced_len = synced.len();
        let derived = Self::missing_from_scan(&scanned, synced);
        let observable_synced = derived.observable(synced_len);
        if !derived.withheld_unreadable.is_empty() {
            tracing::warn!(
                withheld = derived.withheld_unreadable.len(),
                watch_dir = %fauna_core::log_redact::log_path(&self.watch_dir.to_string_lossy()),
                "apply_held_deletes: part of this folder could not be read — those rows are \
                 withheld from the apply, whatever count the confirm was rendered against"
            );
        }
        let missing = derived.missing;

        if !is_mass_delete_floor(missing.len(), observable_synced) {
            tracing::info!(
                missing = missing.len(),
                synced = synced_len,
                watch_dir = %fauna_core::log_redact::log_path(&self.watch_dir.to_string_lossy()),
                "apply_held_deletes: no hold is active — applying nothing \
                 (files reappeared, or a partial state the next reconcile owns)"
            );
            self.emit_progress(crate::progress::ProgressEvent::DeletesHeld { held: 0 });
            return Ok(AppliedHeldDeletes {
                applied: 0,
                remaining_held: 0,
                floor_was_active: false,
            });
        }

        let held_total = missing.len() as u64;
        let mut applied = 0u64;
        for entry in missing {
            let outcome = self.handle_delete(&entry.path).await?;
            if outcome.recorded {
                applied += 1;
            }
        }
        let remaining_held = held_total - applied;
        tracing::warn!(
            applied,
            remaining_held,
            watch_dir = %fauna_core::log_redact::log_path(&self.watch_dir.to_string_lossy()),
            "apply_held_deletes: user-confirmed propagation of a held set"
        );
        self.emit_progress(crate::progress::ProgressEvent::DeletesHeld {
            held: remaining_held,
        });
        Ok(AppliedHeldDeletes {
            applied,
            remaining_held,
            floor_was_active: true,
        })
    }

    /// Upload all files currently marked as LocallyModified.
    /// Returns the rels whose change record reached the nest
    /// ([`UploadOutcome::recorded`]) — the provably-synced-end-to-end set a platform
    /// sync-state flip keys on (the windows on-demand host marks these ✅) — plus the
    /// total bytes uploaded. A file whose upload failed, or whose record did not land,
    /// is absent from the rels.
    pub async fn upload_pending(&self, max_concurrent_files: usize) -> Result<(Vec<String>, u64)> {
        use futures_util::stream::{self, StreamExt};
        use std::sync::atomic::{AtomicU64, Ordering};

        let pending = self.db.list_by_state(SyncState::LocallyModified)?;

        // ── This pass's exclusive-edit window (`file-sync.md` § Exclusive
        // editing) ──
        //
        // Opened ONCE for the whole pass, never per file: an un-governed folder
        // pays no nest round-trip at all, and a governed one pays exactly one
        // acquire however many files the pass covers.
        //
        // A window this pass could not get — the lease is another device's, or
        // the nest could not be asked — returns **empty-handed and clean**:
        // every entry stays `LocallyModified`, which is the same state an
        // offline pass leaves behind and the state the next converge re-drives.
        // Returning `Ok` rather than an error is deliberate: nothing failed,
        // and `converge` stamps `last_clean_pass_at` off a pass that ran clean
        // — a deferral is not a clean *drain*, so it must leave the pending
        // rows behind that stamp reads, which it does, because it uploaded
        // nothing.
        //
        // The pre-seal edge first (decision 2): this pass seals, so it re-reads
        // the set's content-key floor rather than trust the last tick's.
        self.refresh_seal_floor().await;
        let window = self.open_lease_window().await;
        if !window.may_write() {
            tracing::info!(
                pending = pending.len(),
                "exclusive editing: this pass uploads nothing and every local edit stays \
                 pending for the next one"
            );
            return Ok((Vec::new(), 0));
        }

        let total_bytes = AtomicU64::new(0);
        // A `std::sync::Mutex` is right here: the closures are polled concurrently but
        // never hold the lock across an await (push-and-drop), same as `total_bytes`.
        let recorded = std::sync::Mutex::new(Vec::new());

        stream::iter(pending)
            .for_each_concurrent(max_concurrent_files, |entry| {
                let total_bytes = &total_bytes;
                let recorded = &recorded;
                async move {
                    // The renewal half of the window, on a pass that outruns
                    // half the lease TTL. A lock and a comparison per file; a
                    // nest round-trip once per half-TTL. `false` means the hold
                    // lapsed and another device took over mid-pass — stop
                    // writing rather than write through the new holder's lease;
                    // the untouched entries are the next pass's work.
                    if !self.renew_lease_if_due().await {
                        return;
                    }
                    match self.upload_file(&entry.path).await {
                        Ok(outcome) => {
                            total_bytes.fetch_add(entry.size_bytes as u64, Ordering::Relaxed);
                            if outcome.recorded {
                                recorded.lock().unwrap().push(entry.path.clone());
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                path = %fauna_core::log_redact::log_path(&entry.path),
                                error = %e,
                                "failed to upload"
                            );
                        }
                    }
                }
            })
            .await;

        // The pass has drained — give the folder back. A no-op on an
        // un-governed folder, and never fatal on a governed one (the nest
        // expires the row on its own TTL).
        if window.holds_lease() {
            self.close_lease_window().await;
        }

        Ok((
            recorded.into_inner().unwrap(),
            total_bytes.load(Ordering::Relaxed),
        ))
    }

    /// Re-seal every materialized file not yet at the current M2 content-key
    /// generation under `current`, so a member (or the WebDAV MDA) holding only
    /// recent generations can decrypt the set's full back-catalogue — closing the
    /// history-on-join **pre-bind gap** (`mls-group-key-material.md` § M2:
    /// `bind_set`/`share_set` bind a genesis key but never re-seal files that
    /// existed before the share, leaving them `content_key_version = None` and
    /// undecryptable by a joiner). Piece A of the re-seal migration.
    ///
    /// **Additive / no-data-loss by construction:** it only re-uploads through the
    /// ordinary seal path (`upload_file` → `content_seal_root` = `current`,
    /// re-recording the change stamped with the new generation, which supersedes the
    /// old one per-path in the reader's latest-per-path fold); it deletes **no**
    /// chunks (the superseded pre-bind chunks are reclaimed later, only after verify
    /// — Piece B). **Store-idempotent** (convergent `chunk_crypto` ⇒ byte-identical
    /// ciphertext, deduped by `check_chunks`). A **no-op** for an unbound / plaintext
    /// engine; a bound-but-keyless engine **fails closed** (`content_seal_root`
    /// errors) rather than re-seal shared content in plaintext. Fails fast on the
    /// first file error (already-re-sealed files stay valid — additive; a resume
    /// re-drives). Returns the number of files re-sealed.
    ///
    /// NOTE (termination / resumability): the pass is **self-terminating and
    /// ungated** — the sync agent runs it on every engine start. A successful
    /// record stamps the local row's `content_key_version` at `current`
    /// (`commit_recorded_head`), and the loop below skips rows already at the
    /// target generation, so a re-sealed file is not revisited; a failed or
    /// interrupted pass leaves its rows unstamped and the next start resumes
    /// there. (A sealed "this set owes a pass" sentinel once gated it;
    /// retired 2026-09-25 with no reader left.)
    ///
    /// **Verify + reclaim (Piece B).** Each re-sealed file is verified
    /// retrievable + decryptable + integrity-checked end-to-end over the real
    /// route (`download_file_bytes_by_manifest` — whole-file blake3 against the
    /// new manifest) BEFORE any reclaim; a verify failure aborts the pass
    /// (fail-loud — the re-sealed copy must be readable; the next start retries).
    /// Then `fauna.sync.changes.supersede` marks the path's pre-re-seal change
    /// rows superseded so nest GC reclaims their now-unreferenced chunks. The
    /// supersede is **best-effort**: reclaim is a storage optimization, so any
    /// failure (a transport error, a head moved by a concurrent
    /// record) logs and defers — it never fails the pass (I2: any client×nest
    /// within a major is supported). The verify walks the new copy a window at
    /// a time (`verify_file_by_manifest`) — bounded, like the re-seal itself.
    pub async fn reseal_pending_under_current(&self) -> Result<usize> {
        let Some(target_version) = self.prebind_reseal_target()? else {
            return Ok(0);
        };
        let mut resealed = 0usize;
        let mut unrecorded = 0usize;
        let heads = self.prebind_heads().await?;
        // One file at a time (bounded memory on a capability host).
        for entry in self.prebind_owed_rows(target_version, &heads)? {
            match self.reseal_prebind_row(&entry.path, target_version).await? {
                ResealDisposition::Nothing => {} // neither on this disk nor on the nest
                ResealDisposition::Unrecorded => unrecorded += 1,
                ResealDisposition::Recorded => resealed += 1,
            }
        }
        Self::require_fully_recorded("re-seal under current", resealed, unrecorded)?;
        Ok(resealed)
    }

    /// **The re-record leg of the owner's custody reconcile**
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records* →
    /// *Custody shape of the set nonce*, point (g)): every nest head THIS
    /// device signed under one of the set's retired nonces — a race loser's
    /// mint, a duplicate the reconcile retired — is re-recorded under the live
    /// one. A re-sign, not a re-seal: the same manifest, size, generation and
    /// thumbnail, no chunk moved, stamped resolution-class (it carries no novel
    /// content). Readers verify the current identity's rows under every nonce
    /// of the lineage (`writer-signed-change-records.md` ruling (11)(c)(3)),
    /// so the move is a convenience: it keeps every head under the one nonce
    /// a reader tries first.
    ///
    /// Only this device's own heads, and only ones its own signer's
    /// (deterministic) signature over the row under a retired nonce reproduces
    /// byte for byte — a row another writer signed, or one the nest altered,
    /// never matches, so the leg can re-sign nothing but what this device
    /// already said. The retired list never holds a deleted earlier
    /// incarnation's nonce (`custody::set_nonces_for`), so a nest replaying
    /// such a row here gets it refused, not laundered. Idempotent: a head
    /// already under the live nonce is skipped, and a re-recorded one is the
    /// new head. Returns how many heads were re-recorded — a record counts only
    /// when the door answers with a `seq` above the head it re-records, since a
    /// door that folds it into an existing row landed nothing; a record that
    /// did not land is warned and retried at the next engine start.
    pub async fn rerecord_under_live_nonce(&self) -> Result<usize> {
        use fauna_protocol::sync_writer_sig::SignedChange;
        let retired = self
            .retired_set_nonces
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing = self
            .change_signing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(signing), Some(folder)) = (signing, self.folder.as_deref()) else {
            return Ok(0);
        };
        if retired.is_empty() {
            return Ok(0);
        }
        let own_device = self.client.device_id_hex().to_string();
        let own_key = signing.signer.signer_key();
        let mut heads: std::collections::HashMap<String, fauna_protocol::sync::SyncChange> =
            std::collections::HashMap::new();
        // Unjudged on purpose: the leg's own check below is stricter than the
        // reader's — a byte-exact re-sign by this device under a retired
        // nonce of the set's lineage.
        for row in self.fetch_change_batch(folder, 0, false).await?.changes {
            if row.is_retention == Some(true) {
                continue;
            }
            match heads.get(&row.path_hash) {
                Some(h) if h.seq >= row.seq => {}
                _ => {
                    heads.insert(row.path_hash.clone(), row);
                }
            }
        }
        let mut rerecorded = 0usize;
        for head in heads.values() {
            if head.device_id.as_deref() != Some(own_device.as_str())
                || head.signer_key.as_ref().map(|k| &k[..]) != Some(&own_key[..])
            {
                continue;
            }
            let Some(signature) = head.signature.as_ref() else {
                continue;
            };
            let signed_under = |nonce: [u8; 32]| {
                SignedChange::for_row(head, nonce)
                    .map(|st| signing.signer.sign_statement(&st)[..] == signature[..])
                    .unwrap_or(false)
            };
            if signed_under(signing.set_nonce) || !retired.iter().any(|n| signed_under(*n)) {
                continue;
            }
            let Some(path) = head.path.as_deref() else {
                tracing::warn!(
                    "re-record under the live nonce: a retired-nonce head whose path did not \
                     open; skipped"
                );
                continue;
            };
            // The fetch above is unjudged, so the reader's plaintext-path check
            // is this leg's own: a served `path` is outside the statement, and
            // re-recording by it would put this device's head at a path the
            // nest chose. (A path opened from the sealed label matches by
            // construction.)
            if !fauna_protocol::sync_writer_sig::plaintext_path_matches(head) {
                tracing::warn!(
                    seq = head.seq,
                    "re-record under the live nonce: a retired-nonce head served with a \
                     plaintext path that does not match its signed path_hash; skipped"
                );
                continue;
            }
            match self
                .record_change(
                    folder,
                    path,
                    head.manifest_hash.as_deref(),
                    head.size_bytes,
                    &head.change_type,
                    head.content_key_version,
                    head.thumbnail_hash.as_deref(),
                    self.resolution_stamp(path),
                )
                .await
            {
                Ok(seq) if seq > head.seq => rerecorded += 1,
                // The door answered with a row no newer than the head: it
                // swallowed the record (a nest whose echo guard folds a
                // delete into the retired-nonce tombstone). Nothing landed
                // under the live nonce, so it is not counted.
                Ok(seq) => tracing::warn!(
                    path = %fauna_core::log_redact::log_path(path),
                    seq,
                    head_seq = head.seq,
                    "re-record under the live nonce was answered with no new row; \
                     retried next engine start"
                ),
                Err(e) => tracing::warn!(
                    path = %fauna_core::log_redact::log_path(path),
                    error = %format!("{e:#}"),
                    "re-record under the live nonce did not land; retried next engine start"
                ),
            }
        }
        rerecorded += self.take_over_history_heads(folder).await?;
        Ok(rerecorded)
    }

    /// The succession take-over (`writer-signed-change-records.md` ruling
    /// (11)(d), with (e)), the re-record leg's second arm as ruling (8)(d)
    /// amended it: after the cut every head the owner's chain signed under a
    /// retired nonce judges **history** (ruling (11)(c), arm (2)), so the set's
    /// listing stays as the cut left it until the successor records what
    /// those heads held, as itself, under the live nonce.
    ///
    /// - **Who:** the set's owner's own hosts only ([`Self::owns_set`]) — a
    ///   member holding the content keys could otherwise re-sign the owner's
    ///   history as itself.
    /// - **What:** every path whose newest record (admitted or history; a held
    ///   row skips the path this pass, a refused one is no record) judges
    ///   history under this engine's own reader. A path another device already
    ///   took is no candidate by construction: its newest record is that
    ///   device's live-nonce head.
    /// - **From where:** a device holding local state for the set re-records
    ///   only the paths it holds, at the manifest its entry holds and under the
    ///   roots the entry's persisted signer allows ([`Self::manifest_signer`]'s
    ///   column; an unknown one allows no owner root). The nest's own heads are
    ///   adopted only while this device's adoption marker is
    ///   [`Begun`](crate::db::AdoptionState::Begun) — judged as it arrived
    ///   ([`Self::set_adoption_marker`]) — and the spend is recorded after the
    ///   pass completes, so a re-pushed marker never adopts twice and a crash
    ///   part-way adopts the remainder at the next start.
    /// - **How:** bytes under an owner root are opened under the signer's
    ///   roots and re-sealed under the current one
    ///   ([`Self::reseal_nest_copy_windowed_under`]); bytes under a content key
    ///   or in plaintext are re-signed over the SAME manifest once it opens. A
    ///   resolution stamp, never an edit stamp, and **no supersede**: the
    ///   history row stays the version it is.
    /// - **Not carried:** a head whose name or bytes open under none of its
    ///   signer's roots is noted ([`SyncDb::note_signer_bound`] on a held
    ///   entry) and skipped, and the upload choke point holds the path (ruling
    ///   (11)(e)).
    /// - **Deletes:** none on an owner-only set. A history delete over an
    ///   older row signed outside the owner's chain (a member's, which the cut
    ///   resurfaced) is re-recorded as the current identity's delete — where
    ///   this device holds no live entry for the path — and counted only when
    ///   the door answers a row above the head.
    async fn take_over_history_heads(&self, folder: &str) -> Result<usize> {
        use fauna_core::file_download::RecordSigner;
        use fauna_protocol::sync_row_verify::RowVerdict;
        if !self.owns_set() {
            return Ok(0);
        }
        let mut reader = self
            .row_reader
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if reader.binding().retired_set_nonces.is_empty() {
            return Ok(0);
        }
        let marker = self.adoption_marker();
        let adopting = match marker.as_ref() {
            Some(nonce) => self.db.adoption_state(nonce)? == Some(crate::db::AdoptionState::Begun),
            None => false,
        };
        let (served, signer_certs) = self.list_changes(folder, 0).await?;
        reader.ingest_certs(&signer_certs);

        // Each path's rows, newest first.
        let mut by_path: std::collections::HashMap<String, Vec<&fauna_protocol::sync::SyncChange>> =
            std::collections::HashMap::new();
        for row in &served {
            if row.is_retention == Some(true) {
                continue;
            }
            by_path
                .entry(row.path_hash.to_ascii_lowercase())
                .or_default()
                .push(row);
        }
        struct Candidate<'a> {
            head: &'a fauna_protocol::sync::SyncChange,
            signed_as: [u8; 32],
            /// The newest record below the head was signed outside the
            /// owner's chain — a member's row the cut resurfaced.
            over_member: bool,
        }
        let mut candidates: std::collections::HashMap<String, Candidate<'_>> =
            std::collections::HashMap::new();
        for (hash, mut rows) in by_path {
            rows.sort_by_key(|r| std::cmp::Reverse(r.seq));
            let mut found: Option<(&fauna_protocol::sync::SyncChange, [u8; 32])> = None;
            let mut over_member = false;
            for row in rows {
                let verdict = reader.judge(row);
                if found.is_none() {
                    match verdict {
                        RowVerdict::History { signed_as, .. } => found = Some((row, signed_as)),
                        // Not a record: the cursor passes it, and so does this.
                        RowVerdict::Refused(_) => continue,
                        // A current head, or one not judgeable yet.
                        _ => break,
                    }
                    continue;
                }
                match verdict {
                    RowVerdict::Verified { signed_as, .. } => {
                        over_member = !self.is_own_account(&signed_as);
                        break;
                    }
                    RowVerdict::Exempt => break,
                    _ => continue,
                }
            }
            if let Some((head, signed_as)) = found {
                candidates.insert(
                    hash,
                    Candidate {
                        head,
                        signed_as,
                        over_member,
                    },
                );
            }
        }

        let held: std::collections::HashMap<String, crate::db::SyncEntry> = self
            .db
            .list_all()?
            .into_iter()
            .filter(|e| e.state != SyncState::Deleted)
            .map(|e| (hex::encode(fauna_core::sync::path_hash(&e.path)), e))
            .collect();
        tracing::info!(
            candidates = candidates.len(),
            rule = if adopting {
                "adoption marker: the nest's history heads"
            } else {
                "local state: the paths this device holds"
            },
            "succession take-over"
        );

        let mut rerecorded = 0usize;
        // A transient failure (the door, a fetch) keeps an adoption unspent.
        let mut complete = true;
        for (hash, candidate) in &candidates {
            let head = candidate.head;
            let entry = held.get(hash);
            let is_delete =
                head.manifest_hash.is_none() || head.change_type.eq_ignore_ascii_case("delete");
            // What this device records, and under whose bound it opens.
            let (manifest_hex, size_bytes, content_key_version, thumbnail, signer) = if is_delete {
                if !candidate.over_member || entry.is_some() {
                    continue;
                }
                (
                    None,
                    0,
                    None,
                    None,
                    self.record_signer_of(&candidate.signed_as),
                )
            } else {
                match entry {
                    Some(e) => {
                        let Some(manifest) = e.manifest_hash else {
                            continue;
                        };
                        (
                            Some(hex::encode(manifest.digest())),
                            e.size_bytes,
                            e.content_key_version,
                            e.thumbnail_hash.clone(),
                            e.head_signed_as
                                .map_or(RecordSigner::Other, |a| self.record_signer_of(&a)),
                        )
                    }
                    None if adopting => (
                        head.manifest_hash.clone(),
                        head.size_bytes,
                        head.content_key_version,
                        head.thumbnail_hash.clone(),
                        self.record_signer_of(&candidate.signed_as),
                    ),
                    None => continue,
                }
            };
            let keys = fauna_core::file_download::FileDownloadKeys {
                record_signer: signer,
                ..self.download_keys()
            };
            let note = |path: &str, manifest: Option<&ContentHash>| {
                if let Err(e) = self.db.note_signer_bound(path, manifest) {
                    tracing::warn!(error = %format!("{e:#}"), "noting a held path failed");
                }
            };
            // The name: the entry's own, else the head's under the same bound
            // as the bytes (a plaintext one the judge already checked).
            let path = match entry {
                Some(e) => e.path.clone(),
                None => match head.path.as_deref().filter(|p| !p.is_empty()) {
                    Some(p) => p.to_string(),
                    None => match fauna_core::label_custody::open_change_path(
                        |generation| keys.label_open_roots(generation),
                        head.path_sealed.as_ref().map(|b| &b[..]),
                        &head.path_hash,
                    ) {
                        fauna_core::label_custody::ChangePathOpen::Opened(p) => p,
                        fauna_core::label_custody::ChangePathOpen::NoRoot => {
                            tracing::warn!(
                                seq = head.seq,
                                "succession take-over: a history head whose name opens under \
                                 none of its signer's roots; not carried"
                            );
                            continue;
                        }
                        _ => continue,
                    },
                },
            };

            let mut manifest_hex = manifest_hex;
            let mut size_bytes = size_bytes;
            let mut content_key_version = content_key_version;
            let mut thumbnail = thumbnail;
            if let Some(hex_hash) = manifest_hex.clone() {
                let Ok(manifest) = Self::parse_manifest_hash(&hex_hash) else {
                    continue;
                };
                let owner_root = content_key_version.is_none()
                    && !self.is_public_audience()
                    && self.effective_backup_key().is_some();
                if owner_root {
                    if signer == RecordSigner::Other {
                        // No owner root is this signer's to open.
                        note(&path, Some(&manifest));
                        tracing::warn!(
                            path = %fauna_core::log_redact::log_path(&path),
                            "succession take-over: a head under an owner root whose signer is not \
                             this account's; held, not carried"
                        );
                        continue;
                    }
                    let uploaded = match self
                        .reseal_nest_copy_windowed_under(&path, manifest, None, &keys)
                        .await
                    {
                        Ok(u) => u,
                        Err(e) => {
                            if fauna_core::apply_failure::permanent_reason(&e)
                                == Some(
                                    fauna_core::apply_failure::PermanentApplyFailure::SIGNER_BOUND
                                        .reason,
                                )
                            {
                                note(&path, Some(&manifest));
                            } else {
                                complete = false;
                            }
                            tracing::warn!(
                                path = %fauna_core::log_redact::log_path(&path),
                                error = %format!("{e:#}"),
                                "succession take-over: a head whose bytes do not open under its \
                                 signer's roots; not carried"
                            );
                            continue;
                        }
                    };
                    manifest_hex = Some(hex::encode(uploaded.manifest_hash.digest()));
                    size_bytes = uploaded.manifest.total_size as i64;
                    content_key_version = uploaded.content_key_version;
                    thumbnail = self.thumbnail_hash_for_reseal(&path, None, signer).await;
                } else if let Err(e) = fauna_core::file_download::fetch_manifest(
                    &self.blob_fetcher(),
                    &keys,
                    &manifest,
                    content_key_version,
                )
                .await
                {
                    // Content-keyed or plaintext: re-signed over the same
                    // manifest, and only once it opens under that key.
                    complete = false;
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(&path),
                        error = %format!("{e:#}"),
                        "succession take-over: a head whose manifest does not open; not carried \
                         this pass"
                    );
                    continue;
                }
            }

            let change_type = if is_delete {
                "delete"
            } else if head.change_type.eq_ignore_ascii_case("delete") {
                "modify"
            } else {
                head.change_type.as_str()
            };
            match self
                .record_change(
                    folder,
                    &path,
                    manifest_hex.as_deref(),
                    size_bytes,
                    change_type,
                    content_key_version,
                    thumbnail.as_deref(),
                    self.resolution_stamp(&path),
                )
                .await
            {
                Ok(seq) if seq > head.seq => {
                    rerecorded += 1;
                    if let (Some(_), Some(hex_hash)) = (entry, manifest_hex.as_deref())
                        && let Ok(manifest) = Self::parse_manifest_hash(hex_hash)
                        && let Err(e) = self.db.update_recorded_head(
                            &path,
                            &manifest,
                            size_bytes,
                            content_key_version,
                            &self.own_actor_id().0,
                        )
                    {
                        tracing::warn!(
                            path = %fauna_core::log_redact::log_path(&path),
                            error = %format!("{e:#}"),
                            "succession take-over: re-pointing the entry at its re-recorded head \
                             failed"
                        );
                    }
                }
                // The door answered with a row no newer than the head: it
                // swallowed the record. Nothing landed under the live nonce.
                Ok(seq) => {
                    complete = false;
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(&path),
                        seq,
                        head_seq = head.seq,
                        "succession take-over was answered with no new row; retried next pass"
                    );
                }
                Err(e) => {
                    complete = false;
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(&path),
                        error = %format!("{e:#}"),
                        "succession take-over record did not land; retried next pass"
                    );
                }
            }
        }
        if adopting
            && complete
            && let Some(nonce) = marker.as_ref()
        {
            self.db
                .set_adoption_state(nonce, crate::db::AdoptionState::Spent)?;
        }
        Ok(rerecorded)
    }

    /// Open a version a **retired identity of this account** signed and re-seal
    /// it under this engine's current owner root — the byte half of a version
    /// restore (`writer-signed-change-records.md` ruling (8)(d), the restore
    /// sentence; the opening form the Media restore shares, ruling (10)(b)).
    /// Answers the new manifest the restore records; records nothing itself.
    ///
    /// The bytes open under the signer's own retired root and its
    /// predecessors' alone ([`Self::record_signer_of`], ruling (8)(c)), so a
    /// version naming bytes the current identity (or a later predecessor)
    /// sealed — the planted row the rule exists for — does not open: nothing
    /// is uploaded, and the caller records nothing. One that opens is
    /// re-sealed as a chunk manifest under the current root, so the head the
    /// restore records opens under the current root *because it was
    /// re-sealed*, never because a signature was swapped.
    ///
    /// Refused outright, with the same words, as not this owner-root move: a
    /// version no retired identity of this account signed, a content-keyed
    /// set (its unstamped content is pre-bind, which only the current
    /// identity's own signature licenses), and a set whose bytes rest
    /// unsealed.
    pub async fn reseal_inherited_version(
        &self,
        path: &str,
        manifest_hash: ContentHash,
        signed_as: Option<[u8; 32]>,
    ) -> Result<ResealedVersion> {
        use fauna_core::file_download::RecordSigner;
        let refused = || anyhow::anyhow!(fauna_core::nest_reseal::RESTORE_INHERITED_UNOPENABLE);
        let path_r = fauna_core::log_redact::log_path(path);
        // The roots the version's signer may reach — its own and its
        // predecessors', never the current one. A caller holding a version
        // signed as the current identity re-points it verbatim and never
        // asks; any other writer is offered no owner root at all.
        let signer = signed_as.map_or(RecordSigner::Other, |a| self.record_signer_of(&a));
        if !matches!(signer, RecordSigner::Predecessor(_)) {
            return Err(refused());
        }
        // An owner-root move only: `effective_backup_key` is `None` for a
        // content-keyed set (and a keyless engine), which has no owner root to
        // re-seal under.
        if self.is_public_audience() || self.effective_backup_key().is_none() {
            return Err(refused());
        }
        // Its puts are a send to the nest (decision 2′ (c)).
        if let Some(hold) = self.publish_hold() {
            anyhow::bail!("re-seal of the restored version held back: {hold}");
        }

        // One column, two stores ([`Self::reseal_one_entry`]): the hash names a
        // chunk-store manifest for an engine-recorded file, a blob-store
        // primary for a Media-page upload. The manifest first — the shared
        // windowed move, opening under the signer's roots.
        let keys = fauna_core::file_download::FileDownloadKeys {
            record_signer: signer,
            ..self.download_keys()
        };
        let manifest_err = match self
            .reseal_nest_copy_windowed_under(path, manifest_hash, None, &keys)
            .await
        {
            Ok(uploaded) => {
                return Ok(ResealedVersion {
                    manifest_hash: uploaded.manifest_hash,
                    size_bytes: uploaded.manifest.total_size as i64,
                });
            }
            Err(e) => e,
        };

        // No manifest opened: a Media-page upload rests as one blob primary
        // under the bare owner key. Its content address is the only anchor
        // those bytes have (the frame carries no AAD), so it is checked before
        // anything opens; whatever the blob store answers that is not this
        // hash's blob, or opens under no root the signer may reach, is the
        // version that does not open.
        let plaintext = self
            .client
            .download_blob_opt(&manifest_hash)
            .await?
            .filter(|sealed| *blake3::hash(sealed).as_bytes() == manifest_hash.digest())
            .and_then(|sealed| {
                signer
                    .retired_keys(&self.predecessor_backup_keys)
                    .into_iter()
                    .filter_map(|key| key.client_key())
                    .find_map(|key| fauna_core::crypto::decrypt_backup_chunk(key, &sealed).ok())
            });
        let Some(plaintext) = plaintext else {
            tracing::warn!(
                path = %path_r,
                error = %format!("{manifest_err:#}"),
                "an inherited version did not open under a retired root; not restored"
            );
            return Err(refused());
        };
        let uploaded = self
            .upload_sealed(self.seal_for_upload(&plaintext)?, path, false)
            .await
            .with_context(|| format!("restoring {path_r}: uploading the re-sealed version"))?;
        Ok(ResealedVersion {
            manifest_hash: uploaded.manifest_hash,
            size_bytes: uploaded.manifest.total_size as i64,
        })
    }

    /// **One step** of [`Self::reseal_pending_under_current`] — re-seal the
    /// first owed row not already in `attempted`, add it there, and report
    /// whether a row was attempted (`false` = nothing left owed this pass).
    ///
    /// For a host that cannot give the pass its whole attention: a capability
    /// host (the File Provider extension) is control-inverted and must keep
    /// answering the OS, so it drives the pass one file between requests
    /// rather than blocking every `enumerate` behind a whole-corpus walk
    /// (`mls-group-key-material.md` § M2 → *Pre-bind re-seal migration*, part
    /// (D)). Same rows, same per-row stamp, so the two drives are
    /// interchangeable and a host killed mid-pass resumes where it stopped.
    /// `attempted` is the caller's, so a row whose record did not land is not
    /// retried in a tight loop within one drive; the next drive retries it.
    pub async fn reseal_next_pending_under_current(
        &self,
        attempted: &mut std::collections::HashSet<String>,
    ) -> Result<bool> {
        let Some(target_version) = self.prebind_reseal_target()? else {
            return Ok(false);
        };
        // The heads are read once per drive: the first step (an empty
        // `attempted`) reads them, every later step of the drive reuses them.
        if attempted.is_empty() {
            *self
                .prebind_heads_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(self.prebind_heads().await?);
        }
        let heads = self
            .prebind_heads_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        let Some(entry) = self
            .prebind_owed_rows(target_version, &heads)?
            .into_iter()
            .find(|e| !attempted.contains(&e.path))
        else {
            return Ok(false);
        };
        attempted.insert(entry.path.clone());
        // Unlike the whole pass, one row's failure does not end the drive: a
        // row whose nest copy will not open (a withheld chunk, a generation not
        // yet synced) would otherwise be the first row of every later drive
        // too, and nothing behind it would ever move. It stays unstamped, so
        // the next drive retries it. `Err` is reserved for "no pass can run
        // here" (the seal root itself failed closed).
        if let Err(e) = self.reseal_prebind_row(&entry.path, target_version).await {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(&entry.path),
                error = %format!("{e:#}"),
                "pre-bind re-seal step: this row stays owed; continuing with the next"
            );
        }
        Ok(true)
    }

    /// The generation the pre-bind pass re-seals under, or `None` when there is
    /// no pass to run.
    fn prebind_reseal_target(&self) -> Result<Option<u64>> {
        // A `public`-audience folder rests plaintext by ratified design
        // (phase 4) — a pass run while it is public must not seal the site
        // dark. Nothing is lost by returning early: the pass re-runs on every
        // engine start and picks the rows up once the audience flips back.
        if self.is_public_audience() {
            return Ok(None);
        }
        Ok(match self.content_seal_root()? {
            Some((_, Some(v))) => Some(v),
            // Unbound / plaintext / owner-only (no content-key generation to seal
            // under) — nothing to re-seal.
            _ => None,
        })
    }

    /// The rows the pre-bind pass still owes a re-seal under `target_version`.
    ///
    /// Every live row whose recorded head is the nest head — materialized
    /// (`Synced`) and, on the set owner's engine, cloud-only (`Placeholder`)
    /// too: part (D) (`mls-group-key-material.md` § M2 → *Pre-bind re-seal
    /// migration*). An owner whose replicas are all on-demand holds only
    /// placeholders, and walking materialized rows alone moved nothing for it.
    /// A placeholder resolves to the nest-bytes source in
    /// `reseal_path_under_current` (bounded, never hydrated), and its unstamped
    /// pre-bind record opens under the owner root only because this engine is
    /// the owner's. A member's engine keeps the materialized walk: an unstamped
    /// placeholder there is the owner's pre-bind content, which it can neither
    /// open nor is its to move.
    fn prebind_owed_rows(
        &self,
        target_version: u64,
        heads: &PrebindHeads,
    ) -> Result<Vec<crate::db::SyncEntry>> {
        let mut rows = self.db.list_by_state(SyncState::Synced)?;
        // Cloud-only rows are the owner's to move (a member's engine keeps the
        // materialized walk).
        if self.owns_set() {
            rows.extend(self.db.list_by_state(SyncState::Placeholder)?);
        }
        rows.retain(|e| {
            if e.content_key_version == Some(target_version) {
                return false;
            }
            match e.manifest_hash.as_ref() {
                // A materialized row with no recorded head is this disk's own
                // content — no nest record exists that could have planted it.
                None => e.state == SyncState::Synced,
                // Only a row whose nest head the reader admitted moves (ruling
                // (5): a plaintext head too — it "re-seals iff its record
                // verifies"). A materialized row re-seals from this disk and
                // opens nothing; a cloud-only UNSTAMPED row is sourced from the
                // nest under the owner root, which only this account's own
                // verified record of this set is offered — skip the rest here
                // rather than fail the pass on them.
                Some(m) => {
                    heads.admits(&e.path, m)
                        && (e.state == SyncState::Synced
                            || e.content_key_version.is_some()
                            || self.record_is_owner_signed(m))
                }
            }
        });
        Ok(rows)
    }

    /// The set's current nest heads with the reader's verdict on each — what
    /// the pre-bind pass gates its rows on (ruling (5): the pass re-seals
    /// only rows that verify). One verified fetch; it also fills
    /// [`Self::owner_signed_manifests`], which the pass's nest-bytes reads
    /// need.
    async fn prebind_heads(&self) -> Result<PrebindHeads> {
        // This crate's unit tests run with the WS-RPC control plane
        // unconnected, so no head can be fetched there: they pin the pass's
        // byte plane, and the gate is pinned against a real nest
        // (`bins/fauna-nest/tests/conformance_sync_engine_record_commit.rs`).
        #[cfg(test)]
        return Ok(PrebindHeads {
            admit_all: true,
            ..Default::default()
        });
        #[allow(unreachable_code)]
        let Some(folder) = self.folder.clone() else {
            return Ok(PrebindHeads::default());
        };
        let mut heads: HashMap<String, (i64, Option<String>)> = HashMap::new();
        for row in self.fetch_changes(&folder, 0).await? {
            let Some(path) = row.path.clone() else {
                continue;
            };
            if row.is_retention == Some(true) {
                continue;
            }
            match heads.get(&path) {
                Some((seq, _)) if *seq >= row.seq => {}
                _ => {
                    heads.insert(path, (row.seq, row.manifest_hash.clone()));
                }
            }
        }
        Ok(PrebindHeads {
            heads: heads
                .into_iter()
                .filter_map(|(path, (_, m))| {
                    Some((path, Self::parse_manifest_hash(m.as_deref()?).ok()?))
                })
                .collect(),
            admit_all: false,
        })
    }

    /// Re-seal one owed row under `target_version`, logging a record that did
    /// not land (sealed bytes up, nest head unchanged — the path stays stale at
    /// rest and the next pass retries it).
    async fn reseal_prebind_row(
        &self,
        path: &str,
        target_version: u64,
    ) -> Result<ResealDisposition> {
        let disposition = self
            .reseal_path_under_current(path, Some(target_version), ResealSource::LocalFile)
            .await?;
        if disposition == ResealDisposition::Unrecorded {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(path),
                "re-seal under current: the re-sealed copy uploaded but its change record \
                 did not land; the nest head is unchanged, so the path stays stale at rest"
            );
        }
        Ok(disposition)
    }

    /// The **owner-only plaintext re-seal** — the unbound (public → private)
    /// flip-back arm of [`Self::converge_corpus_to_audience`], and the
    /// owner-only twin of [`Self::reseal_pending_under_current`], sharing the
    /// same force-upload → verify → best-effort-supersede trio: every live head
    /// recorded during a public window rests as a plaintext manifest (no
    /// `stored_hashes`); this pass re-uploads each through the sealed path
    /// (idempotent — the convergent seal yields byte-identical ciphertext on
    /// re-run), whole-file-verifies the sealed copy, then
    /// best-effort-supersedes the plaintext rows so nest GC reclaims their
    /// chunks. (Its once-per-start run as a migration of a pre-seal plaintext
    /// back-catalogue was retired by the compat-remnant sweep —
    /// `version-compatibility.md` § Dimension 2.)
    ///
    /// A no-op for a bound set (the pre-bind pass owns that migration) and for
    /// a keyless engine (nothing to seal under — the upload guard already
    /// fails such an engine closed). **Termination:** the per-entry filter is
    /// the recorded manifest itself — a small, content-addressed fetch per
    /// not-yet-checked entry, and entries whose manifest already carries
    /// `stored_hashes` are marked done in the local `SyncDb`
    /// (`mark_owner_sealed`), so steady-state passes are one indexed query and
    /// zero network. Device-local + additive + idempotent: a lost marker
    /// merely re-checks (convergent re-upload dedups to the identical blob).
    pub async fn reseal_owner_only_plaintext(&self) -> Result<usize> {
        if self.is_public_audience() {
            // A `public`-audience folder's whole corpus IS the plaintext shape
            // this pass exists to migrate away — deliberately, by the owner's
            // ratified declassification (phase 4). Running it here would seal
            // the site dark on every catch-up pass, fighting the audience
            // forever.
            return Ok(0);
        }
        if self.mls_group_id.is_some() {
            return Ok(0); // bound set — the pre-bind re-seal owns it
        }
        if self.effective_backup_key().is_none() {
            return Ok(0); // keyless — nothing to seal under (uploads fail closed)
        }
        let mut resealed = 0usize;
        let mut unrecorded = 0usize;
        for entry in self.db.list_by_state(SyncState::Synced)? {
            if entry.owner_sealed {
                continue;
            }
            let Some(manifest_hash) = entry.manifest_hash else {
                continue; // never uploaded — nothing at rest to re-seal
            };
            // One small fetch decides this entry once: already sealed ⇒ mark
            // done; plaintext ⇒ re-seal, verify, supersede, then mark.
            let manifest = self
                .fetch_manifest(&manifest_hash, entry.content_key_version)
                .await?;
            if manifest.stored_hashes.is_none() {
                let disposition = self
                    .reseal_path_under_current(&entry.path, None, ResealSource::LocalFile)
                    .await?;
                if disposition == ResealDisposition::Nothing {
                    continue; // neither on this disk nor on the nest
                }
                if disposition == ResealDisposition::Unrecorded {
                    // ⚠ The marker is gated on the RECORD, not the upload — the
                    // same reason the post-succession re-seal gates its own
                    // sentinel. `owner_sealed` means "this path's manifest is
                    // known sealed"; while the nest head still names the
                    // plaintext manifest that is false, and marking it would
                    // strand the path plaintext forever (the mark-gated pass
                    // skips it on every later run).
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(&entry.path),
                        "owner-only re-seal: the re-sealed copy uploaded but its change record \
                         did not land; the nest head is unchanged, so the entry stays owed"
                    );
                    unrecorded += 1;
                    continue;
                }
                resealed += 1;
            }
            self.db.mark_owner_sealed(&entry.path)?;
        }
        Self::require_fully_recorded("owner-only re-seal", resealed, unrecorded)?;
        Ok(resealed)
    }

    /// The **declassify pass** (folders re-model phase 4 — `folders.md`
    /// § Target re-model; the public exception is `principles.md` § The user
    /// always controls their data): move a `public`-audience folder's SEALED
    /// back-catalogue to the world-readable plaintext shape, so the whole site
    /// serves — not just files uploaded after the flip.
    ///
    /// [`Self::reseal_owner_only_plaintext`] INVERTED: same walk, same
    /// force-upload → verify → best-effort-supersede trio
    /// ([`Self::reseal_path_under_current`], whose upload leg takes the
    /// engine's armed public arm and therefore re-records plaintext), opposite
    /// per-entry predicate (`stored_hashes` **present** ⇒ re-record). Runs
    /// ONLY on a public-armed engine — an engine still carrying the sealed
    /// posture would "declassify" by re-uploading sealed bytes, a no-op that
    /// would then be recorded as done.
    ///
    /// Two marker duties, both load-bearing:
    /// - every re-recorded path's `owner_sealed` marker is CLEARED — it meant
    ///   "this path's manifest is known sealed", which just stopped being
    ///   true, and the flip-back re-seal skips marked entries (a survivor
    ///   would strand the path plaintext after the owner re-seals);
    /// - this pass keeps NO done-markers of its own — its steady-state gate is
    ///   folder-level ([`Self::converge_corpus_to_audience`]'s
    ///   `corpus_audience` meta row), written only after a clean pass.
    ///
    /// Reads need no special casing: the engine keeps its `backup_key` /
    /// `content_keys` while public (deliberately — see the field docs), so a
    /// sealed entry's local file is simply read from disk (`LocalFile`); the
    /// keys still open anything the verify leg must fetch. **The `NestBytes`
    /// widening this comment used to name as a someday-precedent is BUILT
    /// (ccxliii, 2026-08-21)** and lives in the shared trio, so an entry with no
    /// local bytes is fetched from the nest rather than deferred — and, far more
    /// importantly, rather than aborting the walk and taking every entry behind
    /// it down too. The old text declared *one* entry not converging; what the
    /// code actually did was let that entry decide the whole pass.
    pub async fn declassify_owner_corpus(&self) -> Result<usize> {
        if !self.is_public_audience() {
            return Ok(0); // sealed posture — nothing may be unsealed
        }
        let mut declassified = 0usize;
        let mut unrecorded = 0usize;
        for entry in self.db.list_by_state(SyncState::Synced)? {
            let Some(manifest_hash) = entry.manifest_hash else {
                continue; // never uploaded — nothing at rest to declassify
            };
            let manifest = self
                .fetch_manifest(&manifest_hash, entry.content_key_version)
                .await?;
            if manifest.stored_hashes.is_some() {
                let disposition = self
                    .reseal_path_under_current(&entry.path, None, ResealSource::LocalFile)
                    .await?;
                if disposition == ResealDisposition::Nothing {
                    continue; // neither on this disk nor on the nest
                }
                if disposition == ResealDisposition::Unrecorded {
                    // The plaintext bytes are up but the nest's head still names
                    // the SEALED manifest, so this path is not declassified —
                    // the marker stays true and the walk goes on (one refused
                    // path must not shield the rest), but the pass reports the
                    // shortfall so its caller withholds the `corpus_audience`
                    // stamp and the next tick re-drives.
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(&entry.path),
                        "declassify: the plaintext copy uploaded but its change record did not \
                         land; the nest head is unchanged, so the path stays sealed at rest"
                    );
                    unrecorded += 1;
                    continue;
                }
                self.db.clear_owner_sealed(&entry.path)?;
                declassified += 1;
            }
        }
        Self::require_fully_recorded("declassify", declassified, unrecorded)?;
        Ok(declassified)
    }

    /// The shared refusal arm of the three corpus passes — the discipline the
    /// post-succession re-seal has always applied per path
    /// ([`ResealOutcome::StillOwed`]), lifted to the pass level for phase 4's
    /// audience convergence (2026-08-21).
    ///
    /// **Why an `Err` rather than a short count.** A pass whose change record
    /// was refused did not move the corpus: the nest's head still names the old
    /// manifest, so every other device — and this one after a dehydration —
    /// still reads the old shape. The one observable that must not happen is
    /// [`SyncEngine::converge_corpus_to_audience`] stamping `corpus_audience`,
    /// because its `current == target` short-circuit makes that stamp permanent:
    /// no later pass ever retries. That caller already treats an error as
    /// "leave the meta row unmoved so the next catch-up re-drives" — this is
    /// what makes the refusal reach it.
    ///
    /// Found by the member-seat e2e: a writer member's declassify was
    /// refused `stale_content_key` by a nest floor gate with no public arm, the
    /// refusal was swallowed as a WARN inside the upload path, and the pass
    /// stamped the corpus converged anyway — one layer down.
    fn require_fully_recorded(pass: &str, recorded: usize, unrecorded: usize) -> Result<()> {
        if unrecorded == 0 {
            return Ok(());
        }
        anyhow::bail!(
            "{pass}: {unrecorded} of {} path(s) uploaded but their change records did not land \
             — the corpus has NOT converged; retrying next pass",
            recorded + unrecorded
        )
    }

    /// Converge this folder's at-rest corpus to its CURRENT audience — the
    /// phase-4 catch-up step that makes an audience flip real for bytes
    /// recorded before it (`folders.md` § Target re-model: the nest records
    /// only the state flip; the corpus moves client-side).
    ///
    /// **The projected audience itself is the cross-device signal** — every
    /// device's engine is built from the folder list, so no custody sentinel
    /// is needed: each device converges independently, and the convergent
    /// seal + store dedup make concurrent converging devices race benignly.
    /// The steady-state gate is one `meta` row (`corpus_audience`), written
    /// only after a clean pass so a crash re-drives; **absent reads as
    /// `"sealed"`** — a folder that never flipped audience — so the
    /// fleet pays no walk for folders whose audience never flipped.
    ///
    /// Directions:
    /// - target **plaintext** (the engine is public-armed):
    ///   [`Self::declassify_owner_corpus`];
    /// - target **sealed** (flip-back): drop every `owner_sealed` marker first
    ///   — markers set before the public window (or on another schedule than
    ///   the declassify that cleared its own) are stale-true and would make
    ///   the re-seal skip plaintext paths forever — then re-seal by bound-ness:
    ///   a BOUND engine (public → shared) via
    ///   [`Self::reseal_pending_under_current`], whose version-mismatch filter
    ///   selects exactly the public-window plaintext rows
    ///   (`content_key_version: None`) and whose supersede leg already
    ///   tolerates a member seat; an unbound one (public → private) via
    ///   [`Self::reseal_owner_only_plaintext`]. Both arms ride the projection
    ///   alone — the remedy: the custody sentinel is per-actor, so it
    ///   structurally cannot reach a member's engine, and a member engine
    ///   that armed the public window holds public-window plaintext exactly
    ///   as the owner does (audience rides both projection arms; a member
    ///   engine with no anchor never arms and has nothing to move —
    ///   `encryption-at-rest.md` § Implementation status today). The
    ///   `corpus_audience` stamp lands only after
    ///   the chosen pass returns cleanly — an error leaves the row unmoved so
    ///   the next catch-up re-drives.
    pub async fn converge_corpus_to_audience(&self) -> Result<usize> {
        let target = if self.is_public_audience() {
            "plaintext"
        } else {
            "sealed"
        };
        let current = self.db.corpus_audience()?;
        if current.as_deref().unwrap_or("sealed") == target {
            return Ok(0);
        }
        let n = if self.is_public_audience() {
            self.declassify_owner_corpus().await?
        } else {
            self.db.clear_all_owner_sealed()?;
            if self.mls_group_id.is_some() {
                self.reseal_pending_under_current().await?
            } else {
                self.reseal_owner_only_plaintext().await?
            }
        };
        self.db.set_corpus_audience(target)?;
        Ok(n)
    }

    /// Converge this folder's **`web_files` projection** to its current website
    /// toggle — the client-driven half of the enable-time backfill, and the
    /// only half a SEALED folder can have (`web-content-hosting.md` § Content
    /// model).
    ///
    /// **Why the nest cannot do this.** `web_files` is a projection of the
    /// folder's live sync heads, and `folder_handlers`' `update_handler`
    /// rebuilds it on any serving transition landing enabled — but only from
    /// the folder's **plaintext-resting** heads, because a sealed head rests
    /// `path = NULL` (S9: the nest holds no names). So a sealed folder synced
    /// first and toggle-enabled second serves only files recorded *after* the
    /// toggle, and a finished static site never re-records. The back-catalogue
    /// reaches `web_files` exactly one way: a client re-records it while the
    /// toggle is on, and sync-time routing
    /// (`web_files_projection::route_web_file_change`) ingests each arrival with
    /// its sealed `content_key_version` intact.
    ///
    /// **Shape: [`Self::converge_corpus_to_audience`]'s twin**, for the same
    /// reasons. The projected toggle is the cross-device signal — every
    /// device's engine reads it off the same `fauna.folders.list` tick as the
    /// mode ([`crate::config::SeatResolution::website_enabled`]), so there is
    /// no sentinel to stage or lose, and concurrent converging devices race
    /// benignly (convergent seal + store dedup ⇒ byte-identical uploads, and
    /// the projection upsert is idempotent per `(actor, path)`). The
    /// steady-state gate is one `meta` row (`corpus_website`), written only
    /// after a clean pass so a crash re-drives; **absent reads as `"off"`** —
    /// a folder that never enabled a website — so the fleet
    /// pays one meta-row read per tick for folders that serve no website.
    ///
    /// **Only a sealed folder is this pass's business.** A `public`-audience
    /// folder rests plaintext, which is precisely the class the nest-side
    /// backfill folds, so re-recording its corpus here would duplicate that
    /// work on every device for nothing. Hence the target is `"served"` only
    /// when the toggle is on AND the engine is not public-armed; every other
    /// combination converges to `"off"`, which is zero work plus one stamp.
    ///
    /// **A keyless engine leaves the marker unmoved.** It cannot upload at all
    /// (the seal sites fail closed), so stamping `"served"` would record a walk
    /// that never happened and never retry it. Returning early instead means
    /// the next tick re-drives once the keys arrive.
    pub async fn converge_corpus_to_website(&self) -> Result<usize> {
        let target = if self.is_website_enabled() && !self.is_public_audience() {
            "served"
        } else {
            "off"
        };
        let current = self.db.corpus_website()?;
        if current.as_deref().unwrap_or("off") == target {
            return Ok(0);
        }
        let n = if target == "served" {
            // Keyless ⇒ nothing can be re-recorded (every seal site fails
            // closed), so leave the marker UNMOVED rather than record a walk
            // that never happened — the next catch-up re-drives once the keys
            // arrive. The question has two halves because the seal root does:
            // a bound set's root is `content_seal_root` (which itself bails
            // loudly when bound-but-keyless, leaving the marker unmoved for the
            // same reason), while an unbound owner-only set has none there and
            // seals under the `BackupKey` at the call sites.
            if self.content_seal_root()?.is_none() && self.effective_backup_key().is_none() {
                return Ok(0);
            }
            let live = self.reissue_corpus_for_web().await?;
            let n = live.len();
            // **the sentence the walk cannot say by itself.** The
            // re-records above are additive: they tell the nest what EXISTS,
            // never what stopped existing. A sealed head rests no plaintext
            // name (S9), so the nest's own reconcile deliberately skips sealed
            // `web_files` rows — its live set could not name them — and a path
            // deleted while the toggle was OFF therefore kept a row that kept
            // SERVING after re-enable: a delete that does not take effect on a
            // published surface. Only this side can enumerate that class, so
            // here it does, once, straight after a walk that has already
            // refused to report a partial result.
            self.declare_live_web_corpus(live).await;
            n
        } else {
            0
        };
        self.db.set_corpus_website(target)?;
        Ok(n)
    }

    /// Tell the nest the complete live path set of this folder's website corpus
    /// (`fauna.web.files.prune_sealed`) so it can drop the sealed `web_files`
    /// rows outside it.
    ///
    /// **Best-effort on purpose.** The walk it follows has already landed; a
    /// failed declaration leaves exactly the pre-fix residual rather than
    /// anything new, and taking the marker anyway is right — the corpus IS
    /// served, which is what the marker records. The next serving transition
    /// re-declares.
    ///
    /// **Never a partial set.** `reissue_corpus_for_web` fails loudly rather
    /// than returning a short list, so the only way to under-declare is an
    /// oversized payload — and there the answer is to send **nothing**, because
    /// a truncated set deletes live content. That degrades to today's residual;
    /// a truncated one would delete the site.
    async fn declare_live_web_corpus(&self, live: Vec<String>) {
        let Some(folder) = self.folder.clone() else {
            return; // unbound engine — no folder to reconcile
        };
        // ⚠ **An EMPTY set is never declared, and that is the load-bearing
        // guard.** "This folder has no live heads" and "this device has not
        // caught up yet" are the same observation from here, and they are not
        // the same fact. A second device binding the folder starts with an
        // empty state DB and an absent `corpus_website` marker — so its very
        // first convergence would walk nothing, declare nothing-is-live, and
        // take the whole site down. Nothing else in this pass can cause that:
        // a short set is impossible (`reissue_corpus_for_web` fails loudly
        // rather than returning one), so the empty case is the only way to
        // over-claim, and refusing it is the additive-everywhere reading —
        // silence must never mean *delete everything*.
        //
        // The bounded cost, stated honestly: an owner who deletes EVERY page of
        // a sealed site while the toggle is off keeps those rows until they add
        // one back (which re-declares and prunes) or the folder stops serving.
        // That is the pre-fix residual, surviving in exactly the one
        // shape where the alternative risks the opposite mistake — and the
        // opposite mistake destroys a live site.
        if live.is_empty() {
            tracing::debug!(
                "web corpus declaration skipped: nothing live to declare (an empty walk is \
                 indistinguishable from a device that has not caught up)"
            );
            return;
        }
        let budget = web_declaration_bytes(&live);
        if budget > fauna_protocol::web::MAX_PRUNE_SEALED_PATH_BYTES {
            tracing::warn!(
                paths = live.len(),
                budget,
                "web corpus declaration skipped: the live path set does not fit one RPC frame, \
                 and a truncated set would delete live content — a sealed path deleted while \
                 the website toggle was off keeps serving until the next transition"
            );
            return;
        }
        let req =
            fauna_protocol::folders::addressed(fauna_protocol::web::WebFilesPruneSealedRequest {
                folder,
                paths: live,
                ..Default::default()
            });
        let control = Arc::clone(&*self.control.read().unwrap());
        match control.prune_sealed_web_files(req).await {
            Ok(0) => {}
            Ok(dropped) => tracing::info!(
                dropped,
                "web corpus declaration: the nest dropped sealed web_files rows this corpus no \
                 longer holds"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                "web corpus declaration failed; sealed rows for deleted paths may keep serving \
                 until the next serving transition"
            ),
        }
    }

    /// [`Self::converge_corpus_to_website`]'s walk: re-record every live head
    /// as a **no-novel-content reissue**, so the nest's sync-time routing lands
    /// each one in `web_files`.
    ///
    /// Borrows the migration trio wholesale
    /// ([`Self::reseal_path_under_current`]: force upload → verify → best-effort
    /// supersede) — the same primitive the declassify and pre-bind passes use.
    /// Nothing is *re-sealed* here: the folder's seal is unchanged, so the
    /// convergent chunk ciphertext is byte-identical and the store dedups it;
    /// what the pass is actually buying is the **change record**, which is the
    /// only event that reaches `route_web_file_change`. `upload_file_inner`
    /// stamps the record `proven_reissue` (its `recorded_content_hash` witness
    /// matches), so it rides the wire as `is_resolution = true` and a receiver
    /// whose frontier already passed these bytes skips it instead of merging it
    /// from a stale ancestor (`conflicts.md` § Concurrent resolution) — a
    /// re-record of the back-catalogue must not read as an edit storm.
    ///
    /// The verify's `target_version` is this engine's current seal generation —
    /// `None` for an owner-only set, which has no generation (its chunks rest
    /// under the `BackupKey`) — so the check proves the head the pass just
    /// published opens under what the folder is sealed under now.
    ///
    /// Entries with no recorded manifest are skipped (never uploaded — nothing
    /// for the projection to point at), and a path the nest refuses for its
    /// extension (`.php` &c.) is re-recorded like any other: the refusal is the
    /// nest's, at `route_web_file_change`'s own guard, and duplicating that
    /// list client-side would be a second owner for one rule.
    /// Returns the paths it re-recorded — the folder's complete live website
    /// corpus, which [`Self::declare_live_web_corpus`] then declares. The list
    /// is deliberately the pass's OWN product rather than a re-read of the DB:
    /// a path skipped here (no manifest, or resolvable on neither side) has no
    /// head for the projection to name, so it must not be declared live — the
    /// same rule the nest's plaintext reconcile applies to its own class.
    async fn reissue_corpus_for_web(&self) -> Result<Vec<String>> {
        let target_version = self.content_seal_root()?.and_then(|(_, v)| v);
        let mut reissued: Vec<String> = Vec::new();
        let mut unrecorded = 0usize;
        for entry in self.db.list_by_state(SyncState::Synced)? {
            if entry.manifest_hash.is_none() {
                continue; // never uploaded — no head for the projection to name
            }
            let disposition = self
                .reseal_path_under_current(&entry.path, target_version, ResealSource::LocalFile)
                .await?;
            if disposition == ResealDisposition::Nothing {
                continue; // neither on this disk nor on the nest
            }
            if disposition == ResealDisposition::Unrecorded {
                // The re-record IS this pass's product — sync-time routing is
                // what lands the head in `web_files`. A refused record means
                // the projection did not gain this path, so the pass reports
                // the shortfall and `converge_corpus_to_website` withholds its
                // marker rather than claiming the back-catalogue is served.
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(&entry.path),
                    "web reissue: the head re-uploaded but its change record did not land; the \
                     web_files projection did not gain this path"
                );
                unrecorded += 1;
                continue;
            }
            reissued.push(entry.path.clone());
        }
        Self::require_fully_recorded("web reissue", reissued.len(), unrecorded)?;
        Ok(reissued)
    }

    /// **Owner-root corpus convergence** — the shared engine for TWO retired-root
    /// triggers that both answer the identical question ("does this entry's
    /// manifest open under my CURRENT effective owner root, or does it still
    /// rest under one I hold only as a retired read candidate?"): the
    /// **post-succession** re-seal (`succession-aftermath.md` § Re-key scope,
    /// the `BackupKey` corpus row: the seal re-keys *"client-driven and
    /// urgent"*) and the **WebDAV serve-disable** re-seal (`webdav-server.md`
    /// § Key model, Revocation ). Both share this one
    /// walk rather than two copies (priority #2/#4: the mechanism does not
    /// care *why* a root retired, only that [`Self::effective_backup_key`]
    /// no longer names it) — the successor's twin of
    /// [`Self::reseal_owner_only_plaintext`], sharing its force-upload → verify →
    /// best-effort-supersede trio, its per-entry filter + `SyncDb` sentinel shape,
    /// and its device-local + additive + idempotent semantics.
    ///
    /// **What each trigger repairs.** A succession re-points corpus *ownership*
    /// in one nest-side transaction but moves no *seal*, so every chunk already
    /// at rest stays sealed under the **predecessor's** `BackupKey` while the
    /// successor derives a new one. Disabling a group-less served set's WebDAV
    /// flag rotates its content key (`FoldersAuthor::serve_disable`) and drops
    /// it from the engine's *live* binding, so every chunk sealed during the
    /// served window stays under that **retired M2 generation** while the
    /// disabled engine now runs owner-only. Both reads are fixed the same way
    /// ([`fauna_core::file_download::FileDownloadKeys::predecessor_backup_keys`]
    /// / [`fauna_core::file_download::FileDownloadKeys::retired_content_keys`]
    /// offer the retired roots as read candidates); this is the write half that
    /// ends the window, after which the retired key stops being load-bearing —
    /// the device-loss race § Re-key scope's ratified blockquote describes (and
    /// the WebDAV twin of it: nothing else ever re-seals a disabled set's
    /// served-era back-catalogue, so without this pass those files stay
    /// readable only by whoever still holds the rotated-out generation).
    ///
    /// **The per-entry filter is "which root opens it", not "is it sealed".**
    /// That is the one thing this pass cannot borrow from its plaintext-migration
    /// sibling: every sealed-chunk manifest is *also* sealed (its hashes ride in
    /// `sealed_hashes` under the root that sealed its chunks), and the reader
    /// opens it under every candidate root — current and retired alike — so
    /// manifest readability discriminates nothing, for an owner-only set or the
    /// WebDAV case. The chunk bodies are what carry the seal, so the filter
    /// fetches **one chunk** and asks whether it opens under the current root
    /// alone. Hence a distinct sentinel ([`crate::db::SyncDb::mark_current_root_sealed`])
    /// rather than a reuse of `owner_sealed`: the two answer different
    /// questions, and collapsing them would mark a retired-root-sealed entry
    /// done.
    ///
    /// **Zero cost for everyone else.** Empty `predecessor_backup_keys` AND no
    /// `retired_content_keys` — every identity that never succeeded, on a set
    /// that was never served — returns before touching the DB, so the
    /// overwhelmingly common fleet pays two branches. The pass is likewise a
    /// no-op for a bound set (whose chunks never rest under an owner root;
    /// FS-5DC) and for a keyless engine.
    ///
    /// **Both plaintext sources, so a fresh device is covered.** A path this
    /// disk holds moves by force-upload ([`ResealSource::LocalFile`]); anything
    /// else — a cloud-only placeholder, a media-library item, and in the case
    /// the aftermath actually exists for, a successor restoring onto a fresh
    /// device where *nothing* is materialized — moves by
    /// [`ResealSource::NestBytes`], whose plaintext source is the predecessor-key
    /// read the write half was always the other half of.
    ///
    /// ⚠ **The sentinel is gated on the change RECORD, not on the upload.** An
    /// entry whose re-sealed copy uploaded but whose record did not land is left
    /// **unmarked**, because the nest's head still names the predecessor-sealed
    /// manifest: every other device would still hydrate the retired-root copy.
    /// So [`crate::db::SyncDb::list_pending_current_root_reseal`] draining to
    /// empty means "no path's *head* rests under a retired root", which is the
    /// only statement strong enough to license `sync-agent.md` bound (3).
    ///
    /// ⚠ **This alone still does not discharge the capability's retired keys.**
    /// Media **thumbnails** are a separate seal that `FileDownloadKeys` does not
    /// reach (`fauna-media-machine`'s raw framed `decrypt_backup_chunk` under the
    /// bare `BackupKey`), so bound (3) waits on that plane too — see
    /// `sync-agent.md` § Implementation status today → A8.
    ///
    /// **The pass reports itself.** § Re-key scope also requires the re-seal be
    /// *"surfaced with progress, resumed until complete"*, and on desktop the
    /// surface is in a different process — so the pass records what it is doing
    /// into its own `SyncDb` ([`crate::succession_progress::CorpusResealPass`])
    /// rather than returning it to a caller who would then have to re-derive it.
    /// A caller that re-ran the pass to render a status line would race the very
    /// work it is reporting on. The record is written at entry and overwritten
    /// at exit; a write failure is logged and swallowed, because a progress
    /// report that cannot be filed must never take down the re-seal it reports
    /// on. ⚠ It is a *report*, never the completion proof — bound (3) is
    /// enforced on the drained [`crate::db::SyncDb::list_pending_current_root_reseal`],
    /// never on a count from here.
    pub async fn reseal_predecessor_sealed(&self) -> Result<usize> {
        // FIRST, and deliberately ahead of every early return: bind this DB's
        // `current_root_sealed` sentinels to the identity now serving them,
        // clearing them when it changed (`crate::succession_drain`, ruling 5).
        //
        // It lives *here*, at the head of the pass that consumes the sentinels,
        // rather than in the two drive loops that call the pass — a guard the
        // caller must remember is a guard that can be unwired, and unwiring this
        // one produces a silently **vacuous** drain rather than a visible
        // failure. Both drive shapes already run this pass, so the guard reaches
        // both by construction and neither has to know it exists.
        self.adopt_sentinel_root_generation();
        if self.served_era_hold {
            // Rule (5)'s hold: a set served before the serve stamps existed
            // reads not served until the owner's one flip ON, which walks the
            // back-catalogue onto content keys again. Walking it onto the
            // owner root first would be a re-seal undone by the next one.
            return Ok(0);
        }
        if self.predecessor_backup_keys.is_empty() && self.retired_content_keys.is_none() {
            // Never succeeded AND never served — neither trigger applies, so
            // the whole pass is inapplicable.
            return Ok(0);
        }
        if self.is_public_audience() {
            // A `public`-audience folder rests plaintext by ratified design
            // (phase 4): there is no owner seal to move to the successor root,
            // and force-uploading through the sealed path would seal the site
            // dark. (A predecessor-sealed back-catalogue in a *declassified*
            // folder is the declassify pass's business, not succession's.)
            return Ok(0);
        }
        let Some(current) = self.effective_backup_key() else {
            // Bound set (its chunks rest under content keys, not an owner root)
            // or keyless. Either way there is no owner seal here to move.
            return Ok(0);
        };
        self.record_reseal_pass(&crate::succession_progress::CorpusResealPass::Running);
        let result = self.reseal_predecessor_sealed_inner(current).await;
        self.record_reseal_pass(&match &result {
            Ok((resealed, owed)) => crate::succession_progress::CorpusResealPass::Settled {
                resealed: *resealed as u64,
                owed: *owed as u64,
            },
            Err(e) => crate::succession_progress::CorpusResealPass::Failed {
                reason: format!("{e}"),
            },
        });
        result.map(|(resealed, _)| resealed)
    }

    /// File one progress record, best-effort. Separate from the pass so the
    /// swallow is written once and is visible as a decision rather than as a
    /// stray `let _ =`.
    fn record_reseal_pass(&self, pass: &crate::succession_progress::CorpusResealPass) {
        if let Err(e) = self.db.record_corpus_reseal_pass(pass) {
            tracing::warn!(
                error = ?e,
                "owner-root corpus re-seal: recording pass progress failed; the pass itself is \
                 unaffected and the surface simply shows the previous reading"
            );
        }
    }

    /// [`Self::reseal_predecessor_sealed`]'s body, returning **both** counts so
    /// the caller can file them. Split out so the progress record is written on
    /// every exit path — including the error one — without threading a guard
    /// through the loop.
    async fn reseal_predecessor_sealed_inner(
        &self,
        current: &fauna_core::crypto::OwnerSealKey,
    ) -> Result<(usize, usize)> {
        let current_root = current.convergent_chunk_root();

        let mut resealed = 0usize;
        let mut owed = 0usize;
        for entry in self.db.list_pending_current_root_reseal()? {
            // Contained per entry, deliberately. One un-fetchable path (chunks
            // swept, no held root opens it) must not abort the pass and strand
            // every entry behind it under a retired root — a repair pass that
            // stops at its first casualty repairs the least when it matters
            // most. A contained failure leaves the entry unmarked, so the
            // completion observable keeps reporting it and the next catch-up
            // retries.
            match self.reseal_one_entry(&entry, &current_root).await {
                Ok(ResealOutcome::AlreadyCurrent) => {}
                Ok(ResealOutcome::Resealed) => resealed += 1,
                Ok(ResealOutcome::StillOwed) => owed += 1,
                Err(e) => {
                    owed += 1;
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(&entry.path),
                        error = ?e,
                        "owner-root corpus re-seal: entry failed; still owed, will retry next pass"
                    );
                }
            }
        }
        if owed > 0 {
            tracing::info!(
                resealed,
                owed,
                "owner-root corpus re-seal pass finished with entries still owed"
            );
        }
        Ok((resealed, owed))
    }

    /// One entry's re-seal decision + execution — [`Self::reseal_predecessor_sealed`]'s
    /// body, split out so a single entry's failure is containable.
    async fn reseal_one_entry(
        &self,
        entry: &crate::db::SyncEntry,
        current_root: &[u8; 32],
    ) -> Result<ResealOutcome> {
        let Some(manifest_hash) = entry.manifest_hash else {
            return Ok(ResealOutcome::AlreadyCurrent); // filtered by the query; defensive
        };
        // One column, two stores. `manifest_hash` names a chunk-store manifest
        // for an engine-recorded file, but a **blob-store primary** for a
        // Media-page upload — so a manifest fetch that misses is not
        // necessarily an error, it is the other provenance. Handing the failure
        // to the blob arm is what lets the completion observable drain at all
        // on an account holding any Media-page upload; before it, such an entry
        // failed here every catch-up, forever (`sync-agent.md` A8).
        let manifest = match self
            .fetch_manifest(&manifest_hash, entry.content_key_version)
            .await
        {
            Ok(manifest) => manifest,
            // Ruling (10)(c), the stamp binds the root: a stamped record opens
            // under its generation or not at all, and a blob primary rests
            // under the bare owner key (recorded unstamped), so a stamped
            // entry never reaches the blob arm — the manifest failure stands.
            Err(manifest_err) if entry.content_key_version.is_some() => {
                return Err(manifest_err);
            }
            Err(manifest_err) => {
                return self
                    .reseal_blob_primary(entry, manifest_hash, manifest_err)
                    .await;
            }
        };
        if self
            .opens_under_root(&manifest, current_root, &entry.path)
            .await?
        {
            // Already under this identity's own root — the common steady state
            // once the pass has run, and the state a corpus uploaded after the
            // succession was born in.
            self.db.mark_current_root_sealed(&entry.path)?;
            return Ok(ResealOutcome::AlreadyCurrent);
        }

        // Owed. A path this device holds moves by force-upload; anything else —
        // a cloud-only placeholder, a media-library item, the whole corpus of a
        // successor restoring onto a fresh device — moves by the nest-sourced
        // leg, whose plaintext source is the predecessor-key read.
        //
        // `Unknown` falls to the nest leg rather than refusing, and
        // that is not the special-casing item 1 forbids — it is the opposite
        // resolution. This arm always holds a `manifest_hash`, so it never
        // reaches the headless "there is nothing to re-seal anywhere" door;
        // routing an unreadable path to the nest concludes that the bytes exist
        // THERE, never that they exist nowhere, and the sentinel here is gated
        // on `Recorded` regardless. A successor restoring onto a fresh device
        // must not be stopped by one EACCES.
        let source = match self.path_is_materialized(&entry.path).await {
            Materialization::Present => ResealSource::LocalFile,
            Materialization::Absent | Materialization::Unknown(_) => ResealSource::NestBytes {
                manifest_hash,
                content_key_version: entry.content_key_version,
            },
        };
        let recorded = self
            .reseal_path_under_current(&entry.path, entry.content_key_version, source)
            .await?
            == ResealDisposition::Recorded;

        // ⚠ The sentinel is gated on the RECORD, not on the upload. Marking on
        // a successful upload alone would claim an entry is under the current
        // root while the nest's change-log head still names the
        // predecessor-sealed manifest — so every other device (and this one
        // after a dehydration) would still hydrate the retired-root copy, and a
        // completion check built on this observable would license bound (3) to
        // drop the only keys that can open it. Unmarked means "retry", which is
        // exactly what an unrecorded re-seal deserves.
        if !recorded {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(&entry.path),
                "owner-root corpus re-seal: the re-sealed copy uploaded but its change record did \
                 not land; the nest head is unchanged, so the entry stays owed"
            );
            return Ok(ResealOutcome::StillOwed);
        }
        self.db.mark_current_root_sealed(&entry.path)?;
        Ok(ResealOutcome::Resealed)
    }

    /// The **raw-AEAD Library plane's** re-seal arm: move a Media-page upload's
    /// blob-store primary off a retired owner root.
    ///
    /// Reached only when the manifest fetch missed, because the two provenances
    /// share one `manifest_hash` column
    /// ([`SyncClient::download_blob_opt`](crate::nest_client::SyncClient::download_blob_opt)).
    /// This is a *different seal* from the chunk plane, not a variant of it:
    /// a single framed `decrypt_backup_chunk` blob under the **bare**
    /// `BackupKey` (`Audience::Library`), never the convergent chunk root — which
    /// is why `FileDownloadKeys` does not reach it and why it did not move with
    /// the chunk plane (`succession-aftermath.md` § Re-key scope, the `BackupKey`
    /// corpus row).
    ///
    /// ⚠ **The content address is the only anchor this plane has, and it is
    /// load-bearing.** The backup-chunk frame carries no AAD, so a
    /// tag proves only *sealed-under-some-root-we-hold* — never *these bytes*.
    /// Without the address check a malicious nest serves any blob it holds under
    /// a retired root and this pass re-seals it under the **current** key and
    /// re-records it at the victim's path: the substitution becomes the
    /// account's own authenticated content. Do not carry the chunk leg's
    /// reasoning across — there the seal is content-bound by construction, so
    /// its walk may verify afterwards; here nothing else authenticates the
    /// bytes.
    ///
    /// The check runs **before** the candidate opens, matching the read plane
    /// (`MediaMachine::open_library_blob`). ⚠ Be precise about why, because the
    /// two call sites differ: on the *read* plane the ordering is itself
    /// load-bearing — the plaintext is handed to the user, so opening first
    /// would serve substituted bytes. Here it is **defense-in-depth**:
    /// reversing it changes no observable, since the address check still bails
    /// before anything is re-sealed, uploaded or recorded. The tests reflect
    /// that honestly — `post_succession_reseal_rejects_a_substituted_library_blob`
    /// pins the *check* (deleting it re-records attacker bytes) and no test pins
    /// the *ordering* at this call site, because at this call site there is
    /// nothing to observe. Keep the ordering anyway: it is free, and it keeps
    /// the two planes' shape identical for a reader.
    async fn reseal_blob_primary(
        &self,
        entry: &crate::db::SyncEntry,
        blob_hash: ContentHash,
        manifest_err: anyhow::Error,
    ) -> Result<ResealOutcome> {
        let path = entry.path.as_str();
        // Redacted once and reused by every log line/error string below
        // (`path-sealing.md` § Sealed names & paths, S7).
        let path_r = fauna_core::log_redact::log_path(path);
        // This plane exists only under an owner `BackupKey`. A bound or
        // plaintext set has no Library blob to move, so the manifest failure
        // was the real one.
        let Some(current) = self
            .backup_key
            .as_ref()
            .and_then(|k| k.client_key())
            .cloned()
        else {
            return Err(manifest_err.context(format!(
                "re-sealing {path_r}: no owner key for a Library blob"
            )));
        };

        let hash_hex = hex::encode(blob_hash.digest());
        let Some(sealed) = self.client.download_blob_opt(&blob_hash).await? else {
            // Neither store holds it: the manifest failure stands, with its own
            // diagnosis rather than a blob-shaped one.
            return Err(manifest_err.context(format!(
                "re-sealing {path_r}: {hash_hex} is in neither the manifest store nor the blob store"
            )));
        };

        // ⚠ The only anchor these bytes have — see the doc comment. Run it
        // before the candidate opens, mirroring the read plane's shape.
        if !blake3::hash(&sealed)
            .to_hex()
            .as_str()
            .eq_ignore_ascii_case(&hash_hex)
        {
            anyhow::bail!("re-sealing {path_r}: the nest served a different blob for {hash_hex}");
        }

        // Current key first — the steady state once the pass has run, and the
        // state any post-succession upload was born in.
        if fauna_core::crypto::decrypt_backup_chunk(&current, &sealed).is_ok() {
            self.db.mark_current_root_sealed(path)?;
            return Ok(ResealOutcome::AlreadyCurrent);
        }
        // The retired roots are bounded by who signed the head naming this
        // blob (`writer-signed-change-records.md` ruling (8)(c), the bare-key
        // twin of `MediaMachine::open_library_blob`): a predecessor's
        // signature moves only what its own root or an earlier one sealed.
        // The current-key probe above is not that open — it re-seals and
        // records nothing, exactly like the chunk arm's `opens_under_root`.
        let signer = self.manifest_signer(&blob_hash);
        let plaintext = signer
            .retired_keys(&self.predecessor_backup_keys)
            .into_iter()
            .filter_map(|key| key.client_key())
            .find_map(|key| fauna_core::crypto::decrypt_backup_chunk(key, &sealed).ok());
        let Some(plaintext) = plaintext else {
            // Owed, not done: no root this record's signer may reach opens
            // it. Leaving it unmarked is what keeps bound (3) from being
            // licensed to drop the keys — the list must drain on merit, never
            // by giving up.
            anyhow::bail!(
                "re-sealing {path_r}: no current or retired owner root its record's signer may \
                 reach opens the Library blob {hash_hex}; entry stays owed"
            );
        };

        // Re-seal under the current bare key. `process_and_seal` is the same
        // producer `MediaMachine::do_upload` runs, so the re-sealed primary is
        // byte-shaped exactly like an original upload — including a regenerated
        // thumbnail on a producer build, which moves that seal with the primary
        // instead of stranding it under the retired root.
        let audience = fauna_media::audience::Audience::Library {
            backup_key: current,
        };
        let payload = fauna_media::pipeline::process_and_seal(&plaintext, &audience);
        let size_bytes = payload.primary.bytes.len() as i64;
        let new_hash_hex = self
            .client
            .upload_blob_multipart(
                &payload.primary_sidecar.to_dag_cbor(),
                &payload.primary.bytes,
            )
            .await
            .with_context(|| {
                format!("re-sealing {path_r}: uploading the re-sealed Library blob")
            })?;

        // Best-effort, exactly like every other thumbnail site: a thumbnail that
        // fails to upload must not orphan the primary that already landed.
        let regenerated = match payload.thumbnail {
            Some((sealed_thumb, thumb_sidecar)) => {
                match self
                    .client
                    .upload_blob_multipart(&thumb_sidecar.to_dag_cbor(), &sealed_thumb.bytes)
                    .await
                {
                    Ok(hash) => Some(hash),
                    Err(e) => {
                        tracing::warn!(
                            path = %path_r,
                            error = %e,
                            "re-sealed Library thumbnail failed to upload; falling back to a move"
                        );
                        None
                    }
                }
            }
            None => None,
        };
        // A build without the thumbnailer regenerates nothing, so it moves the
        // recorded thumbnail instead of dropping its pointer.
        let thumbnail_hash = self
            .thumbnail_hash_for_reseal(path, regenerated, signer)
            .await;

        // The sentinel is gated on the RECORD, exactly as the chunk plane
        // settled it: an unrecorded re-seal leaves the nest head naming the
        // predecessor-sealed blob, so every other device keeps fetching that
        // copy — and marking it done would license bound (3) to drop the only
        // keys that open it.
        let Some(ref folder) = self.folder else {
            return Ok(ResealOutcome::StillOwed);
        };
        let new_hash = ContentHash::from_digest_raw(
            hex::decode(&new_hash_hex)
                .ok()
                .and_then(|v| <[u8; 32]>::try_from(v).ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("re-sealing {path_r}: nest returned a malformed blob hash")
                })?,
        );
        match self
            .record_change(
                folder,
                path,
                Some(&new_hash_hex),
                size_bytes,
                "create",
                // Unstamped, always: these bytes rest under the bare owner key,
                // and a stamp names a content-key generation — a stamp here
                // would be the stamped-but-owner-sealed record ruling (10)(c)
                // never opens (stamped entries never reach this arm).
                None,
                thumbnail_hash.as_deref(),
                // A proven reissue by construction — the same reasoning the
                // nest-sourced chunk leg records under: these bytes came out of
                // the recorded head itself, so nothing novel is being authored
                // and a receiver past this frontier must skip rather than merge.
                self.resolution_stamp(path),
            )
            .await
        {
            Ok(_seq) => {
                self.db.update_recorded_head(
                    path,
                    &new_hash,
                    size_bytes,
                    None,
                    &self.own_actor_id().0,
                )?;
                // Keep the cached pointer equal to what we just recorded —
                // otherwise the next pass would chase the pre-move thumbnail.
                self.db
                    .set_thumbnail_hash(path, thumbnail_hash.as_deref())?;
                self.db.mark_current_root_sealed(path)?;
                Ok(ResealOutcome::Resealed)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path_r,
                    error = %e,
                    "Library-blob re-seal: the re-sealed blob uploaded but its change record did \
                     not land; the nest head is unchanged, so the entry stays owed"
                );
                Ok(ResealOutcome::StillOwed)
            }
        }
    }

    /// The thumbnail hash a re-seal should record: regenerate if this build can,
    /// otherwise **move** the recorded one off the retired root.
    ///
    /// `regenerated` is what the caller's own producer yielded — `Some` on a
    /// build with the thumbnailer compiled in, which already re-seals under the
    /// current key on the way and needs nothing further. `None` has two causes
    /// that look identical here and must be told apart by the *recorded head*,
    /// not by guessing: the file legitimately has no thumbnail (a text file, a
    /// record made without one), or this build cannot render one
    /// (`process_media` off, or the upload failed). The row's cached
    /// `thumbnail_hash` is the discriminator — a head that names a thumbnail has
    /// one, whatever this build can do about it.
    ///
    /// ⚠ **Recording `None` over a head that names a thumbnail is the bug this
    /// closes.** The nest's latest-per-path projection takes the newest row, so
    /// a re-seal that records `None` drops the pointer: the thumbnail blob stays
    /// at rest, sealed under a root the aftermath is about to retire, referenced
    /// by nothing. Nothing notices — the entry still records and still marks, so
    /// the drain is unaffected — and the grid silently loses a tile that a
    /// producer build would have kept. Hence: move it.
    ///
    /// `signer` is who signed the head being re-sealed — the row that names
    /// the recorded thumbnail — and bounds the roots the move may open it
    /// under ([`Self::move_recorded_thumbnail`]).
    async fn thumbnail_hash_for_reseal(
        &self,
        path: &str,
        regenerated: Option<String>,
        signer: fauna_core::file_download::RecordSigner,
    ) -> Option<String> {
        if regenerated.is_some() {
            return regenerated;
        }
        // Read the pointer here rather than taking it as an argument: both
        // re-seal arms reach this from a different shape (one holds the entry,
        // one holds only the path), and a lookup keyed by the path they both
        // have is one row of local sqlite.
        let recorded = self.db.get_entry(path).ok().flatten()?.thumbnail_hash?;
        match self.move_recorded_thumbnail(path, &recorded, signer).await {
            Ok(moved) => moved,
            Err(e) => {
                // Best-effort, exactly like every other thumbnail site: a
                // thumbnail that cannot be moved must not fail the re-seal that
                // already moved the file's own bytes. The pointer drops, which
                // is the pre-fix behaviour, and the warning says so.
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(path),
                    error = %e,
                    "could not move the recorded thumbnail off the retired root; \
                     re-recording without one"
                );
                None
            }
        }
    }

    /// Move one **already-sealed** thumbnail blob from a retired owner root onto
    /// the current one, returning the hash to record.
    ///
    /// Needs no thumbnailer: it opens the existing thumbnail and re-seals those
    /// same pixels (`fauna_media::pipeline::seal_rendered_thumbnail`), which is
    /// what makes it work on precisely the builds that cannot regenerate.
    ///
    /// Three outcomes, all of them honest:
    /// - `Some(hash)` — the thumbnail now rests under the current root. When it
    ///   was already there the hash is the **caller's own**, unchanged and
    ///   un-reuploaded: re-sealing it would burn a blob to produce equivalent
    ///   ciphertext, and the steady state after one pass is that every thumbnail
    ///   is already current.
    /// - `None` — nothing to move that this device can move (the blob is gone,
    ///   or no held root opens it). The pointer drops, as it does today.
    /// - `Err` — the fetch itself failed, or the nest served the wrong bytes.
    ///
    /// ⚠ **The content address is load-bearing here for the same reason it is on
    /// the primary** ([`Self::reseal_blob_primary`]): the backup-chunk frame
    /// carries no AAD, so a tag proves only *sealed-under-some-root-we-hold*,
    /// never *these bytes*. Without the check a malicious nest serves any blob
    /// it holds under a retired root and this pass re-seals it under the
    /// **current** key and re-records it as the user's own thumbnail. A
    /// thumbnail is a smaller prize than a file's contents — it is also the one
    /// the user actually looks at.
    ///
    /// `signer` is who signed the head naming this thumbnail
    /// (`writer-signed-change-records.md` ruling (8)(c), through the same
    /// [`RecordSigner::retired_keys`](fauna_core::file_download::RecordSigner::retired_keys)
    /// as `MediaMachine::open_library_blob`): the re-seal re-records the
    /// pointer under the current identity's signature, so a thumbnail a
    /// predecessor's row named is carried only when that predecessor's own
    /// root or an earlier one opens it — never one the current root or a
    /// later retired root sealed, which that signature never vouched for.
    async fn move_recorded_thumbnail(
        &self,
        path: &str,
        recorded_hex: &str,
        signer: fauna_core::file_download::RecordSigner,
    ) -> Result<Option<String>> {
        // Redacted once and reused by every log line/error string below
        // (`path-sealing.md` § Sealed names & paths, S7).
        let path_r = fauna_core::log_redact::log_path(path);
        let Some(current) = self
            .backup_key
            .as_ref()
            .and_then(|k| k.client_key())
            .cloned()
        else {
            return Ok(None); // no owner key ⇒ no Library plane on this engine
        };
        let hash = ContentHash::from_digest_raw(
            fauna_core::hex32::decode(recorded_hex)
                .map_err(|_| anyhow::anyhow!("moving {path_r}'s thumbnail: malformed hash"))?,
        );
        let Some(sealed) = self.client.download_blob_opt(&hash).await? else {
            tracing::warn!(
                path = %path_r,
                thumbnail = recorded_hex,
                "the recorded thumbnail blob is gone from the nest; re-recording without one"
            );
            return Ok(None);
        };

        // ⚠ The only anchor these bytes have — before any candidate open, as on
        // the primary plane.
        if !blake3::hash(&sealed)
            .to_hex()
            .as_str()
            .eq_ignore_ascii_case(recorded_hex)
        {
            anyhow::bail!("moving {path_r}'s thumbnail: the nest served a different blob");
        }

        if signer.offers_current()
            && fauna_core::crypto::decrypt_backup_chunk(&current, &sealed).is_ok()
        {
            // Already current. Keep the pointer exactly as recorded.
            return Ok(Some(recorded_hex.to_string()));
        }
        let plaintext = signer
            .retired_keys(&self.predecessor_backup_keys)
            .into_iter()
            .filter_map(|key| key.client_key())
            .find_map(|key| fauna_core::crypto::decrypt_backup_chunk(key, &sealed).ok());
        let Some(plaintext) = plaintext else {
            tracing::warn!(
                path = %path_r,
                thumbnail = recorded_hex,
                "no owner root its record's signer may reach opens the recorded thumbnail; \
                 re-recording without one"
            );
            return Ok(None);
        };

        let audience = fauna_media::audience::Audience::Library {
            backup_key: current,
        };
        let resealed = fauna_media::pipeline::seal_rendered_thumbnail(&plaintext, &audience);
        let moved = self
            .client
            .upload_blob_multipart(&resealed.sidecar.to_dag_cbor(), &resealed.bytes)
            .await
            .with_context(|| {
                format!("moving {path_r}'s thumbnail: uploading the re-sealed blob")
            })?;
        Ok(Some(moved))
    }

    /// Does this manifest's first chunk open under `root` alone?
    ///
    /// The post-succession re-seal's per-entry discriminator. Deliberately one
    /// chunk, not the whole file: the seal is per-chunk and convergent under a
    /// single root, so chunk 0 decides the entry — and the sentinel means a given
    /// entry is asked at most once per device.
    ///
    /// A plaintext manifest (`stored_hashes` absent) is reported as opening under
    /// any root: there is no owner seal on it to move, and its migration belongs
    /// to [`Self::reseal_owner_only_plaintext`], not here.
    async fn opens_under_root(
        &self,
        manifest: &fauna_core::chunk::ChunkManifest,
        root: &[u8; 32],
        relative_path: &str,
    ) -> Result<bool> {
        let Some(stored) = manifest.stored_hashes.as_ref() else {
            return Ok(true); // plaintext manifest — the sibling pass owns it
        };
        let (Some(store_key), Some(chunk_hash)) = (stored.first(), manifest.chunk_hashes.first())
        else {
            return Ok(true); // empty file — nothing sealed
        };
        use fauna_core::file_download::BlobFetcher as _;
        let bodies = self
            .blob_fetcher()
            .fetch_chunks(std::slice::from_ref(store_key), relative_path)
            .await?;
        let Some(body) = bodies.first() else {
            anyhow::bail!(
                "chunk fetch returned nothing for {}",
                fauna_core::log_redact::log_path(relative_path)
            );
        };
        Ok(fauna_core::chunk_crypto::decrypt_chunk(root, chunk_hash, body).is_ok())
    }

    /// Are this path's bytes actually on this disk — i.e. can the force-upload
    /// leg read them?
    ///
    /// **Three answers, not two, and the third is the point**. [`Materialization::Absent`] is reserved for the two states
    /// that genuinely mean *the bytes are not here*: the file does not exist
    /// (`NotFound`), or `stat` succeeded and says it is a cloud-only
    /// placeholder. Every **other** `stat` failure — EACCES on a parent
    /// directory, EIO, ESTALE on a blipped network mount, ELOOP, an unmounted
    /// removable volume — answers [`Materialization::Unknown`]: this predicate
    /// does not know, and must not say it does.
    ///
    /// The distinction was invisible while the answer only ever meant *abort*.
    /// The fix moved it to a choke point where "not here" can also mean
    /// *converged, mark done*, and at that point a conflated transient fault
    /// becomes a **pass reported complete over an unconverged path** — one
    /// blip, once, and every caller counting on `Ok` (the drain requeue below,
    /// and until 2026-09-25 a cross-device one-shot sentinel that the `Ok`
    /// cleared permanently) treats the path as done. Hence the split lives HERE rather than in one
    /// caller: [`Self::reseal_path_under_current`] is not the only reader, and
    /// a per-caller fix would leave [`Self::drain_pending_uploads`]' requeue on
    /// the same conflation.
    ///
    /// This is the care the sibling predicate one file over already takes in the
    /// other direction (`placeholder.rs`'s `path_is_cloud_placeholder`: a path
    /// that cannot be `stat`ed is *not* quietly reinterpreted as a cloud-only
    /// one).
    async fn path_is_materialized(&self, relative_path: &str) -> Materialization {
        match tokio::fs::metadata(self.watch_dir.join(relative_path)).await {
            Ok(meta) => {
                if crate::placeholder::is_cloud_placeholder(&meta) {
                    Materialization::Absent
                } else {
                    Materialization::Present
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Materialization::Absent,
            Err(e) => Materialization::Unknown(e.to_string()),
        }
    }

    /// Re-seal one path under the engine's **current** seal root — a bound
    /// set's current content-key generation (`target_version = Some(v)`), or
    /// the owner-only `BackupKey` root (`None`) — verify the re-sealed copy
    /// end-to-end, then best-effort-supersede the path's stale change rows —
    /// the force-upload → verify → reclaim trio shared by
    /// [`Self::reseal_pending_under_current`] (the pre-bind migration),
    /// [`Self::reseal_owner_only_plaintext`] (the owner-only plaintext
    /// migration), and the drain's requeue-under-current (a
    /// pending transfer that matched a *prior* generation after a
    /// rotate-on-removal, or a plaintext queue entry left by a public-to-private
    /// flip-back —
    /// [`Self::drain_pending_uploads`]).
    async fn reseal_path_under_current(
        &self,
        path: &str,
        target_version: Option<u64>,
        source: ResealSource,
    ) -> Result<ResealDisposition> {
        // Redacted once and reused by every log line/error string below
        // (`path-sealing.md` § Sealed names & paths, S7).
        let path_r = fauna_core::log_redact::log_path(path);
        // the source decision lives HERE, not in each
        // caller, because five of the six passes that reach the choke point
        // forgot it and one remembered. `upload_file_inner`'s choke point
        // `bail!`s on a cloud-only placeholder — correctly — but every caller
        // `?`s that straight out of its walk, so ONE un-materialized entry
        // decided the fate of every entry behind it: the owner-only plaintext
        // migration left later entries resting plaintext, the declassify left
        // the site partly dark, and `reissue_corpus_for_web` (which has no
        // per-entry sentinel) re-force-uploaded and re-recorded the whole
        // prefix every 300 s, forever.
        //
        // The widening is the one `declassify_owner_corpus`' own doc already
        // pointed at, and `reseal_predecessor_sealed` was already doing: a
        // cloud-only placeholder is by definition *on the nest and not on this
        // disk* (`file-sync.md` § the six-state vocabulary — RemoteOnly: "on
        // the nest only … not yet hydrated"), so its bytes are fetched from
        // there instead of aborting. Deciding it in the shared trio makes the
        // omission unrepresentable rather than something six call sites must
        // each remember — the same reasoning the choke point itself is built on.
        // ask ONCE and branch on all three answers. The
        // `Unknown` arm refuses rather than continuing, and that refusal is the
        // whole fix: every caller `?`s this fn, so an `Err` is what leaves
        // the path unstamped for the next pass *and* stops the drain
        // requeue below from completing a transfer whose re-seal never
        // published. A fourth `ResealDisposition` would have had to be learned
        // by all five counting sites and would still have left the drain arm
        // discarding it.
        //
        // Note what this deliberately does NOT do: it does not restore the
        // pre-fix abort for a placeholder or a missing file. Those stay
        // `Absent` and keep the widening. Only a genuine fault aborts — which
        // is what the pass did for this same input, and was
        // the safe half of that behaviour.
        let materialization = match source {
            ResealSource::LocalFile => self.path_is_materialized(path).await,
            // The nest-bytes source never reads this disk, so its state is
            // irrelevant — do not pay a `stat` to ignore the answer.
            _ => Materialization::Present,
        };
        if let Materialization::Unknown(why) = &materialization {
            anyhow::bail!(
                "re-seal {path_r}: cannot tell whether its bytes are on this disk ({why}) — \
                 refusing to treat an unreadable path as one that exists nowhere; the pass \
                 stays owed and retries"
            );
        }
        let source = match source {
            ResealSource::LocalFile if materialization == Materialization::Absent => {
                match self.db.get_entry(path)?.and_then(|e| {
                    e.manifest_hash
                        .map(|manifest_hash| (manifest_hash, e.content_key_version))
                }) {
                    Some((manifest_hash, content_key_version)) => {
                        tracing::debug!(
                            path = %path_r,
                            "re-seal: path is a cloud-only placeholder; sourcing its bytes from \
                             the nest instead of the local disk"
                        );
                        ResealSource::NestBytes {
                            manifest_hash,
                            content_key_version,
                        }
                    }
                    // No recorded head: the bytes are neither on this disk nor
                    // on the nest, so there is nothing to re-seal anywhere.
                    // Not an error — the walk moves on, exactly as it does for
                    // an entry that was never uploaded.
                    None => {
                        tracing::debug!(
                            path = %path_r,
                            "re-seal: skipping — not materialized here and no recorded head to \
                             fetch from the nest"
                        );
                        return Ok(ResealDisposition::Nothing);
                    }
                }
            }
            other => other,
        };
        let receipt = match source {
            // Force: bypass the unchanged-content short-circuit — the file's
            // content is unchanged by definition; its at-rest sealing (or its
            // pending publication's generation) is what's stale.
            ResealSource::LocalFile => self.upload_file_inner(path, true).await?,
            ResealSource::NestBytes {
                manifest_hash,
                content_key_version,
            } => {
                self.reupload_from_nest(path, manifest_hash, content_key_version)
                    .await?
            }
        };
        let Some(receipt) = receipt else {
            return Ok(ResealDisposition::Nothing); // unreachable under force; defensive
        };

        // Verify BEFORE any reclaim (`webdav-server.md` § Architectural
        // rules: "deletes nothing until the re-sealed copy is verified").
        //
        // ⚠ Verified under the CURRENT roots **alone**. The engine's ordinary
        // `download_keys()` offers the retired roots — both axes,
        // `predecessor_backup_keys` (owner-key succession) and
        // `retired_content_keys` (a since-unserved set's M2 generations) — as
        // read candidates, so a verify through it answers "does this open at
        // all" — which a copy that never left a retired root passes. That is
        // the precise vacuity this pass exists to end (and the family of
        // findings kept hitting: an assertion that would have held
        // with the mechanism deleted). Emptying BOTH candidate sets makes the
        // verify prove the one thing `mark_current_root_sealed` then records.
        let mut verify_keys = self.download_keys();
        verify_keys.predecessor_backup_keys.clear();
        verify_keys.retired_content_keys = None;
        // Bounded (`verify_file_by_manifest` hashes window by window): the
        // verify must fit wherever the re-seal itself does — a capability
        // host's memory cap included (part (D)).
        fauna_core::file_download::verify_file_by_manifest(
            &self.blob_fetcher(),
            &verify_keys,
            receipt.manifest_hash,
            target_version,
            path,
        )
        .await
        .with_context(|| format!("verifying re-sealed {path_r} under the current root alone"))?;

        // Reclaim — best-effort supersede of the stale rows. Skipped when the
        // change record never reached the nest (the head there is still the
        // old manifest; a supersede would head-mismatch).
        if receipt.recorded
            && let Some(ref folder) = self.folder
        {
            match fauna_client_sync::SyncClient::new(self.nest_client.clone())
                .changes_supersede(
                    folder.clone(),
                    self.client.device_id_hex(),
                    path,
                    hex::encode(receipt.manifest_hash.digest()),
                )
                .await
            {
                Ok(reply) => tracing::debug!(
                    path = %path_r,
                    superseded = reply.superseded,
                    "pre-re-seal change rows superseded"
                ),
                // Multi-writer D3 (file-sync.md § Multi-writer shared sets):
                // `changes.supersede` is owner-only, so a WRITER member's
                // engine lands here with a typed `not_found` on every re-seal —
                // deliberately non-fatal (the re-sealed record already landed
                // via `changes.record`; reclaiming the stale pre-re-seal record
                // is the owner's verified-reclaim pass). Same arm as any other
                // refusal: nothing propagates, the engine never wedges.
                Err(e) => tracing::warn!(
                    path = %path_r,
                    error = %e,
                    "supersede failed; reclaim deferred (old chunks stay until a later pass)"
                ),
            }
        }
        Ok(if receipt.recorded {
            ResealDisposition::Recorded
        } else {
            ResealDisposition::Unrecorded
        })
    }

    /// [`ResealSource::NestBytes`]'s leg: fetch a tracked path's plaintext from
    /// the nest, re-seal it under this engine's current root, and re-record it.
    ///
    /// The post-succession re-seal's dominant case. `reseal_path_under_current`'s
    /// local leg force-uploads from the watch dir, and `upload_file_inner`'s
    /// choke point refuses a cloud-only placeholder outright — correctly, there
    /// is nothing local to upload — so a device that does not hold the file has
    /// exactly one plaintext source: the nest's own copy, opened under a retired
    /// root ([`fauna_core::file_download::FileDownloadKeys::predecessor_backup_keys`]).
    /// That read fallback is not an optimization here, it is this leg's
    /// precondition.
    ///
    /// ⚠ **Deliberately does not hydrate.** The bytes are re-sealed in memory
    /// and never written to the watch dir: materializing the corpus to re-seal
    /// it would fill a device that deliberately does not hold it (and, on a
    /// cfapi root, would fight the provider for its own placeholders).
    async fn reupload_from_nest(
        &self,
        path: &str,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
    ) -> Result<Option<ResealUpload>> {
        // Bounded: the file is fetched, opened, re-sealed and uploaded a window
        // at a time and never held whole (`mls-group-key-material.md` § M2 →
        // *Pre-bind re-seal migration*, part (D) — a capability host runs this
        // under an extension's memory cap). The whole-file content address is
        // verified before the new manifest posts, so what is recorded is
        // provably the recorded head's own content.
        let uploaded = self
            .reseal_nest_copy_windowed(path, manifest_hash, content_key_version)
            .await
            .with_context(|| {
                format!(
                    "re-sealing {} from its nest copy",
                    fauna_core::log_redact::log_path(path)
                )
            })?;
        let size = uploaded.manifest.total_size as i64;

        // The plaintext is never whole here, so nothing regenerates a
        // thumbnail: the recorded one is MOVED instead (needs no thumbnailer),
        // so the pointer survives the re-seal either way.
        let thumbnail_hash = self
            .thumbnail_hash_for_reseal(path, None, self.manifest_signer(&manifest_hash))
            .await;

        let mut recorded = false;
        if let Some(ref folder) = self.folder {
            let manifest_hash_hex = hex::encode(uploaded.manifest_hash.digest());
            match self
                .record_change(
                    folder,
                    path,
                    Some(&manifest_hash_hex),
                    size,
                    "create",
                    uploaded.content_key_version,
                    thumbnail_hash.as_deref(),
                    // A proven reissue **by construction**, stronger than the
                    // local path's heuristic: these bytes came out of the
                    // recorded head itself, so there is no novel content here
                    // and a receiver whose frontier already passed it must skip
                    // rather than merge it (the gap-1 ruling, `conflicts.md`
                    // § Concurrent resolution). Never `CausalStamp::edit` — and
                    // the edit frontier is deliberately NOT advanced either:
                    // this device authored nothing, it moved a seal.
                    self.resolution_stamp(path),
                )
                .await
            {
                Ok(_seq) => {
                    recorded = true;
                    // The record landed, so the new manifest IS the nest head:
                    // re-point the row's hydration anchor at it, or the next
                    // hydration fetches the predecessor-sealed copy — which is
                    // exactly what goes dark once `sync-agent.md` bound (3)
                    // drops the retired keys.
                    //
                    // `update_recorded_head`, NOT `commit_recorded_head`: the
                    // latter stamps `recorded_content_hash` from `local_hash`,
                    // and on a placeholder row (NULL `local_hash`) that would
                    // CLEAR a dehydration proof this re-seal never invalidated
                    // — the content is byte-identical, only its seal moved.
                    self.db.update_recorded_head(
                        path,
                        &uploaded.manifest_hash,
                        size,
                        uploaded.content_key_version,
                        &self.own_actor_id().0,
                    )?;
                    // Keep the cached pointer equal to what we just recorded —
                    // otherwise the next pass would chase the pre-move thumbnail.
                    self.db
                        .set_thumbnail_hash(path, thumbnail_hash.as_deref())?;
                }
                Err(e) => tracing::warn!(
                    path = %fauna_core::log_redact::log_path(path),
                    error = %e,
                    "nest-sourced re-seal: the change record did not land, so the nest head still \
                     names the predecessor-sealed manifest; entry stays owed"
                ),
            }
        }

        Ok(Some(ResealUpload {
            manifest_hash: uploaded.manifest_hash,
            recorded,
        }))
    }

    /// Re-seal the nest's copy of one recorded head under this engine's upload
    /// root **one window at a time** — [`fauna_core::file_download::ManifestWalk`]
    /// opens each window under whichever root the recorded head's stamp selects
    /// (the owner root for an owner's unstamped pre-bind record, part (D)), and
    /// each chunk is re-sealed through the one per-chunk pipeline
    /// (`seal::seal_chunk_body`) and uploaded before the next window is fetched.
    /// Peak memory is O(window × chunk), never O(file).
    ///
    /// The new manifest keeps the source's plaintext chunk list (the chunking
    /// is a function of the content, and the content is unchanged) with the new
    /// store keys — the manifest `seal_blob` would build from the whole buffer,
    /// so a re-run converges on the identical manifest hash. It posts only
    /// after the running whole-file hash equals the source manifest's address:
    /// a nest that served other chunks under this head gets nothing recorded.
    ///
    /// No resume queue, like the whole-buffer form it replaces: the transfer
    /// queue's resume leg re-reads the WATCH DIR, which is exactly where these
    /// bytes are not. An interrupted re-seal leaves the row unstamped and the
    /// next pass re-drives it (the chunks it already uploaded dedup).
    async fn reseal_nest_copy_windowed(
        &self,
        path: &str,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
    ) -> Result<UploadedManifest> {
        let keys = self.download_keys_for_record(&manifest_hash);
        self.reseal_nest_copy_windowed_under(path, manifest_hash, content_key_version, &keys)
            .await
    }

    /// [`Self::reseal_nest_copy_windowed`] opening under `keys` — for a caller
    /// that holds the record's verdict itself (the re-record leg's
    /// predecessor arm, whose retired-nonce heads no verify step admitted).
    async fn reseal_nest_copy_windowed_under(
        &self,
        path: &str,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
        keys: &fauna_core::file_download::FileDownloadKeys,
    ) -> Result<UploadedManifest> {
        // The byte move itself is the shared one (`fauna_core::nest_reseal`,
        // which the flipping client's served-set walk runs too — the two
        // converge on the same store keys); this engine supplies only its
        // root, its per-record keys and where the chunks go.
        let seal_root = self.upload_seal_root()?;
        // Its chunk puts are a send to the nest (decision 2′ (c)).
        if let Some(hold) = self.publish_hold() {
            anyhow::bail!("re-seal of the nest copy held back: {hold}");
        }
        let sink = EngineChunkSink {
            client: &self.client,
            transfer_pool: &self.transfer_pool,
            metadata_only: self.is_metadata_only_residency(),
        };
        let resealed = fauna_core::nest_reseal::reseal_nest_copy_windowed(
            &self.blob_fetcher(),
            keys,
            manifest_hash,
            content_key_version,
            path,
            seal_root,
            &sink,
        )
        .await?;
        Ok(UploadedManifest {
            manifest: resealed.manifest,
            manifest_hash: resealed.manifest_hash,
            uploaded_count: resealed.uploaded_count,
            content_key_version: resealed.content_key_version,
        })
    }

    /// Drain any pending uploads left over from a previous run or failed batch.
    pub async fn drain_pending_uploads(&self) -> Result<()> {
        // Route the drained-upload seal through the M2 content keys so a bound
        // (shared) set matches entries under every retained generation — an
        // entry enqueued before a rotate-on-removal still matches its
        // (now-prior) generation's ciphertext store key instead of being
        // silently dropped (see `content_drain_roots`). `?` propagates the
        // FS-BIND-5 fail-closed error (a bound-but-unkeyed engine refuses to
        // drain rather than re-upload plaintext).
        //
        // A prior-generation match is NOT re-uploaded as-is: the chunk
        // was never published, the removed member holds that generation
        // irrevocably, and chunk GET is unauthenticated by design — so
        // publishing it after the rotation would hand the removed member
        // content first published post-removal. Instead the path is
        // **requeued under current**: re-sealed + re-recorded via the shared
        // re-seal trio (`reseal_path_under_current` — the change record IS
        // re-stamped with the new generation, superseding the stale gen-N
        // record whose never-uploaded chunks would otherwise dangle), and only
        // then are its stale queue entries completed. Crash-safe: entries
        // outlive the re-seal, so an interrupted requeue re-detects and
        // re-drives on the next drain (the re-upload is convergent +
        // store-deduped).
        // A `public`-audience engine (phase 4) enqueued its entries under
        // PLAINTEXT store keys — the drain map's plaintext arm (no roots, no
        // backup key) is the matching shape; routing through the sealed arms
        // would miss every queued entry and silently drop the never-uploaded
        // chunks (the STOREKEY-GAP failure mode, plaintext edition).
        // Phase 5: a metadata-only folder's queued BYTE uploads are
        // deliberately never pushed — complete them so the queue drains
        // instead of growing forever. Entries enqueued before the flip name
        // bytes the nest-side drop already reclaims; entries enqueued after
        // it cannot exist (the gate above the queue never enqueues while
        // armed). A later flip back to full re-uploads convergently through
        // the ordinary reconcile, not this queue.
        if self.is_metadata_only_residency() {
            for entry in self.db.eligible_transfers("upload")? {
                self.db.complete_transfer(entry.id)?;
            }
            return Ok(());
        }
        // Decision 2′ (c): entries an earlier pass enqueued publish on a read
        // floor only. Held, they wait — a floor read ahead rebuilds the engine,
        // and the requeue below then completes the older generation's entries
        // unpublished.
        if let Some(hold) = self.publish_hold() {
            tracing::info!(%hold, "queued uploads wait for a floor read");
            return Ok(());
        }
        let (chunk_roots, drain_backup_key) = if self.is_public_audience() {
            (None, None)
        } else {
            (self.content_drain_roots()?, self.effective_backup_key())
        };
        let requeue = crate::transfer_worker::drain_pending_uploads(
            &self.watch_dir,
            &self.db,
            &self.client,
            chunk_roots.as_deref(),
            drain_backup_key,
            &self.transfer_pool,
        )
        .await?;
        if requeue.is_empty() {
            return Ok(());
        }
        // A requeue match implies an engine with a live seal root: a bound
        // engine's loaded generations (re-seal under `current`, stamped), or a
        // keyed owner-only engine re-sealing a PLAINTEXT queue entry left by a
        // public-to-private flip-back under the `BackupKey` root (no generation
        // stamp). Anything else is a logic error —
        // fail loud rather than publish or drop.
        let target_version = if self.effective_backup_key().is_some() {
            None
        } else {
            match self.content_seal_root()? {
                Some((_, Some(v))) => Some(v),
                _ => anyhow::bail!(
                    "drain requeue: stale-generation matches on an engine with no live seal \
                     root (refusing to publish or drop)"
                ),
            }
        };
        for rel in requeue {
            self.reseal_path_under_current(&rel, target_version, ResealSource::LocalFile)
                .await?;
            // The re-seal re-recorded + re-published the path under `current`;
            // its remaining queued entries (prior-generation store keys) are
            // superseded — complete them so the next drain doesn't re-detect.
            for entry in self.db.eligible_transfers("upload")? {
                if entry.path == rel {
                    self.db.complete_transfer(entry.id)?;
                }
            }
        }
        Ok(())
    }

    /// Hex device id this engine records sync changes under (the bearer's
    /// `device_id`, shared with the byte-plane [`SyncClient`]). Used by the
    /// segment-backup coordinator to register + member-add this device on a
    /// destination before recording custody, and — `pub` for the same
    /// off-crate reason as [`Self::control_plane`] — by the agent's
    /// `HydrationHost::sync_mode` to find *this* device's member row (its
    /// role decides `SyncMode::applies_remote_deletes`, `file-sync.md` § 4).
    pub fn device_id_hex(&self) -> &str {
        self.client.device_id_hex()
    }

    /// The control-plane WS-RPC client. Cloned cheaply (it is an `Arc`)
    /// so a coordinator can drive control-plane kinds over the same
    /// authenticated connection [`Self::record_change`] uses.
    ///
    /// `pub` for the same reason off-crate: an out-of-app on-demand hydration host
    /// (the Windows cfapi service; a macOS File Provider next) drives its own
    /// `fauna.folders.list` over this one already-connected plane — and watches
    /// `subscribe_reconnects` — rather than opening a second socket per root.
    pub fn control_plane(&self) -> Arc<fauna_client::NestClient> {
        Arc::clone(&self.nest_client)
    }

    /// The folder this engine is scoped to (`None` = the actor's whole change
    /// history under one root — the single-root shape). A multi-root host
    /// needs it to resolve *this* root's row out of `fauna.folders.list`.
    pub fn folder(&self) -> Option<&str> {
        self.folder.as_deref()
    }

    /// Mint (or refresh) `relative_path`'s own-PENDING retention row — the
    /// offline-authored leg (B2.5, `p2p-shared-set-build.md` § *Build design — the row half*,
    /// the own-pending bullet). Called by the two upload paths at the seam
    /// the design names: the manifest was just computed from THIS upload's
    /// own local seal (never from applied state — the synthesis trap), and no
    /// network has been touched yet. Offline, this row is exactly what the
    /// share leg serves; online it lives milliseconds until the funnel's
    /// `Ok(seq)` upgrades it in place (`SyncDb::retain_own_change`).
    ///
    /// Folder-gated like the record itself (a folder-less engine records
    /// nothing, so its pending row could never upgrade), and best-effort like
    /// the retention epilogue: the upload must not fail over retention
    /// bookkeeping.
    fn mint_pending_own_change(
        &self,
        relative_path: &str,
        manifest_hash: &ContentHash,
        size_bytes: i64,
        content_key_version: Option<u64>,
        proven_reissue: bool,
    ) {
        if self.folder.is_none() {
            return;
        }
        // The same stamp the record will carry (`upload_file_inner`'s
        // `Ok(seq)` arm): a fresh edit derives from the honest anchor; a
        // proven reissue is no-novel-content at the same claim.
        let causal = if proven_reissue {
            self.resolution_stamp(relative_path)
        } else {
            self.edit_stamp(relative_path)
        };
        let minted = self
            .seal_recorded_path(relative_path)
            .and_then(|path_sealed| {
                let mut pending = crate::db::OwnChangeRow {
                    seq: None,
                    path: relative_path.to_string(),
                    path_hash: hex::encode(fauna_core::sync::path_hash(relative_path)),
                    path_sealed,
                    manifest_hash: Some(hex::encode(manifest_hash.digest())),
                    size_bytes,
                    change_type: "create".to_string(),
                    created_at: fauna_core::data::Timestamp::now_millis() as i64,
                    content_key_version,
                    // Computed after the upload — the sequenced upgrade
                    // carries it; a pending row serves without one.
                    thumbnail_hash: None,
                    derived_through: causal.derived_through,
                    is_resolution: causal.is_resolution,
                    author_actor_id: self.owner_actor_id_hex(),
                    device_id: self.client.device_id_hex().to_string(),
                    ..Default::default()
                };
                // Signed as served, so a peer relays this offline row onward
                // and every member verifies it (the relayed-row lift).
                self.sign_own_row(&mut pending);
                self.db.mint_pending_own_change(&pending)
            });
        if let Err(e) = minted {
            tracing::warn!(
                error = %e,
                path = %fauna_core::log_redact::log_path(relative_path),
                "own-pending retention mint failed; the upload continues"
            );
        }
    }

    /// Record a file change on the nest over the `fauna.sync.changes.record`
    /// WS-RPC kind. The control plane rides the bearer [`fauna_client::NestClient`]
    /// ([`Self::nest_client`]), not the byte-plane HTTP [`SyncClient`]; returns
    /// the assigned sequence number. `manifest_hash = None` records a delete.
    /// `content_key_version` is the M2 generation the chunks were sealed under
    /// (`None` for owner-only sets / deletes); the nest stores it opaque and
    /// echoes it back so a reader selects the right content key.
    ///
    /// `causal` is the causal-watermark stamp (the 2026-08-02 ruling,
    /// `file-sync.md` § Conflicts): the recorded
    /// row's `derived_through`/`is_resolution` pair. [`CausalStamp::unknown`]
    /// records honestly-unknown causality (pre-ruling row semantics).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_change(
        &self,
        folder: &str,
        path: &str,
        manifest_hash: Option<&str>,
        size_bytes: i64,
        change_type: &str,
        content_key_version: Option<u64>,
        thumbnail_hash: Option<&str>,
        causal: CausalStamp,
    ) -> Result<i64> {
        // Cross-nest writer (Phase 3): relay the record through this actor's own
        // nest to the set's home nest by carrying `nest_url`+`channel_id` (the
        // home nest applies its foreign-member + writer gate + owner-pays
        // metering + content-idempotence). Sent directly over the control-plane
        // `NestClient` — the `changes_record` self-heal is a local-device concern
        // that never fires cross-nest (the home nest doesn't gate on the
        // member's device).
        // Parked (D4): the authoritative nest has already refused this set's
        // write grant, so every further record is a known-doomed request. Refuse
        // locally instead of re-asking forever — the retry-forever loop is what
        // makes a revoked folder *look* like it is still syncing.
        if self.access_gate.is_revoked() {
            anyhow::bail!(
                "folder {:?} is parked: write access was revoked by the nest that owns it",
                fauna_core::log_redact::log_folder_name(folder)
            );
        }
        // Decision 2′ (c): a record is a send to the nest like any chunk.
        if let Some(hold) = self.publish_hold() {
            anyhow::bail!("record_change: {hold}");
        }

        // The one seal funnel (`path-sealing.md`): every recorded label passes
        // through here, so both record planes below carry the sealed sibling
        // and no caller of `record_change` has to know sealing exists. The
        // plaintext `path` still travels alongside, and is not a dual-write
        // remnant: the nest hashes it to `path_hash` (which the writer
        // signature binds), and rests it only on a set that rests plaintext
        // by ratification — on every other set the flip made a sealless
        // record `path_seal_required` and the plaintext column rests NULL.
        let path_sealed = self.seal_recorded_path(path)?;

        let seq = if let Some((home_nest_url, channel_id_hex)) = self.foreign_routing() {
            use fauna_protocol::RpcRequester;
            let mut req = fauna_protocol::sync::SyncChangeRecordRequest {
                folder: folder.to_string(),
                device_id: self.client.device_id_hex().to_string(),
                path: path.to_string(),
                manifest_hash: manifest_hash.map(str::to_string),
                size_bytes,
                change_type: change_type.to_string(),
                content_key_version,
                thumbnail_hash: thumbnail_hash.map(str::to_string),
                nest_url: Some(home_nest_url),
                channel_id: Some(channel_id_hex),
                path_sealed: path_sealed.clone().map(fauna_protocol::ByteBuf::from),
                name_hash: None,
                derived_through: causal.derived_through,
                is_resolution: causal.is_resolution,
                extra: Default::default(),
                signature: None,
                signer_key: None,
                signer_cert: None,
            };
            req = fauna_protocol::folders::addressed(req);
            // Signed last, over the final field values; the relay carries the
            // cert inline (the home nest holds no row of this writer's grants).
            self.sign_record(&mut req);
            let reply: fauna_protocol::sync::SyncChangeRecordReply = self
                .nest_client
                .request("fauna.sync.changes.record", req)
                .await
                .map_err(|e| {
                    self.note_access_refusal(folder, &e);
                    anyhow::anyhow!("cross-nest fauna.sync.changes.record: {e}")
                })?;
            reply.seq
        } else {
            let reply = self
                .record_client()
                .changes_record(
                    folder,
                    self.client.device_id_hex(),
                    path,
                    manifest_hash.map(str::to_string),
                    size_bytes,
                    change_type,
                    content_key_version,
                    // The hex hash of the sealed thumbnail blob the producer uploaded
                    // for this file (`maybe_upload_thumbnail`), or `None` for a
                    // non-image / small image / non-owner-sealed set. Surfaces as
                    // `fauna.media.list` → `MediaItem.thumbnail_hash`.
                    thumbnail_hash.map(str::to_string),
                    path_sealed.clone(),
                    causal.derived_through,
                    causal.is_resolution,
                )
                .await
                .inspect_err(|e| self.note_access_refusal(folder, e))
                .context("fauna.sync.changes.record")?;
            reply.seq
        };

        // The share leg's ROW-half retention (B2 — `p2p-shared-set-build.md` § Build design —
        // the row half): the ack names the seq, so the row provably exists in
        // the nest's log — write it through to `own_change_log` so the share
        // leg serves this replica's REAL recorded rows, never rows synthesized
        // from `sync_entries` (the trap that defeats the provenance ruling).
        // Own-authored by construction: every caller of this funnel records
        // this replica's own change. Best-effort: the nest's log is the
        // durable copy, so a failed write costs offline serveability of one
        // row, never the record itself.
        let mut retained = crate::db::OwnChangeRow {
            seq: Some(seq),
            path: path.to_string(),
            path_hash: hex::encode(fauna_core::sync::path_hash(path)),
            path_sealed,
            manifest_hash: manifest_hash.map(str::to_string),
            size_bytes,
            change_type: change_type.to_string(),
            created_at: fauna_core::data::Timestamp::now_millis() as i64,
            content_key_version,
            thumbnail_hash: thumbnail_hash.map(str::to_string),
            derived_through: causal.derived_through,
            is_resolution: causal.is_resolution,
            author_actor_id: self.owner_actor_id_hex(),
            device_id: self.client.device_id_hex().to_string(),
            ..Default::default()
        };
        self.sign_own_row(&mut retained);
        if let Err(e) = self.db.retain_own_change(&retained) {
            tracing::warn!(
                error = %e,
                path = %fauna_core::log_redact::log_path(path),
                "own-change retention write failed; the record itself landed"
            );
        }
        Ok(seq)
    }

    /// Ingest one page of ACCEPTED peer-served share rows (B2.3 — `p2p-shared-set-build.md`
    /// § *Build design — the row half*): land each as the path's provisional
    /// overlay row and, where the local path is clean, materialize a
    /// create/modify's bytes through the shared download walk with `fetcher`
    /// (the caller's per-set peer fetcher, `PeerShareBlobFetcher` in
    /// production).
    ///
    /// Four doors, each load-bearing:
    /// - **Provenance is judged HERE**, authoritatively
    ///   ([`fauna_peer_share::provenance::judge_peer_row`]): each row as served
    ///   through this engine's own reader (its nonce, owner and roster, plus
    ///   the row's inline cert), the share leg's cached writer roster standing
    ///   in for a roster the reader has not read — the engine never trusts
    ///   that the pump screened. A verified row of another writer is also
    ///   retained for relay onward.
    /// - **No nest-log accounting.** Anchor, path frontiers and the
    ///   edit-frontier are the converged log's bookkeeping
    ///   (`conflicts.md` clause 5); a provisional row has no seq there and
    ///   touches none of them — which is also why this fn never calls into
    ///   `apply_remote_changes`' fold.
    /// - **A provisional delete is view-only.** It lands in the overlay and
    ///   touches neither `sync_entries` nor disk — local bytes die only on
    ///   nest-confirmed deletes, by ruling.
    /// - **Materialization never earns the dehydration proof.**
    ///   `recorded_content_hash` stays untouched: the nest does not hold
    ///   these bytes, so freeing local bytes on their account would be data
    ///   loss until the nest confirms (B2.4's reconcile stamps it then).
    ///
    /// A per-row fetch failure skips that row (reported), never the page.
    ///
    /// This is the **resident** landing — a bound tree takes every accepted
    /// body. An on-demand replica lands through
    /// [`Self::ingest_peer_share_rows_on_demand`]: the same four doors, and
    /// bodies by policy.
    #[cfg(feature = "p2p-share")]
    pub async fn ingest_peer_share_rows(
        &self,
        accepted: &[fauna_protocol::peer_share::PeerShareChange],
        proven_actor_hex: &str,
        fetcher: &dyn fauna_core::file_download::BlobFetcher,
    ) -> Result<crate::peer_share_store::PeerIngestReport> {
        self.ingest_peer_share_rows_landing(accepted, proven_actor_hex, fetcher, None)
            .await
    }

    /// [`Self::ingest_peer_share_rows`] for an **on-demand replica**
    /// (`p2p-shared-set-build.md` § *Phone peers — design*, decision 1: *the
    /// ingest half lands rows always, and bodies by policy*). Provenance, the
    /// overlay row, relay retention and the view-only delete are the same
    /// code; what differs is the landing: every accepted row is recorded (a
    /// new path as a placeholder, metadata only), and a body lands only when
    /// it is wanted ([`crate::share_landing::body_is_wanted`]) — in the kept
    /// root, where it stays until the nest confirms it, and never into the
    /// device's last free space ([`crate::share_landing::STORAGE_FLOOR_BYTES`]).
    #[cfg(feature = "p2p-share")]
    pub async fn ingest_peer_share_rows_on_demand(
        &self,
        accepted: &[fauna_protocol::peer_share::PeerShareChange],
        proven_actor_hex: &str,
        fetcher: &dyn fauna_core::file_download::BlobFetcher,
        landing: &OnDemandLanding<'_>,
    ) -> Result<crate::peer_share_store::PeerIngestReport> {
        self.ingest_peer_share_rows_landing(accepted, proven_actor_hex, fetcher, Some(landing))
            .await
    }

    #[cfg(feature = "p2p-share")]
    pub(crate) async fn ingest_peer_share_rows_landing(
        &self,
        accepted: &[fauna_protocol::peer_share::PeerShareChange],
        proven_actor_hex: &str,
        fetcher: &dyn fauna_core::file_download::BlobFetcher,
        landing: Option<&OnDemandLanding<'_>>,
    ) -> Result<crate::peer_share_store::PeerIngestReport> {
        use crate::peer_share_store::{
            LocalPathState, MaterializeVerdict, PeerIngestReport, judge_materialization,
        };
        let mut report = PeerIngestReport::default();
        // The reader, as of now, plus this page's inline certs — a clone, so a
        // peer-carried cert never replaces one the nest's side table installed.
        let mut reader = self
            .row_reader
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        reader.ingest_certs(accepted.iter().filter_map(|r| r.signer_cert.as_ref()));
        // Offline since launch the reader never read its roster: the share
        // leg's cached one stands in, chains and all, so the one judge gives a
        // peer-served row the nest pull's verdict
        // (`writer-signed-change-records.md` ruling (11)(i)).
        let reader = fauna_peer_share::provenance::reader_over_cached_roster(
            reader,
            crate::peer_share_store::cached_writer_roster(&self.db),
        );
        let peer_is_cached_writer = self
            .db
            .cached_share_writer(proven_actor_hex)
            .unwrap_or(false);
        let own = self.own_actor_id().0;
        let mut relayable = Vec::new();
        let mut relayable_pending = Vec::new();

        // Judged AS SERVED (`row.change`), never the path-opened copy — and
        // BEFORE the open: the path below is opened under the roots the
        // row's signer may reach (ruling (8)(c)), so the copy it opens names
        // the actor the judge recovered, never the peer's stamp.
        let admissions: Vec<_> = accepted
            .iter()
            .map(|row| {
                fauna_peer_share::provenance::judge_peer_row(
                    &reader,
                    &row.change,
                    row.sequenced,
                    proven_actor_hex,
                    peer_is_cached_writer,
                )
            })
            .collect();
        // Open sealed paths for rows carrying no plaintext path — the same
        // render the nest-pull apply runs.
        let mut changes: Vec<fauna_protocol::sync::SyncChange> = accepted
            .iter()
            .zip(&admissions)
            .map(|(row, admission)| {
                let mut change = row.change.clone();
                change.author_actor_id = match admission {
                    Ok(fauna_peer_share::provenance::PeerRowAdmission::Verified { writer }) => {
                        Some(hex::encode(writer))
                    }
                    Ok(fauna_peer_share::provenance::PeerRowAdmission::ChannelProven) => {
                        Some(proven_actor_hex.to_ascii_lowercase())
                    }
                    Err(_) => change.author_actor_id,
                };
                change
            })
            .collect();
        self.open_sealed_change_paths(&mut changes);

        for ((row, change), admission) in accepted.iter().zip(changes.iter()).zip(admissions) {
            let writer_hex = match admission {
                Ok(fauna_peer_share::provenance::PeerRowAdmission::Verified { writer }) => {
                    if writer != own {
                        if row.sequenced {
                            relayable.push((writer, row.change.clone()));
                        } else {
                            relayable_pending.push((writer, row.change.clone()));
                        }
                    }
                    hex::encode(writer)
                }
                Ok(fauna_peer_share::provenance::PeerRowAdmission::ChannelProven) => {
                    report.channel_proven += 1;
                    proven_actor_hex.to_lowercase()
                }
                Err(refusal) => {
                    tracing::debug!(
                        seq = change.seq,
                        reason = refusal.reason(),
                        "peer-served row refused at ingest"
                    );
                    if matches!(
                        refusal,
                        fauna_peer_share::provenance::RowRefusal::NotJudgeableYet(_)
                    ) && row.sequenced
                    {
                        // Judgeable later: keep the cursor below it.
                        report.retry_floor =
                            Some(report.retry_floor.map_or(change.seq, |f| f.min(change.seq)));
                    }
                    report.refused += 1;
                    continue;
                }
            };
            let Some(path) = change.path.as_deref().filter(|p| !p.is_empty()) else {
                report
                    .skipped
                    .push((change.path_hash.clone(), "row carries no openable path"));
                continue;
            };
            if !crate::path_guard::is_safe_relative_path(path) {
                report
                    .skipped
                    .push((path.to_string(), "unsafe path refused"));
                continue;
            }

            // What the overlay said of this path BEFORE this row replaces it:
            // an on-demand landing reads it to tell the body an earlier peer
            // row landed from a write intent.
            let prior_overlay = match landing {
                Some(_) => self.db.get_share_overlay(path)?,
                None => None,
            };
            if !self.db.upsert_share_overlay(&crate::db::ShareOverlayRow {
                path: path.to_string(),
                seq: change.seq,
                sequenced: row.sequenced,
                change_type: change.change_type.clone(),
                manifest_hash: change.manifest_hash.clone(),
                size_bytes: change.size_bytes,
                content_key_version: change.content_key_version,
                proven_author: writer_hex,
                materialized: false,
                content_hash: None,
            })? {
                // Provably staler than the overlay's current row — done.
                continue;
            }
            report.overlaid += 1;

            if change.change_type == "delete" {
                continue; // view-only by ruling (doc above)
            }
            let Some(manifest_hex) = change.manifest_hash.as_deref() else {
                report
                    .skipped
                    .push((path.to_string(), "create/modify without a manifest"));
                continue;
            };
            let peer_manifest = Self::parse_manifest_hash(manifest_hex)?;

            if let Some(landing) = landing {
                self.land_peer_row_on_demand(
                    landing,
                    row.sequenced,
                    change,
                    path,
                    peer_manifest,
                    prior_overlay.as_ref(),
                    fetcher,
                    &mut report,
                )
                .await?;
                continue;
            }

            let entry = self.db.get_entry(path)?;
            let entry_version = entry.as_ref().map(|e| e.version_num).unwrap_or(0);
            // Resolve against the filesystem before any read/write: the lexical `is_safe_relative_path` above rejects `..`,
            // but a symlinked intermediate directory would redirect the write
            // outside the root. Skip + count the row, never abort the page.
            let Some(full_path) =
                crate::path_guard::resolved_target_within_root(&self.watch_dir, path)
            else {
                report.skipped.push((
                    path.to_string(),
                    "path escapes the sync root after symlink resolution",
                ));
                continue;
            };
            let disk = match tokio::fs::metadata(&full_path).await {
                Ok(m) if m.is_file() => Some(
                    fauna_core::chunker_stream::content_hash_streaming(&full_path).with_context(
                        || format!("hashing local {}", fauna_core::log_redact::log_path(path)),
                    )?,
                ),
                _ => None,
            };

            match judge_materialization(&LocalPathState { entry, disk }, &peer_manifest) {
                MaterializeVerdict::AlreadyCurrent => {
                    self.db.mark_share_overlay_materialized(path, None)?;
                    report.already_current += 1;
                }
                MaterializeVerdict::Skip(reason) => {
                    report.skipped.push((path.to_string(), reason));
                }
                MaterializeVerdict::Write => {
                    let bytes = match fauna_core::file_download::download_file_bytes_by_manifest(
                        fetcher,
                        &self.peer_row_keys(change),
                        peer_manifest,
                        change.content_key_version,
                        path,
                    )
                    .await
                    {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!(
                                path = %fauna_core::log_redact::log_path(path),
                                error = %e,
                                "peer materialization fetch failed"
                            );
                            report.skipped.push((path.to_string(), "peer fetch failed"));
                            // The one skip a retry can cure: hold the cursor
                            // below it so the peer serves this row again.
                            if row.sequenced {
                                report.retry_floor = Some(
                                    report.retry_floor.map_or(change.seq, |f| f.min(change.seq)),
                                );
                            }
                            continue;
                        }
                    };
                    crate::atomic_write::atomic_write_file(&full_path, &bytes).await?;
                    // The merge base too, exactly as the nest-download apply
                    // does: without it, the nest's LATER row for this path
                    // reads the materialized state as unpublished local work
                    // and mints a bogus conflict instead of the reconcile's
                    // clean supersede. Best-effort like every save_base.
                    self.save_base(path, &bytes);
                    let content = fauna_core::data::ContentHash::of_raw(&bytes);
                    let local_mtime = tokio::fs::metadata(&full_path)
                        .await
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    self.db.upsert_entry(
                        path,
                        Some(content),
                        None, // remote_hash: the nest has not served this head
                        Some(peer_manifest),
                        crate::db::SyncState::Synced,
                        local_mtime,
                        created_at_ms_to_unix_secs(change.created_at),
                        bytes.len() as i64,
                        entry_version + 1,
                        change.content_key_version,
                    )?;
                    // Deliberately NOT stamped: `recorded_content_hash` (the
                    // dehydration proof) — see the fn doc's fourth door.
                    self.db.mark_share_overlay_materialized(
                        path,
                        Some(&hex::encode(content.digest())),
                    )?;
                    report.materialized += 1;
                }
            }
        }
        self.retain_relayed_rows(&reader, &relayable, true);
        self.retain_relayed_rows(&reader, &relayable_pending, false);
        Ok(report)
    }

    /// One accepted create/modify on an on-demand replica, after its overlay
    /// row landed: judge the path ([`crate::share_landing::judge_on_demand_landing`])
    /// and land by the verdict — a wanted body into the kept root through the
    /// owned tree's own landing operation, a new path as a placeholder row,
    /// or nothing.
    ///
    /// The landed body's row is committed exactly as the resident landing
    /// commits it (`Synced`, the peer manifest as its head, **no** dehydration
    /// proof), so [`Self::holds_provisional_peer_body`] can tell it from a
    /// write intent and the reconcile's confirm is what later frees it.
    #[cfg(feature = "p2p-share")]
    #[allow(clippy::too_many_arguments)] // one row's whole context; a struct would only rename it
    async fn land_peer_row_on_demand(
        &self,
        landing: &OnDemandLanding<'_>,
        sequenced: bool,
        change: &fauna_protocol::sync::SyncChange,
        path: &str,
        peer_manifest: ContentHash,
        prior_overlay: Option<&crate::db::ShareOverlayRow>,
        fetcher: &dyn fauna_core::file_download::BlobFetcher,
        report: &mut crate::peer_share_store::PeerIngestReport,
    ) -> Result<()> {
        use crate::provider_face::owned_tree::BodyRoot;
        use crate::share_landing::{
            Landing, OnDemandPathState, OnDemandVerdict, body_is_wanted, judge_on_demand_landing,
            landing_fits, landing_root,
        };

        let tree = landing.tree;
        let entry = self.db.get_entry(path)?;
        let entry_version = entry.as_ref().map(|e| e.version_num).unwrap_or(0);
        let has_row = entry
            .as_ref()
            .is_some_and(|e| e.state != SyncState::Deleted);
        let Some(kept_path) =
            crate::path_guard::resolved_target_within_root(tree.kept_root(), path)
        else {
            report.skipped.push((
                path.to_string(),
                "path escapes the sync root after symlink resolution",
            ));
            return Ok(());
        };
        let kept = match tokio::fs::metadata(&kept_path).await {
            Ok(m) if m.is_file() => Some(
                fauna_core::chunker_stream::content_hash_streaming(&kept_path).with_context(
                    || format!("hashing kept {}", fauna_core::log_redact::log_path(path)),
                )?,
            ),
            _ => None,
        };
        let kept_is_peer_landed = kept.as_ref().is_some_and(|k| {
            prior_overlay.is_some_and(|o| {
                o.materialized
                    && o.content_hash.as_deref() == Some(hex::encode(k.digest()).as_str())
            })
        });
        let wanted = body_is_wanted(
            &Landing::OnDemand {
                kept_root: tree.kept_root().to_path_buf(),
            },
            sequenced,
        );

        // The body did not land this pass. A path with no row still gets one —
        // metadata only — so the file lists; a wanted body is retried by the
        // next pass (an un-sequenced row is served on every tail page, and a
        // sequenced one is held below the cursor).
        let unlanded = |report: &mut crate::peer_share_store::PeerIngestReport,
                        reason: &'static str|
         -> Result<()> {
            if !has_row {
                self.record_peer_placeholder(tree, path, change, peer_manifest)?;
            }
            report.skipped.push((path.to_string(), reason));
            if sequenced {
                report.retry_floor =
                    Some(report.retry_floor.map_or(change.seq, |f| f.min(change.seq)));
            }
            Ok(())
        };

        match judge_on_demand_landing(
            &OnDemandPathState {
                entry,
                kept,
                kept_is_peer_landed,
            },
            wanted,
            &peer_manifest,
        ) {
            OnDemandVerdict::AlreadyCurrent => {
                self.db.mark_share_overlay_materialized(path, None)?;
                report.already_current += 1;
            }
            OnDemandVerdict::Skip(reason) => report.skipped.push((path.to_string(), reason)),
            OnDemandVerdict::RowOnly { record_placeholder } => {
                if record_placeholder {
                    self.record_peer_placeholder(tree, path, change, peer_manifest)?;
                }
            }
            OnDemandVerdict::Land => {
                // Every body this build wants is un-sequenced, and so takes
                // the kept root. The cache-root landing (a sequenced body the
                // user asked for) is the wanted-body slice's.
                if landing_root(sequenced) != BodyRoot::Kept {
                    report
                        .skipped
                        .push((path.to_string(), "cache-root landing is not built"));
                    return Ok(());
                }
                // The floor, before any byte is opened (the row's own size)
                // and again on the bytes that actually arrived.
                if !landing_fits((landing.free_space)(), change.size_bytes.max(0) as u64) {
                    report.storage_limited = true;
                    return unlanded(report, "storage floor");
                }
                let bytes = match fauna_core::file_download::download_file_bytes_by_manifest(
                    fetcher,
                    &self.peer_row_keys(change),
                    peer_manifest,
                    change.content_key_version,
                    path,
                )
                .await
                {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            path = %fauna_core::log_redact::log_path(path),
                            error = %e,
                            "peer landing fetch failed"
                        );
                        return unlanded(report, "peer fetch failed");
                    }
                };
                if !landing_fits((landing.free_space)(), bytes.len() as u64) {
                    report.storage_limited = true;
                    return unlanded(report, "storage floor");
                }
                let staged = tree.temp_path(BodyRoot::Kept)?;
                tokio::fs::write(&staged, &bytes).await?;
                let landed = tree.land_body(self, path, &staged, BodyRoot::Kept)?;
                // The merge base and the row, exactly as the resident landing
                // commits them (see `ingest_peer_share_rows_landing`).
                self.save_base(path, &bytes);
                let content = ContentHash::of_raw(&bytes);
                let local_mtime = tokio::fs::metadata(&landed)
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                self.db.upsert_entry(
                    path,
                    Some(content),
                    None, // remote_hash: the nest has not served this head
                    Some(peer_manifest),
                    SyncState::Synced,
                    local_mtime,
                    created_at_ms_to_unix_secs(change.created_at),
                    bytes.len() as i64,
                    entry_version + 1,
                    change.content_key_version,
                )?;
                // Deliberately NOT stamped: `recorded_content_hash` — the body
                // stays in the kept root until the nest confirms it.
                self.db
                    .mark_share_overlay_materialized(path, Some(&hex::encode(content.digest())))?;
                report.materialized += 1;
            }
        }
        Ok(())
    }

    /// Record a peer-served row's path as a placeholder — metadata only — on
    /// a replica that holds no row for it, so the file lists before its bytes
    /// are anywhere this device can fetch them. The row is the fold's own
    /// shape ([`Self::record_placeholders_from_changes`]); the tree then drops
    /// any cache-root body under it.
    #[cfg(feature = "p2p-share")]
    fn record_peer_placeholder(
        &self,
        tree: &crate::provider_face::owned_tree::OwnedTree,
        path: &str,
        change: &fauna_protocol::sync::SyncChange,
        manifest: ContentHash,
    ) -> Result<()> {
        let mtime = created_at_ms_to_unix_secs(change.created_at);
        // A 0-byte file is present-and-empty (the fold's own arm).
        let (local_hash, local_mtime) = if change.size_bytes == 0 {
            (Some(ContentHash::of_raw(b"")), mtime)
        } else {
            (None, 0)
        };
        self.db.upsert_entry(
            path,
            local_hash,
            None,
            Some(manifest),
            SyncState::Placeholder,
            local_mtime,
            mtime,
            change.size_bytes,
            1,
            change.content_key_version,
        )?;
        tree.record_placeholder(self, path)
    }

    /// Is the body at `relative_path` one the share plane landed from a peer
    /// and the nest has not confirmed — the path's overlay row says its bytes
    /// were landed, and the disk still hashes to them? Such a body is not a
    /// write intent (`provider_face::ProviderEngine::holds_provisional_peer_body`).
    /// Once the user writes over it the hash moves and it is an ordinary edit.
    pub fn holds_provisional_peer_body(&self, relative_path: &str) -> Result<bool> {
        let Some(overlay) = self.db.get_share_overlay(relative_path)? else {
            return Ok(false);
        };
        let (true, Some(landed)) = (overlay.materialized, overlay.content_hash) else {
            return Ok(false);
        };
        let full_path = self.watch_dir.join(relative_path);
        if !full_path.is_file() {
            return Ok(false);
        }
        let disk = fauna_core::chunker_stream::content_hash_streaming(&full_path)?;
        Ok(hex::encode(disk.digest()) == landed)
    }

    /// Advance this set's pull cursor for `peer_hex` (forward-only) and read
    /// the resulting value back — [`EngineCommand::ShareIngest`]'s epilogue.
    ///
    /// [`EngineCommand::ShareIngest`]: crate::always_resident::EngineCommand::ShareIngest
    #[cfg(feature = "p2p-share")]
    pub fn advance_share_pull_cursor(&self, peer_hex: &str, cursor: i64) -> Result<i64> {
        self.db.advance_share_pull_cursor(peer_hex, cursor)?;
        self.db.share_pull_cursor(peer_hex)
    }

    /// This set's pull cursor for `peer_hex` (`0` = never pulled).
    #[cfg(feature = "p2p-share")]
    pub fn share_pull_cursor(&self, peer_hex: &str) -> Result<i64> {
        self.db.share_pull_cursor(peer_hex)
    }

    /// The serve-side byte half's MANIFEST read (slice E — `p2p.md` § Built —
    /// the serve/pull core, the row-half finding's byte complement): answer a
    /// manifest fetch from the `own_manifests` retention, falling back to an
    /// in-memory re-derivation for content recorded before retention landed.
    ///
    /// `Ok(None)` is the ordinary "this replica cannot produce it" answer the
    /// `ShareStore` contract expects — multi-source covers it. Every served
    /// manifest is indexed into the serve memo so its chunks become
    /// range-servable ([`Self::share_chunk_body`]).
    #[cfg(feature = "p2p-share")]
    pub async fn share_manifest_bytes(&self, manifest_hash: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let hex_hash = hex::encode(manifest_hash);
        if let Some(retained) = self.db.retained_manifest(&hex_hash)? {
            // Self-verify: the key IS the bytes' hash. A mismatch is store
            // corruption — refuse the row and let the fallback (or another
            // source) answer, never serve bytes that would fail the puller's
            // own check anyway.
            if ContentHash::of_raw(&retained.bytes).digest() == *manifest_hash {
                crate::peer_share_store::index_share_manifest(
                    &self.share_serve_memo,
                    *manifest_hash,
                    &retained.path,
                    retained.content_key_version,
                    &retained.bytes,
                    self.content_keys.as_ref(),
                );
                return Ok(Some(retained.bytes));
            }
            tracing::warn!(
                manifest = %hex_hash,
                "retained manifest bytes fail their own hash; ignoring the row"
            );
        }
        self.reseal_share_manifest_fallback(manifest_hash).await
    }

    /// In-memory re-derivation for a manifest whose retention row is missing
    /// — retention is best-effort at every upload site, so a failed write
    /// leaves a recorded head with no retained bytes: find the entry whose head
    /// names it, re-seal the local plaintext under the RECORDED generation (the
    /// deterministic seal reproduces the exact bytes), verify, retain for next
    /// time. Bounded by `peer_share_store::SHARE_RESEAL_IN_MEMORY_MAX` — a file
    /// past the cap answers `None` with the limit named (re-recording
    /// repopulates the retention and lifts the cap for that file).
    #[cfg(feature = "p2p-share")]
    async fn reseal_share_manifest_fallback(
        &self,
        manifest_hash: &[u8; 32],
    ) -> Result<Option<Vec<u8>>> {
        let target = ContentHash::from_digest_raw(*manifest_hash);
        let Some(keys) = self.content_keys.as_ref() else {
            // An unbound (owner-only) engine serves no cross-user set.
            return Ok(None);
        };
        for entry in self.db.entries_by_manifest(&target)? {
            let Some(version) = entry.content_key_version else {
                continue; // an unversioned head is an owner-only head — not this corpus
            };
            let Some(secret) = keys.key_for(version).copied() else {
                continue; // generation no longer retained
            };
            let Some(sealed) = crate::peer_share_store::rederive_manifest_from_disk(
                &crate::share_body::TreeBodySource::new(self.watch_dir.clone()),
                &entry.path,
                secret,
                version,
                &target,
            )
            .await?
            else {
                continue;
            };
            // Retain so the next fetch is a lookup (best-effort, like the
            // upload-site retention).
            if let Err(e) = self.db.retain_manifest(
                &hex::encode(manifest_hash),
                &sealed.manifest_bytes,
                &entry.path,
                Some(version),
            ) {
                tracing::warn!(error = %e, "backfill manifest retention failed");
            }
            crate::peer_share_store::index_opened_share_manifest(
                &self.share_serve_memo,
                *manifest_hash,
                &entry.path,
                Some(version),
                &sealed.manifest,
            );
            return Ok(Some(sealed.manifest_bytes));
        }
        Ok(None)
    }

    /// The serve-side byte half's CHUNK read (slice E): re-derive one chunk's
    /// sealed body from a plaintext **range** read + the recorded generation,
    /// through the one per-chunk seal pipeline (`seal::seal_chunk_body`), and
    /// verify it hashes to the requested store key before serving.
    ///
    /// Manifest-anchored: a store key outside the serve memo answers
    /// `Ok(None)` — the shared download walk always fetches the manifest
    /// first, so every honest puller indexes before it pulls. A drifted file
    /// (range no longer hashes to the manifest's plaintext anchor) refuses
    /// early, same answer.
    #[cfg(feature = "p2p-share")]
    pub async fn share_chunk_body(&self, store_key: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let Some(hit) = self
            .share_serve_memo
            .lock()
            .expect("share serve memo poisoned")
            .chunk(store_key)
        else {
            return Ok(None);
        };
        let Some(version) = hit.content_key_version else {
            return Ok(None); // sealed share corpora always carry a generation
        };
        let Some(secret) = self
            .content_keys
            .as_ref()
            .and_then(|k| k.key_for(version))
            .copied()
        else {
            return Ok(None);
        };
        crate::peer_share_store::chunk_body_from_hit(
            &crate::share_body::TreeBodySource::new(self.watch_dir.clone()),
            &self.share_serve_memo,
            &hit,
            secret,
            store_key,
        )
        .await
    }

    /// Record that the body now at `relative_path` is `manifest`'s content, in
    /// the store-key index the serve reads (`held_chunks`). Called where the
    /// seat seals an upload and where it applies a download — the two places
    /// the manifest is in hand. Best-effort like the manifest retention: a
    /// lost row costs only the serveability of that body until it is next
    /// sealed or applied.
    fn index_held_body(
        &self,
        relative_path: &str,
        manifest: &fauna_core::chunk::ChunkManifest,
        content_key_version: Option<u64>,
    ) {
        let Some(chunks) = crate::serve_core::held_chunks_of(manifest) else {
            return;
        };
        if let Err(e) = self
            .db
            .index_held_body(relative_path, content_key_version, &chunks)
        {
            tracing::warn!(
                error = %e,
                path = %fauna_core::log_redact::log_path(relative_path),
                "store-key index write failed; the body is unservable until re-indexed"
            );
        }
    }

    /// Serve one stored chunk from the body this seat holds — the engine's one
    /// serve path (`file-sync.md` § Relay serving → *The seat serves from the
    /// file, through one serve core*).
    ///
    /// The key resolves through this folder's store-key index to a path and a
    /// plaintext range; the core reads it, checks it, seals it under the root
    /// the upload would have used, and serves it only when the result is
    /// `store_key`. `Ok(None)` — never an error — for a key the index does not
    /// name, a path whose row is a placeholder, a body that changed, or a
    /// reseal that does not reproduce the key. Nothing is fetched from the nest
    /// on this path, ever.
    pub async fn serve_chunk(&self, store_key: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let body = crate::share_body::TreeBodySource::new(self.watch_dir.clone());
        for row in self.db.held_chunks(store_key)? {
            let not_held = self.db.get_entry(&row.path)?.is_some_and(|e| {
                matches!(
                    e.state,
                    SyncState::Placeholder | SyncState::Deleted | SyncState::LocallyDeleted
                )
            });
            if not_held {
                continue;
            }
            for root in self.serve_seal_roots(row.content_key_version) {
                if let Some(served) =
                    crate::serve_core::serve_held_chunk(&body, &row.path, &row.chunk, root.as_ref())
                        .await?
                {
                    return Ok(Some(served));
                }
            }
        }
        Ok(None)
    }

    /// Answer one relay ask the host routed to this folder's engine: serve the
    /// key from the bound file and `POST` it, or `DELETE` when this seat holds
    /// none (`file-sync.md` § Relay serving, step (3)). A serve that errs — a
    /// DB or seal failure — declines like a miss: the relay moves on to the
    /// next seat rather than waiting out its deadline on this one.
    pub async fn answer_relay_ask(&self, ask: crate::relay_seat::ServeAsk) {
        let served = match self.serve_chunk(&ask.store_key).await {
            Ok(served) => served,
            Err(e) => {
                tracing::warn!(error = %e, "relay serve failed; declining the ask");
                None
            }
        };
        if let Err(e) = self
            .client
            .answer_relay_ask(ask.request_id, served.as_deref())
            .await
        {
            tracing::debug!(error = %e, "relay answer did not reach the nest");
        }
    }

    /// The roots a held chunk may have sealed under, by the rule
    /// [`Self::upload_seal_root`] applies: the content key of the recorded
    /// generation for a content-keyed body; for an unversioned one, the
    /// plaintext arm of a public folder and the owner's convergent root. Each
    /// candidate is checked against the key asked for, never trusted, so a
    /// folder whose posture moved since the body was indexed still serves
    /// whatever its rows can honestly reproduce.
    fn serve_seal_roots(&self, content_key_version: Option<u64>) -> Vec<Option<[u8; 32]>> {
        if let Some(version) = content_key_version {
            return self
                .content_keys
                .as_ref()
                .and_then(|k| k.key_for(version))
                .map(|k| vec![Some(*k)])
                .unwrap_or_default();
        }
        let mut roots = Vec::with_capacity(2);
        if self.is_public_audience() {
            roots.push(None);
        }
        if let Some(key) = self.effective_backup_key() {
            roots.push(Some(key.convergent_chunk_root()));
        }
        roots
    }

    /// Park this engine if `e` is the authoritative nest's write-grant refusal
    /// (D4 — `file-sync.md` § Multi-writer shared sets).
    ///
    /// Called on **both** record planes, because a demotion folds differently on
    /// each: the cross-nest relay passes the home nest's `federation.forbidden`
    /// through, while same-nest `resolve_writable_folder` folds it into
    /// `sync.not_found` (ST-RES-1). One shape covers both — which is what retires
    /// the standing "a stale mapping survives a same-nest demotion" residual
    /// rather than solving revocation only for the cross-nest case.
    ///
    /// Everything else — transport faults, `peer_nest_outdated`, the
    /// self-healing `device_unregistered` — is left retryable and untouched.
    pub(crate) fn note_access_refusal<E: fauna_protocol::RpcErrorClass + std::fmt::Display>(
        &self,
        folder: &str,
        e: &E,
    ) {
        if fauna_client_sync::is_access_revoked(e) && self.access_gate.revoke() {
            tracing::warn!(
                folder = %fauna_core::log_redact::log_folder_name(folder),
                error = %e,
                foreign = self.foreign_routing().is_some(),
                "change record refused: this actor may no longer write this folder; \
                 parking the engine (access revoked). Local files and pending local \
                 edits are left untouched."
            );
        }
    }

    /// Generate, seal, and upload a thumbnail for `data`, returning the hex
    /// content hash of the stored thumbnail blob to record on the folder
    /// member.
    ///
    /// `None` when no thumbnail is warranted: not an image / ≤300px (the
    /// `process_media` thumbnailer declines), or the set has no owner
    /// `BackupKey` — a shared content-key or plaintext set, deferred. Sync
    /// chunks are encrypted at
    /// rest and the nest never holds the `BackupKey`, so the thumbnail is
    /// generated **client-side** here (where the plaintext lives) and sealed
    /// under the owner `BackupKey` exactly like the Media-library producer
    /// (`fauna_media::pipeline::seal_thumbnail_only` + `Audience::Library`), so a
    /// client fetches it direct-by-hash and decrypts it uniformly.
    ///
    /// Best-effort: a seal/upload error logs and returns `None` so the file's
    /// change still records (graceful degrade, mirroring `MediaMachine`).
    async fn maybe_upload_thumbnail(&self, data: &[u8]) -> Option<String> {
        // Client-only path: a segment-backup engine (the `SourceNest` variant)
        // uploads opaque segment bytes and thumbnails nothing, and holds no
        // client `BackupKey` to seal a library blob under.
        let backup_key = self.backup_key.as_ref()?.client_key()?.clone();
        let audience = fauna_media::audience::Audience::Library { backup_key };
        let sealed = fauna_media::pipeline::seal_thumbnail_only(data, &audience)?;
        match self
            .client
            .upload_blob_multipart(&sealed.sidecar.to_dag_cbor(), &sealed.bytes)
            .await
        {
            Ok(hash) => Some(hash),
            Err(e) => {
                tracing::warn!(error = %e, "failed to upload synced-file thumbnail; recording without one");
                None
            }
        }
    }

    /// The [`maybe_upload_thumbnail`](Self::maybe_upload_thumbnail) twin for the
    /// **streaming** upload path (files ≥ 64 MiB): it reads the source image
    /// **incrementally from disk** via `seal_thumbnail_only_from_path` rather
    /// than from an in-memory `&[u8]`, so the O(MAX_CHUNK) streaming upload never
    /// loads the whole file to thumbnail it.
    ///
    /// Same gating + best-effort contract as `maybe_upload_thumbnail`: `None`
    /// when no thumbnail is warranted (not an image / ≤300px, or no owner
    /// `BackupKey` — a shared content-key or plaintext set, still deferred), and
    /// a seal/upload error logs and returns `None` so the file's change still
    /// records.
    async fn maybe_upload_thumbnail_from_path(&self, path: &std::path::Path) -> Option<String> {
        // Client-only path: a segment-backup engine (the `SourceNest` variant)
        // uploads opaque segment bytes and thumbnails nothing, and holds no
        // client `BackupKey` to seal a library blob under.
        let backup_key = self.backup_key.as_ref()?.client_key()?.clone();
        let audience = fauna_media::audience::Audience::Library { backup_key };
        let sealed = fauna_media::pipeline::seal_thumbnail_only_from_path(path, &audience)?;
        match self
            .client
            .upload_blob_multipart(&sealed.sidecar.to_dag_cbor(), &sealed.bytes)
            .await
        {
            Ok(hash) => Some(hash),
            Err(e) => {
                tracing::warn!(error = %e, "failed to upload synced-file thumbnail (streaming); recording without one");
                None
            }
        }
    }

    /// Seam B — thumbnail **download-backfill** for a synced file recorded without one.
    ///
    /// A file whose `sync_changes` row was recorded without a thumbnail (a build
    /// without the thumbnailer, or a failed thumbnail upload) carries
    /// `thumbnail_hash = None`. When we
    /// download such a file we hold its plaintext, so we generate the missing
    /// thumbnail here and re-record the file with it — additively, no data loss.
    ///
    /// Re-records via `changes_record` carrying the **same** `manifest_hash` +
    /// `size_bytes` as the existing row so the nest's quota delta stays 0
    /// (`record_sync_change_metered`'s supersede logic) and only
    /// `thumbnail_hash` changes; the append-only log + latest-per-path
    /// `get_files_for_folder` projection surface it on the next
    /// `fauna.media.list`. The caller invokes this only for the batch-latest
    /// **live** state of a path (never a delete — [`Self::thumbnail_backfill_targets`]),
    /// so the re-record cannot resurrect a deleted file.
    ///
    /// Owner-encrypted sets only: `maybe_upload_thumbnail` returns `None` for a
    /// non-image / ≤300px file or a set with no owner `BackupKey` (shared
    /// content-key / plaintext sets are deferred). Best-effort — any
    /// seal/upload/record error logs and is swallowed so a backfill failure
    /// never breaks the download (a read-only device whose re-record the nest
    /// rejects simply records no backfill; a write-capable peer will).
    async fn maybe_backfill_thumbnail(
        &self,
        relative_path: &str,
        plaintext: &[u8],
        manifest_hash: ContentHash,
        size_bytes: i64,
        content_key_version: Option<u64>,
    ) {
        let Some(folder) = self.folder.clone() else {
            return; // a folder-less engine (the restore walk) has no folder to re-record into
        };
        let Some(thumbnail_hash) = self.maybe_upload_thumbnail(plaintext).await else {
            return;
        };
        let manifest_hex = hex::encode(manifest_hash.digest());
        match self
            .record_change(
                &folder,
                relative_path,
                Some(&manifest_hex),
                size_bytes,
                "modify",
                content_key_version,
                Some(&thumbnail_hash),
                CausalStamp::unknown(),
            )
            .await
        {
            Ok(_) => tracing::info!(
                path = %fauna_core::log_redact::log_path(relative_path),
                "backfilled thumbnail for a synced file recorded without one"
            ),
            Err(e) => tracing::warn!(
                path = %fauna_core::log_redact::log_path(relative_path),
                error = %e,
                "thumbnail backfill re-record failed; the file lists without a thumbnail"
            ),
        }
    }

    /// The per-path targets for the thumbnail download-backfill (Seam B): for a
    /// pull batch, the paths whose **batch-latest** change is a **live**
    /// create/modify (`manifest_hash` present) that still lacks a
    /// `thumbnail_hash`, mapped to that change's `(seq, size_bytes)`.
    ///
    /// Keying on the batch-latest state per path is the live-only guard: a path
    /// deleted (or superseded) later in the same pull is excluded, so a
    /// catching-up device that pulls `[create X, delete X]` in one batch (the
    /// realistic pull-since-0 shape) never re-records the create and resurrects
    /// the file. `is_delete` mirrors the nest's own signal
    /// (`record_sync_change_metered`): a `"delete"` change_type **or** a null
    /// manifest. The caller re-records with the returned `size_bytes` + the
    /// downloaded `manifest_hash` so the nest's quota delta stays 0.
    pub(crate) fn thumbnail_backfill_targets(
        changes: &[fauna_protocol::sync::SyncChange],
    ) -> std::collections::HashMap<String, BackfillTarget> {
        use std::collections::HashMap;
        // The highest-seq change per path wins (the path's batch-latest state).
        let mut latest: HashMap<&str, &fauna_protocol::sync::SyncChange> = HashMap::new();
        for change in changes {
            let Some(path) = change.path.as_deref() else {
                continue;
            };
            match latest.get(path) {
                Some(prev) if prev.seq >= change.seq => {}
                _ => {
                    latest.insert(path, change);
                }
            }
        }
        latest
            .into_iter()
            .filter_map(|(path, change)| {
                let is_delete = change.change_type.eq_ignore_ascii_case("delete")
                    || change.manifest_hash.is_none();
                (!is_delete && change.thumbnail_hash.is_none()).then(|| {
                    (
                        path.to_string(),
                        BackfillTarget {
                            seq: change.seq,
                            size_bytes: change.size_bytes,
                        },
                    )
                })
            })
            .collect()
    }

    /// The highest `seq` seen per path across a `changes.list` batch — i.e. each
    /// path's batch-latest state.
    ///
    /// The log may carry several changes for one path in a single batch, so a
    /// consumer that must act on a path's *final* state (rather than on each
    /// intermediate change) folds through this first. Mirrors the "highest-seq
    /// change per path wins" rule [`Self::thumbnail_backfill_targets`] and
    /// [`Self::record_placeholders_from_changes`] already fold by.
    pub(crate) fn batch_latest_seq_by_path(
        changes: &[fauna_protocol::sync::SyncChange],
    ) -> std::collections::HashMap<&str, i64> {
        let mut latest: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
        for change in changes {
            let Some(path) = change.path.as_deref() else {
                continue;
            };
            let entry = latest.entry(path).or_insert(change.seq);
            if change.seq > *entry {
                *entry = change.seq;
            }
        }
        latest
    }

    /// The highest `seq` per path among the changes in this batch that will
    /// actually decide what the path holds — every delete, and every **peer**
    /// create/modify.
    ///
    /// This is the fold the create/modify arm supersedes by, and it differs
    /// from [`Self::batch_latest_seq_by_path`] in exactly one way: a
    /// **self-echoed create/modify is excluded**, because it applies no
    /// content. A self-echo downloads nothing and writes nothing — it only
    /// re-bases ([`Self::apply_self_echo`]) — so there is no sense in which it
    /// supersedes a peer's change; the two are *concurrent*, which is the very
    /// definition of a conflict (`file-sync.md` § Conflicts).
    ///
    /// Counting it was a silent, permanent data-loss bug (measured
    /// 2026-08-02): a device that published an edit and then pulled a batch
    /// holding both its own echo (higher seq) and a peer's genuinely concurrent
    /// edit skipped the peer's row as "superseded" — no download, no three-way
    /// merge, no conflict row, no error — and because the anchor advances past
    /// a skipped change (`set_anchor(max_seq)` runs whatever the arms did), the
    /// peer's edit was never redelivered. Two app seats editing 80 ms apart
    /// converged on one seat's bytes with the other's edit gone.
    ///
    /// Deletes stay in the fold whoever recorded them: a self-echoed delete
    /// *does* decide the path's end state (the delete arm is deliberately not
    /// gated on `is_self_echo` — see that arm), so downloading a peer version
    /// first would be writing bytes the same batch is about to remove.
    /// Did **this seat** write this row? The peer-vs-self-echo question every arm
    /// in [`Self::apply_remote_changes`] asks, in one place.
    ///
    /// **The device id alone does not answer it.** `sync_devices` is keyed
    /// `(actor_id, device_id)`, so a device id is unique only *within* an actor —
    /// two actors sharing a set may carry the same one. Answering "mine" for a
    /// peer's row is not a cosmetic misread: the row applies nothing and
    /// [`Self::apply_remote_changes`]'s `set_anchor(max_seq)` still moves past it,
    /// so no later pull re-offers it and the peer's file is lost permanently.
    /// Measured 2026-08-19: a writer member read the
    /// owner's upload as its own echo and never materialized it — the same
    /// silent-skip-plus-advancing-anchor shape recorded at
    /// [`Self::content_superseding_seq_by_path`] for row 20.
    ///
    /// An **absent** `author_actor_id` keeps the historical device-only answer
    /// rather than inventing a mismatch: the nest deliberately strips authorship on
    /// the public plane (`bins/fauna-nest/src/folder_public.rs`), and a reader there
    /// has no self-echoes to detect anyway.
    fn row_is_own(
        change: &fauna_protocol::sync::SyncChange,
        our_device: &str,
        our_actor: &str,
    ) -> bool {
        change.device_id.as_deref() == Some(our_device)
            && change
                .author_actor_id
                .as_deref()
                .is_none_or(|a| a == our_actor)
    }

    pub(crate) fn content_superseding_seq_by_path<'a>(
        changes: &'a [fauna_protocol::sync::SyncChange],
        our_device: &str,
        our_actor: &str,
    ) -> std::collections::HashMap<&'a str, i64> {
        let mut latest: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
        for change in changes {
            let Some(path) = change.path.as_deref() else {
                continue;
            };
            let is_delete = change.change_type.eq_ignore_ascii_case("delete");
            let is_self_echo = Self::row_is_own(change, our_device, our_actor);
            if is_self_echo && !is_delete {
                continue;
            }
            let entry = latest.entry(path).or_insert(change.seq);
            if change.seq > *entry {
                *entry = change.seq;
            }
        }
        latest
    }

    /// Fetch one set's changes from the nest over the
    /// `fauna.sync.changes.list` WS-RPC kind — the nest serves a change log per
    /// folder, never an actor-wide one.
    ///
    /// **Cross-nest:** a foreign-routed engine carries
    /// `nest_url`+`channel_id`, so its own nest relays the read to the set's home
    /// nest (`fauna.federation.folder.changes.fetch`) instead of answering
    /// locally — where a foreign set has no row and the poll would come back
    /// empty forever, leaving a bound cross-nest folder silently never pulling.
    /// The additive fields are permitted here by the ratified client-kind wire
    /// rule because the local path fails visibly (`not_found`) for a foreign set.
    async fn fetch_changes(
        &self,
        folder: &str,
        since: i64,
    ) -> Result<Vec<fauna_protocol::sync::SyncChange>> {
        Ok(self.fetch_change_batch(folder, since, true).await?.changes)
    }

    /// [`Self::fetch_changes`] with what the verify step decided about the
    /// batch — the rows it admitted plus how far the cursor may advance
    /// ([`FetchedBatch`]). `verify = false` skips the reader's judgement: only
    /// for a caller that checks every row it acts on more strictly itself
    /// ([`Self::rerecord_under_live_nonce`], which re-signs and compares).
    pub(crate) async fn fetch_change_batch(
        &self,
        folder: &str,
        since: i64,
        verify: bool,
    ) -> Result<FetchedBatch> {
        let (changes, signer_certs) = self.list_changes(folder, since).await?;
        // What the nest actually served, BEFORE the verify and the sealed-label
        // open — the only number that separates "the nest returned nothing"
        // from "the nest returned rows this holder then skipped".
        // `reconciliation complete` cannot: it summarises the LOCAL scan, so it
        // reports zeros for both.
        let served = changes.len();
        let served_max = changes.iter().map(|c| c.seq).max();
        // Verified as SERVED, before any receiver rewrite: the watermark bound
        // below rewrites `derived_through`, a signed field.
        let (changes, held_from) = if verify {
            self.verify_served_rows(changes, &signer_certs).await
        } else {
            (changes, None)
        };
        // The cursor passes a refused row (it is absent) but never a held one.
        let advance_to = match held_from {
            Some(h) => Some(h - 1).filter(|&a| a > since),
            None => served_max,
        };
        let changes = self.finish_fetched(changes, folder, since, served);
        Ok(FetchedBatch {
            changes,
            advance_to,
            held: held_from.is_some(),
        })
    }

    /// The raw `fauna.sync.changes.list` read (or its cross-nest relay): the
    /// rows as served, plus the reply's `signer_certs` side table.
    async fn list_changes(
        &self,
        folder: &str,
        since: i64,
    ) -> Result<(
        Vec<fauna_protocol::sync::SyncChange>,
        Vec<fauna_core::encoding::EmbedAsBytes>,
    )> {
        let reply = if let Some((home_nest_url, channel_id_hex)) = self.foreign_routing() {
            use fauna_protocol::RpcRequester;
            let reply: fauna_protocol::sync::SyncChangesListReply = self
                .nest_client
                .request(
                    "fauna.sync.changes.list",
                    fauna_protocol::folders::addressed(
                        fauna_protocol::sync::SyncChangesListRequest {
                            folder: Some(folder.to_string()),
                            device_id: None,
                            nest_url: Some(home_nest_url),
                            channel_id: Some(channel_id_hex),
                            since,
                            name_hash: None,
                            ..Default::default()
                        },
                    ),
                )
                .await
                .map_err(|e| anyhow::anyhow!("cross-nest fauna.sync.changes.list: {e}"))?;
            reply
        } else {
            fauna_client_sync::SyncClient::new(self.nest_client.clone())
                .changes_list(Some(folder.to_string()), None, since)
                .await
                .context("fauna.sync.changes.list")?
        };
        Ok((reply.changes, reply.signer_certs))
    }

    /// Keep other writers' VERIFIED rows, byte-exact as served, with the cert
    /// each chained through — what the share leg relays so an N-member set
    /// offline-relays every member's rows, not only the relayer's (the
    /// relayed-row lift, `p2p.md` § *Peer-served change-row provenance*).
    /// Best-effort: the nest's log (or the serving peer) is the durable copy,
    /// so a failed write costs offline relay of one row, never the row.
    fn retain_relayed_rows(
        &self,
        reader: &fauna_protocol::sync_row_verify::RowReader,
        rows: &[([u8; 32], fauna_protocol::sync::SyncChange)],
        sequenced: bool,
    ) {
        for (actor, change) in rows {
            let signer_cert = change
                .signer_key
                .as_deref()
                .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok())
                .and_then(|k| reader.cert_for(actor, &k))
                .and_then(|c| fauna_protocol::encode_canonical(c).ok())
                .map(|b| b.to_vec());
            let retained = fauna_protocol::encode_canonical(change)
                .map_err(anyhow::Error::from)
                .and_then(|row| {
                    self.db.retain_relayed_change(&crate::db::RelayedChangeRow {
                        seq: sequenced.then_some(change.seq),
                        author_actor_id: hex::encode(actor),
                        path_hash: change.path_hash.to_ascii_lowercase(),
                        row: row.to_vec(),
                        signer_cert,
                    })
                });
            if let Err(e) = retained {
                tracing::warn!(
                    seq = change.seq,
                    error = %e,
                    "relayed-row retention failed; the row itself was consumed"
                );
            }
        }
    }

    /// Judge every served row ([`fauna_protocol::sync_row_verify::RowReader::judge`]) and
    /// keep the ones it admits, below the first held one. One roster read per
    /// batch that needs one, then a re-judge: a row by a non-owner met before
    /// any roster read, or (`mls-group-key-material.md` § M2 → *Writer-signed
    /// change records*, ruling (8)(e)) a row that verified and whose signed
    /// actor resolves to no writer on the roster last read — a newly granted
    /// writer's first rows. A read that fails changes nothing: the last
    /// roster stands and the refusal stands against it.
    /// Returns the admitted rows and the lowest held seq, if any.
    pub(crate) async fn verify_served_rows(
        &self,
        changes: Vec<fauna_protocol::sync::SyncChange>,
        signer_certs: &[fauna_core::encoding::EmbedAsBytes],
    ) -> (Vec<fauna_protocol::sync::SyncChange>, Option<i64>) {
        // The own nest, whichever nest the set's log lives on: an account's
        // succession statements land on its home nest.
        self.verify_served_rows_over(&self.nest_client, changes, signer_certs)
            .await
    }

    /// [`Self::verify_served_rows`] with the succession lookup's requester
    /// named — the seam its tier_1 pin doubles.
    pub(crate) async fn verify_served_rows_over<R>(
        &self,
        links: &R,
        changes: Vec<fauna_protocol::sync::SyncChange>,
        signer_certs: &[fauna_core::encoding::EmbedAsBytes],
    ) -> (Vec<fauna_protocol::sync::SyncChange>, Option<i64>)
    where
        R: fauna_protocol::RpcRequester,
        R::Error: fauna_protocol::RpcErrorClass,
    {
        use fauna_protocol::sync_row_verify::RowVerdict;
        let mut verdicts = self.judge_served_rows(&changes, signer_certs);
        if verdicts.iter().any(RowVerdict::wants_roster_read) {
            self.refresh_reader_roster().await;
            verdicts = self.judge_served_rows(&changes, &[]);
        }
        // Still unplaced after the roster: a retired identity of this account
        // the host attested nothing about (ruling (8)(b), source (ii)).
        if self.prove_own_links(links, &changes, &verdicts).await {
            verdicts = self.judge_served_rows(&changes, &[]);
        }
        let held_from = changes
            .iter()
            .zip(&verdicts)
            .filter(|(_, v)| matches!(v, RowVerdict::Held(_)))
            .map(|(c, _)| c.seq)
            .min();
        let (changes, verdicts): (Vec<_>, Vec<_>) = changes
            .into_iter()
            .zip(verdicts)
            .filter(|(c, _)| !held_from.is_some_and(|h| c.seq >= h))
            .unzip();
        let kept = self.admit_judged_rows(changes, verdicts);
        if let Some(held) = held_from {
            tracing::warn!(
                held_from = held,
                "fauna.sync.changes: a served row cannot be judged yet (no writer roster read, \
                 or no set nonce in custody) — the pull holds below it and retries"
            );
        }
        (kept, held_from)
    }

    /// Feed the reader a reply's `signer_certs` and judge each row, as served.
    fn judge_served_rows(
        &self,
        changes: &[fauna_protocol::sync::SyncChange],
        signer_certs: &[fauna_core::encoding::EmbedAsBytes],
    ) -> Vec<fauna_protocol::sync_row_verify::RowVerdict> {
        if !signer_certs.is_empty() {
            self.row_reader
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .ingest_certs(signer_certs);
        }
        let reader = self.row_reader.read().unwrap_or_else(|e| e.into_inner());
        changes.iter().map(|c| reader.judge(c)).collect()
    }

    /// What the reader's verdict means for each judged row — the one place a
    /// row enters this engine, shared by the pull's verify step and the head
    /// re-judge ([`Self::fold_passed_heads`]), so no row is folded around the
    /// per-signer bound: an admitted row's manifest signer is noted, its
    /// author rewritten to the identity it was signed as, and another
    /// writer's row retained for the p2p relay. Returns the admitted rows.
    fn admit_judged_rows(
        &self,
        changes: Vec<fauna_protocol::sync::SyncChange>,
        verdicts: Vec<fauna_protocol::sync_row_verify::RowVerdict>,
    ) -> Vec<fauna_protocol::sync::SyncChange> {
        use fauna_protocol::sync_row_verify::RowVerdict;
        let own = self.own_actor_id().0;
        let owns_set = self.owns_set();
        let mut kept = Vec::with_capacity(changes.len());
        let mut relayable = Vec::new();
        // Ruling (11)(b): a member's row verifying under no nonce this engine
        // holds says the owner re-minted the set's nonce — one ask per batch.
        let mut nonce_stale = false;
        for (mut change, verdict) in changes.into_iter().zip(verdicts) {
            let unattributed = verdict.unattributed();
            match verdict {
                RowVerdict::Refused(why) => {
                    nonce_stale |= !owns_set
                        && matches!(
                            why,
                            fauna_protocol::sync_writer_sig::ChangeVerifyError::SignatureInvalid
                        );
                    tracing::warn!(
                        seq = change.seq,
                        error = %why,
                        "fauna.sync.changes: a served row did not verify — skipped as absent \
                         (the cursor passes it; the path frontier does not)"
                    );
                    // A record this reader cannot attribute yet (ruling
                    // (8)(f)): the cursor is about to account for a head this
                    // device never read, so a later local edit of the path
                    // must not stamp itself as derived from it — the note a
                    // row that can never apply here already takes. The
                    // `path_hash` is the signed one (the row verified). It
                    // releases when the head re-judge folds the row, or a
                    // later row carries the path's frontier past it.
                    if unattributed {
                        self.causal()
                            .note_permanent_skip(Some(&change.path_hash), change.seq);
                    }
                }
                RowVerdict::Held(_) => {}
                // History (`writer-signed-change-records.md` ruling (11)(c)):
                // a version of the path, never a head — folded by nothing. The
                // cursor passes it, so a later stamp on the path must account
                // for a row this device never applied: the skip note. The head
                // re-judge folds CURRENT verdicts only, so this note is never
                // the licence it reads to fold a history delete.
                RowVerdict::History { .. } => {
                    self.causal()
                        .note_permanent_skip(Some(&change.path_hash), change.seq);
                }
                // Everything below reads the identity the row was SIGNED AS
                // (ruling (8)(c)) — what the served author used to be, back
                // when a row verified only if the two agreed.
                RowVerdict::Verified { signed_as, .. } => {
                    // Every open of the manifest is bounded by who signed its
                    // record (ruling (8)(c)): a retired identity's signature
                    // reaches only its own root and earlier ones, another
                    // writer's no owner root at all. A row signed as this
                    // account — current or retired — arms the owner-root arm.
                    if let Some(m) = change.manifest_hash.as_deref()
                        && let Ok(m) = Self::parse_manifest_hash(m)
                    {
                        self.note_manifest_signer(m, self.record_signer_of(&signed_as));
                        self.pending_head_signers
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(change.path_hash.to_ascii_lowercase(), (m, signed_as));
                        if owns_set && self.is_own_account(&signed_as) {
                            self.note_owner_signed(m);
                        }
                    }
                    if signed_as != own {
                        // Byte-exact as served: the relay keeps the stamp the
                        // nest sent, and its reader recovers the signer anew.
                        relayable.push((signed_as, change.clone()));
                    }
                    // The served author is no longer part of what verified
                    // (ruling (8)(a)), so every later read of it in this
                    // engine — the own-echo test first — gets the signed
                    // actor, never a stamp a nest could point at this seat.
                    change.author_actor_id = Some(hex::encode(signed_as));
                    kept.push(change);
                }
                RowVerdict::Exempt => kept.push(change),
            }
        }
        if !relayable.is_empty() {
            let reader = self.row_reader.read().unwrap_or_else(|e| e.into_inner());
            self.retain_relayed_rows(&reader, &relayable, true);
        }
        if nonce_stale {
            let request = self
                .custody_refetch_request
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(request) = request {
                request();
            }
        }
        kept
    }

    /// **The head re-judge's trigger** (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (8)(f)): compare what the
    /// reader admits by now — every identity a row may be signed as
    /// ([`RowReader::admitted_signers`](fauna_protocol::sync_row_verify::RowReader::admitted_signers)),
    /// and each predecessor root this host holds — against what it last
    /// admitted by, stored per set ([`SyncDb::reader_admitted_by`]). A
    /// **gain** marks the pass owed; a loss or a reorder never does, and a
    /// set with nothing stored (the first run of this build) owes one pass.
    ///
    /// The roster lives in memory only, so until this process has read it the
    /// stored tokens are only ever added to: a restart must not read the
    /// roster's writers as lost and then, one read later, as gained. Returns
    /// whether a pass is owed.
    fn note_reader_gain(&self) -> Result<bool> {
        let (signers, nonces, roster_read) = {
            let reader = self.row_reader.read().unwrap_or_else(|e| e.into_inner());
            let binding = reader.binding();
            let nonces: Vec<[u8; 32]> = binding
                .set_nonce
                .into_iter()
                .chain(binding.retired_set_nonces.iter().map(|(n, _)| *n))
                .collect();
            (reader.admitted_signers(), nonces, reader.roster_read())
        };
        let current: std::collections::BTreeSet<String> = signers
            .iter()
            .map(|id| format!("signer:{}", hex::encode(id)))
            // Every nonce the binding holds (`writer-signed-change-records.md`
            // ruling (11)(c)): a cut arriving — a live nonce re-minted, its
            // predecessor retired into the lineage — re-reads the passed heads
            // once. A head that turns history there is a loss, which the fold
            // never acts on.
            .chain(nonces.iter().map(|n| format!("nonce:{}", hex::encode(n))))
            // A root carried without its identity is offered to no row (the
            // per-signer bound), so it is nothing the reader admits by.
            .chain(
                self.predecessor_backup_keys
                    .iter()
                    .filter_map(|k| k.actor_id)
                    .map(|a| format!("root:{}", hex::encode(a.0))),
            )
            .collect();
        let stored = self.db.reader_admitted_by()?;
        let gained = stored
            .as_ref()
            .is_none_or(|stored| current.difference(stored).next().is_some());
        if gained {
            // Before the tokens: a crash between the two writes finds the
            // gain again, never a stored set that forgot it owed a pass.
            self.db.set_head_rejudge_owed(true)?;
        }
        // A nonce gained is a cut arriving on a running engine: the succession
        // take-over is owed too (ruling (11)(d), "when its reader's binding
        // gains a nonce"), run by the pull after the re-judge.
        let nonce_gained = match &stored {
            Some(stored) => current
                .difference(stored)
                .any(|token| token.starts_with("nonce:")),
            None => !nonces.is_empty(),
        };
        if nonce_gained {
            self.db.set_take_over_owed(true)?;
        }
        let next = match &stored {
            Some(stored) if !roster_read => stored.union(&current).cloned().collect(),
            _ => current,
        };
        if stored.as_ref() != Some(&next) {
            self.db.set_reader_admitted_by(&next)?;
        }
        self.db.head_rejudge_owed()
    }

    /// Run the head re-judge when one is owed — first thing in every pull, so
    /// the rows above the cursor still reach the ordinary fold in order. Never
    /// fails the pull: a pass that cannot finish stays owed and runs again.
    /// Run the re-record leg — the succession take-over its second arm — when
    /// a nonce gain marked it owed ([`Self::note_reader_gain`]); clear the
    /// marker only when the pass ran to the end. Never fails the pull.
    async fn take_over_if_owed(&self) {
        match self.db.take_over_owed() {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "succession take-over: reading the trigger state failed");
                return;
            }
        }
        match self.rerecord_under_live_nonce().await {
            Ok(rerecorded) => {
                if rerecorded > 0 {
                    tracing::info!(
                        rerecorded,
                        "succession take-over: re-recorded on a nonce gain"
                    );
                }
                if let Err(e) = self.db.set_take_over_owed(false) {
                    tracing::warn!(error = %format!("{e:#}"), "succession take-over: clearing the owed marker failed; the pass runs again");
                }
            }
            Err(e) => tracing::warn!(
                error = %format!("{e:#}"),
                "succession take-over: the pass failed — still owed, retried at the next pull"
            ),
        }
    }

    async fn rejudge_passed_heads_if_owed(&self, folder: &str) {
        match self.note_reader_gain() {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "head re-judge: reading the trigger state failed");
                return;
            }
        }
        match self.rejudge_passed_heads(folder).await {
            Ok(HeadRejudge {
                complete: true,
                folded,
            }) => {
                if let Err(e) = self.db.set_head_rejudge_owed(false) {
                    tracing::warn!(error = %format!("{e:#}"), "head re-judge: clearing the owed marker failed; the pass runs again");
                }
                if folded > 0 {
                    tracing::info!(
                        folded,
                        "head re-judge: folded head(s) the cursor had passed before their signer was admitted"
                    );
                }
            }
            Ok(HeadRejudge { folded, .. }) => tracing::info!(
                folded,
                "head re-judge: incomplete (a row cannot be judged or opened yet, or a fold deferred) — still owed"
            ),
            Err(e) => tracing::warn!(
                error = %format!("{e:#}"),
                "head re-judge: the pass failed part-way — still owed, retried at the next pull"
            ),
        }
    }

    /// **What the cursor already passed is judged again — by heads, never by
    /// replay** (ruling (8)(f)). Read the set's log from the start **up to
    /// this engine's own cursor and no further**, whole — every page — before
    /// folding anything, then [`Self::fold_passed_heads`]. Rows above the
    /// cursor are the ordinary pull's, in order, as always.
    ///
    /// Why never a replay through the ordinary fold, and never a head from
    /// above the cursor: the delete arm carries no frontier check and the
    /// cross-nest read is paged, so an old delete replayed a page ahead of
    /// the create that superseded it would unlink a live file with nothing
    /// left to re-offer it.
    pub(crate) async fn rejudge_passed_heads(&self, folder: &str) -> Result<HeadRejudge> {
        let cursor = self.db.get_anchor()?;
        if cursor <= 0 {
            return Ok(HeadRejudge {
                complete: true,
                folded: 0,
            });
        }
        let (rows, signer_certs) = self.read_log_through(folder, cursor).await?;
        self.fold_passed_heads(rows, &signer_certs, cursor).await
    }

    /// The set's log from the start through `through`, whole — every page —
    /// with the signer certs the pages carried. Unjudged.
    async fn read_log_through(
        &self,
        folder: &str,
        through: i64,
    ) -> Result<(
        Vec<fauna_protocol::sync::SyncChange>,
        Vec<fauna_core::encoding::EmbedAsBytes>,
    )> {
        let mut rows = Vec::new();
        let mut signer_certs = Vec::new();
        let mut since = 0i64;
        loop {
            let (page, page_certs) = self.list_changes(folder, since).await?;
            let Some(page_max) = page.iter().map(|c| c.seq).max() else {
                break;
            };
            anyhow::ensure!(
                page_max > since,
                "fauna.sync.changes.list served a page that does not advance past seq {since}"
            );
            signer_certs.extend(page_certs);
            rows.extend(page.into_iter().filter(|c| c.seq <= through));
            if page_max >= through {
                break;
            }
            since = page_max;
        }
        Ok((rows, signer_certs))
    }

    /// The fold half of [`Self::rejudge_passed_heads`], over the set's whole
    /// log up to `cursor`: per path, take the latest **non-retention** row the
    /// judge now admits, and fold it only when it is above everything this
    /// device has for the path.
    ///
    /// - *Admitted* goes through the verify step's own door
    ///   ([`Self::admit_judged_rows`]) and the label open under the row's
    ///   signer, so the per-signer root bound (ruling (8)(c)) holds here too.
    /// - *Above everything this device has* is the path's frontier **and its
    ///   edit-frontier**: a head at or below either is a duplicate (the
    ///   second knows this device's own acked edit before its echo is pulled).
    /// - **A delete head is folded only where this device noted the skip of
    ///   that very row** ([`crate::causal::CausalStore::skip_noted`]) — its
    ///   own record that it never read the delete. The frontiers cannot say
    ///   so: the delete arm advances none, so a delete this device already
    ///   applied reads as above them for ever, and the upload path marks a
    ///   re-created file `Synced` before its record is acked — folding the old
    ///   delete there would unlink bytes that exist nowhere else. A delete
    ///   refused before this build noted nothing and stays unfolded: a stale
    ///   file kept, never a live one lost. Once the ordinary delete arm has
    ///   spoken for a noted one, the note is released, so it is never folded
    ///   twice.
    /// - A head whose label has no root *yet* leaves the pass owed; one that
    ///   can never open here notes its skip, like the pull.
    pub(crate) async fn fold_passed_heads(
        &self,
        rows: Vec<fauna_protocol::sync::SyncChange>,
        signer_certs: &[fauna_core::encoding::EmbedAsBytes],
        cursor: i64,
    ) -> Result<HeadRejudge> {
        use fauna_core::label_custody::ChangePathOpen;
        use fauna_protocol::sync_row_verify::RowVerdict;
        let mut verdicts = self.judge_served_rows(&rows, signer_certs);
        if verdicts
            .iter()
            .any(|v| matches!(v, RowVerdict::Held(_)) && v.wants_roster_read())
        {
            self.refresh_reader_roster().await;
            verdicts = self.judge_served_rows(&rows, &[]);
        }
        if verdicts.iter().any(|v| matches!(v, RowVerdict::Held(_))) {
            return Ok(HeadRejudge {
                complete: false,
                folded: 0,
            });
        }
        let mut heads: HashMap<String, (fauna_protocol::sync::SyncChange, RowVerdict)> =
            HashMap::new();
        for (row, verdict) in rows.into_iter().zip(verdicts) {
            if row.seq > cursor || row.is_retention == Some(true) || !verdict.admits() {
                continue;
            }
            let key = row.path_hash.to_ascii_lowercase();
            if heads.get(&key).is_none_or(|(head, _)| head.seq < row.seq) {
                heads.insert(key, (row, verdict));
            }
        }
        let (heads, verdicts): (Vec<_>, Vec<_>) = heads.into_values().unzip();
        let mut heads = self.admit_judged_rows(heads, verdicts);
        for head in &mut heads {
            head.derived_through = crate::causal::bounded_watermark(head.seq, head.derived_through);
        }
        self.open_sealed_change_paths(&mut heads);

        // Every row the fold's delete arm could act on (it keys on the type;
        // the wire contract is "no manifest").
        let is_delete = |head: &fauna_protocol::sync::SyncChange| {
            head.manifest_hash.is_none() || head.change_type.eq_ignore_ascii_case("delete")
        };
        let mut complete = true;
        let mut to_fold = Vec::new();
        for head in heads {
            let Some(path) = head.path.clone().filter(|p| !p.is_empty()) else {
                match self.sealed_path_verdict(&head) {
                    ChangePathOpen::NoRoot => complete = false,
                    ChangePathOpen::Refused(_) => self
                        .causal()
                        .note_permanent_skip(Some(&head.path_hash), head.seq),
                    ChangePathOpen::Opened(_) => {}
                }
                continue;
            };
            let frontiers = self.causal().frontiers(&path);
            let has_through = frontiers
                .frontier
                .unwrap_or(0)
                .max(frontiers.edit_frontier.unwrap_or(0));
            if head.seq <= has_through {
                continue;
            }
            if is_delete(&head) && !self.causal().skip_noted(&head.path_hash, head.seq) {
                continue;
            }
            // Already at this head (a frontier file lost, say): nothing to fold.
            if let Some(manifest) = head
                .manifest_hash
                .as_deref()
                .and_then(|m| Self::parse_manifest_hash(m).ok())
                && self.db.get_entry(&path)?.is_some_and(|entry| {
                    entry.manifest_hash == Some(manifest)
                        && matches!(entry.state, SyncState::Synced | SyncState::Placeholder)
                })
            {
                continue;
            }
            to_fold.push(head);
        }
        if to_fold.is_empty() {
            return Ok(HeadRejudge {
                complete,
                folded: 0,
            });
        }
        to_fold.sort_by_key(|head| head.seq);
        // Through the ordinary fold — one head per path, so nothing in it is
        // a replay — but never its anchor write: a fold that defers would
        // otherwise rewind the cursor and replay the log after all.
        let (batch, _) = self.fold_remote_changes(&to_fold, cursor).await?;
        if !batch.deferred {
            // The delete arm has spoken for each noted delete (applied, or
            // declined by its own rules, exactly as on first service): it is
            // read now, and must not be folded again.
            for head in to_fold.iter().filter(|h| is_delete(h)) {
                self.causal().release_skip(&head.path_hash, head.seq);
            }
        }
        Ok(HeadRejudge {
            complete: complete && !batch.deferred,
            folded: batch.applied,
        })
    }

    /// The receiver-side rewrites and the pull log line, over the rows the
    /// verify step admitted.
    fn finish_fetched(
        &self,
        mut changes: Vec<fauna_protocol::sync::SyncChange>,
        folder: &str,
        since: i64,
        served: usize,
    ) -> Vec<fauna_protocol::sync::SyncChange> {
        // The receiver's half of the watermark's lower-bound law
        // (`conflicts.md` clause 5, the watermark-bound ruling): bounded ONCE
        // where the rows enter, so the reads outside the judge — the fold
        // licence, the held-content rung, the merge-ancestor lookup — never
        // see a writer's raw claim either.
        for change in &mut changes {
            change.derived_through =
                crate::causal::bounded_watermark(change.seq, change.derived_through);
        }
        self.open_sealed_change_paths(&mut changes);
        let unopened = changes
            .iter()
            .filter(|c| !c.path.as_deref().is_some_and(|p| !p.is_empty()))
            .count();
        if unopened > 0 {
            // Not a degrade worth staying silent about: every downstream
            // consumer skips a path-less change, and the anchor still advances
            // past it, so an unopenable seal means those bytes are never
            // re-offered to this holder on any later pull.
            tracing::warn!(
                served,
                unopened,
                folder = %fauna_core::log_redact::log_folder_name(folder),
                since,
                "fauna.sync.changes: sealed label(s) this holder's download custody cannot open — skipped, and the anchor advances past them"
            );
        } else {
            tracing::info!(
                served,
                folder = %fauna_core::log_redact::log_folder_name(folder),
                since,
                "fauna.sync.changes: pull"
            );
        }
        changes
    }

    /// Reconstitute each fetched change's in-memory `path` from its sealed
    /// label — the engine's **functional** sealed-first read half, the apply
    /// twin of the UI surfaces' `render_path` wiring (S3).
    ///
    /// Post-S9-flip the nest rests no plaintext path on a sealed plane, so
    /// `changes.list` serves `path: None` there and every downstream consumer
    /// (`apply_remote_changes`, `record_placeholders_from_changes`, the batch
    /// folds) would skip the change. This engine's download custody is exactly
    /// the label audience ("whoever opens the set's bytes opens its names"),
    /// so an openable seal fills `path` before any consumer folds; a seal this
    /// holder cannot open leaves `None`. A plaintext-resting plane's rows (a
    /// `public`-audience or `web`-mode folder, a reserved backup rail) arrive
    /// with `path` and are untouched.
    ///
    /// **`path` is filled only for a row that OPENS AND BINDS** — the shared
    /// opener requires `path_hash(opened) == path_hash`
    /// (`label_custody::open_change_path`). Without
    /// that conjunct a writer holding the set's label key could seal path P
    /// under Q's hash and every device would write P while the nest's
    /// per-path heads, conflicts and history recorded Q.
    ///
    /// What the leftover `None` then MEANS is [`Self::sealed_path_verdict`]'s
    /// job, not this function's: the transient and permanent classes do
    /// opposite things to the anchor (`path-sealing.md` § Apply-path degrade
    /// ruling), and collapsing them is what let one malformed row freeze a
    /// shared set's catch-up for every other member.
    pub(crate) fn open_sealed_change_paths(
        &self,
        changes: &mut [fauna_protocol::sync::SyncChange],
    ) {
        if changes
            .iter()
            .all(|c| c.path.as_deref().is_some_and(|p| !p.is_empty()))
        {
            return;
        }
        for change in changes.iter_mut() {
            if change.path.as_deref().is_some_and(|p| !p.is_empty()) {
                continue;
            }
            if let fauna_core::label_custody::ChangePathOpen::Opened(path) =
                self.sealed_path_verdict(change)
            {
                change.path = Some(path);
            }
        }
    }

    /// The causal stamp for a fresh local edit to `relative_path`: everything
    /// this engine has **honestly** incorporated.
    ///
    /// Every edit-stamp site goes through here rather than reading the anchor
    /// directly, because the anchor on its own stopped being the lower bound
    /// `conflicts.md` § the causal watermark requires the moment the apply
    /// path grew a permanent skip — it accounts for rows this device never
    /// read. [`crate::causal::CausalStore::honest_anchor`] owns the
    /// reduction (per path, self-releasing) and the reasoning.
    pub(crate) fn edit_stamp(&self, relative_path: &str) -> CausalStamp {
        CausalStamp::edit(self.honest_anchor(relative_path))
    }

    /// The no-novel-content stamp for `relative_path` — a proven reissue (the
    /// re-seal migration's re-upload, a re-upload of bytes the ledger holds
    /// at seq ≤ the path frontier, content equal to the live merge base).
    ///
    /// Through the SAME reduction as [`Self::edit_stamp`] (ruled 2026-09-20,
    /// `conflicts.md` § the causal watermark): the resolution bit says the
    /// bytes are old, which is true at any `derived_through`; the claim
    /// itself must still not cross a row this device never read, or a peer
    /// that applied that row adopts these old bytes over it verbatim.
    pub(crate) fn resolution_stamp(&self, relative_path: &str) -> CausalStamp {
        CausalStamp::resolution(self.honest_anchor(relative_path))
    }

    fn honest_anchor(&self, relative_path: &str) -> i64 {
        self.causal()
            .honest_anchor(relative_path, self.db.get_anchor().unwrap_or(0))
    }

    /// Why one row's sealed path did not become a `path` — the apply path's
    /// two-class verdict, for a row that arrived without a plaintext `path`.
    /// A row with no seal at all is the shared opener's `NoSeal` refusal —
    /// permanent like the other locally-decidable classes; no current writer
    /// lands the shape on a plane whose plaintext scrubs (the nest refuses it),
    /// and a plaintext plane serves its `path`. Until the compat-remnant sweep
    /// it was a silent skip-and-advance kept for pre-expand hash-only rows.
    ///
    /// Root selection is this engine's generation-aware, fail-closed custody;
    /// everything the verdict turns on lives in the shared opener, so no two
    /// appliers can drift on which failures freeze a set and which are
    /// recorded and skipped.
    ///
    /// The roots are bounded by who signed the row (ruling (8)(c),
    /// [`Self::record_signer_of_row`]), and an **unstamped** label of a row
    /// signed as a retired identity that opens under none of the roots that
    /// identity may open is [`ChangePathRefusal::SignerBound`](fauna_core::label_custody::ChangePathRefusal::SignerBound)
    /// — a noted skip, never the transient `NoRoot` hold, which would stall
    /// every later row of the set behind a row this host can never open. A
    /// stamped label keeps today's classes: a generation can lag its rows.
    fn sealed_path_verdict(
        &self,
        change: &fauna_protocol::sync::SyncChange,
    ) -> fauna_core::label_custody::ChangePathOpen {
        use fauna_core::label_custody::{ChangePathOpen, ChangePathRefusal};
        let keys = fauna_core::file_download::FileDownloadKeys {
            record_signer: self.record_signer_of_row(change),
            ..self.download_keys()
        };
        let unstamped = std::cell::Cell::new(false);
        let verdict = fauna_core::label_custody::open_change_path(
            |generation| {
                unstamped.set(generation.is_none());
                keys.label_open_roots(generation)
            },
            change.path_sealed.as_ref().map(|b| &b[..]),
            &change.path_hash,
        );
        match verdict {
            ChangePathOpen::NoRoot
                if unstamped.get()
                    && matches!(
                        keys.record_signer,
                        fauna_core::file_download::RecordSigner::Predecessor(_)
                    ) =>
            {
                ChangePathOpen::Refused(ChangePathRefusal::SignerBound)
            }
            other => other,
        }
    }

    /// Poll the node for remote changes and apply them locally.
    ///
    /// **Gated on the place's `accepts` flag** (phase 2 slice c — the
    /// `accepts_remote` field's doc owns the posture): a source-only seat
    /// skips the fetch entirely, leaving the anchor where it is — deliberate,
    /// so a later flag flip to accepting catches the seat up from exactly
    /// where delivery stopped, nothing skipped. The skip stamps no pull
    /// outcome: it neither drained the feed nor deferred anything, and a
    /// source seat's sync freshness rides its upload side.
    pub async fn pull_remote_changes(&self) -> Result<usize> {
        if !self.accepts_remote_changes() {
            // Its own target, so a journey can turn on exactly this line: it is
            // the seat's only account of a HELD pass, and the e2e witness of
            // "a device told not to take changes stops receiving them" counts
            // it as the moment a pass provably declined (`local-folder-sync`
            // outcome 15). Keep the target and the message's leading words.
            tracing::debug!(
                target: "fauna_sync_engine::place_accepts",
                folder = %self
                    .folder()
                    .map(fauna_core::log_redact::log_folder_name)
                    .unwrap_or_default(),
                "place does not accept remote changes; holding the anchor and skipping the pull"
            );
            return Ok(0);
        }
        let anchor = self.db.get_anchor()?;

        // An engine with no set has no feed to pull: the nest serves a change
        // log per folder, never an actor-wide one.
        let Some(ref folder) = self.folder else {
            return Ok(0);
        };
        // What the cursor already passed, judged again when the reader has
        // gained a signer or a root since (ruling (8)(f)) — heads at or below
        // the anchor only, which it never moves.
        self.rejudge_passed_heads_if_owed(folder).await;
        // A cut that arrived on this running engine: the take-over records
        // what it made history (ruling (11)(d)).
        self.take_over_if_owed().await;
        // Skip reports and resolves an earlier pass could not land
        // (`conflicts.md` § Skipped catch-up changes reach the review list:
        // best-effort per attempt, guaranteed over time).
        self.flush_skip_reports().await;
        let fetched = self
            .fetch_change_batch(folder, anchor, true)
            .await
            .context("fetching folder changes")?;
        let changes = fetched.changes;

        if changes.is_empty() {
            // Every served row refused (or none served): the cursor passes the
            // refused ones — they are absent, and re-listing them would only
            // refuse them again.
            if let Some(to) = fetched.advance_to.filter(|&to| to > anchor) {
                self.db.set_anchor(to)?;
            }
            // Verified caught-up with the nest while idle — the heartbeat that
            // keeps `last_sync` fresh on a device where nothing is changing
            // (stamps only if nothing is pending locally either), and the fold
            // evidence bound (3) reads. An empty feed cannot defer: there was
            // nothing to fail to resolve, and the roster really did arrive. A
            // HELD batch did not drain the feed, so it stamps nothing.
            self.note_pull_outcome(fetched.held);
            return Ok(0);
        }

        let batch = self.apply_remote_changes(&changes, anchor).await?;
        // A refused row past the last applied one: the apply advanced the
        // anchor to its max APPLIED seq, which would re-list that row for
        // ever. Unless the apply itself deferred (its caps hold the anchor
        // below rows it must retry), the cursor passes to what the verify step
        // allowed.
        if !batch.deferred
            && let Some(to) = fetched.advance_to
            && to > self.db.get_anchor()?
        {
            self.db.set_anchor(to)?;
        }
        let batch = AppliedBatch {
            deferred: batch.deferred || fetched.held,
            ..batch
        };
        // The feed is drained through this batch; if nothing is left pending
        // locally, this pull left the device consistent. A batch that DEFERRED
        // changes (a sealed path that did not open, an unresolved sync mode)
        // did NOT drain the feed — stamping it clean would report a sync
        // status the device has not reached, so an unfinished pass stamps
        // nothing. The flag is passed straight through rather than branched on
        // here, so both stamps read the same fact.
        self.note_pull_outcome(batch.deferred);
        Ok(batch.applied)
    }

    /// Apply an already-fetched `changes.list` batch to the local folder and db,
    /// advancing the anchor to the batch's max **applied** seq (a transient-class
    /// cap defers everything at and past it — see `deferred`).
    ///
    /// Split out of [`Self::pull_remote_changes`] so the apply rules are
    /// unit-testable against hand-built [`fauna_protocol::sync::SyncChange`]
    /// fixtures with no network — the same fetch/apply split
    /// [`Self::record_placeholders_from_changes`] already has.
    pub(crate) async fn apply_remote_changes(
        &self,
        changes: &[fauna_protocol::sync::SyncChange],
        anchor: i64,
    ) -> Result<AppliedBatch> {
        let (batch, max_seq) = self.fold_remote_changes(changes, anchor).await?;
        self.db.set_anchor(max_seq)?;
        tracing::info!(
            changes = batch.applied,
            anchor = max_seq,
            "pulled remote changes"
        );
        Ok(batch)
    }

    /// [`Self::apply_remote_changes`] without its anchor write: fold the rows
    /// onto the local folder and db, and return beside the batch the seq the
    /// anchor would advance to. The pull writes it; the head re-judge
    /// ([`Self::fold_passed_heads`]) folds rows the cursor already passed and
    /// must leave the cursor where it is.
    async fn fold_remote_changes(
        &self,
        changes: &[fauna_protocol::sync::SyncChange],
        anchor: i64,
    ) -> Result<(AppliedBatch, i64)> {
        let mut count = 0;
        let mut max_seq = anchor;

        // Seam B: paths whose batch-latest change is a live create/modify still
        // lacking a thumbnail — the download-backfill candidates. Computed over
        // the whole batch so a same-batch delete excludes its path (live-only
        // guard against resurrecting a deleted file).
        let backfill_targets = Self::thumbnail_backfill_targets(changes);

        // The highest seq seen per path in this batch. A delete is applied only
        // when it is its path's batch-latest change, so a delete→recreate inside
        // one batch never erases the recreated file (the same "latest state
        // decides" rule `record_placeholders_from_changes` folds by).
        let batch_latest_seq = Self::batch_latest_seq_by_path(changes);
        // The create/modify arm folds only where BOTH of the following agree,
        // because they close different doors onto the same data loss and
        // neither subsumes the other. Both were found the same day, from
        // opposite ends: `content_superseding_seq_by_path` from the two-seat
        // `native+native` red, the causal licence from the three-seat
        // `[engine³]` red.
        //
        // (1) WHICH row may supersede — the same fold minus this device's own
        // create/modify echoes, which apply no content and so supersede
        // nothing. See `content_superseding_seq_by_path`.
        let our_actor_for_fold = self.client.actor_id_hex();
        let content_superseding_seq = Self::content_superseding_seq_by_path(
            changes,
            self.client.device_id_hex(),
            &our_actor_for_fold,
        );
        // (2) WHETHER folding is licensed at all (`conflicts.md` clause 5): a
        // path's earlier batch rows may be folded only when the path's
        // CARRIER is a PEER row that provably supersedes every same-path row
        // below it — the adopted row then carries all the folded information.
        // A later seq ALONE is latest-writer-wins masquerading as
        // supersession, because a sibling is not a descendant. (1) cannot
        // see this: with two concurrent PEERS in one batch it names the
        // higher-seq peer as superseding, and folding the other one there is
        // exactly the N≥3 lost edit.
        //
        // The CARRIER is the path's latest SUPERSEDING row — the max fold-(1)
        // row (every delete + every peer create/modify), NOT the raw
        // batch-latest row (ruled 2026-08-04, row 156 second half — measured
        // live): a fresh bind's reconcile uploads the local file BEFORE the
        // first pull, so the batch tail is this device's own just-recorded
        // echo; taking the raw tail as the only permissible carrier refused
        // the fold outright and let a fully-covered stale create download
        // against the user's file. Any row above the superseding max is by
        // construction a self create/modify echo (anything else would itself
        // be the superseding max), writes nothing, and neither carries nor
        // blocks the fold — the fold covers only rows BELOW the carrier
        // (which is exactly the set fold (1) supersedes at the consult site).
        // Two rungs prove supersession per row below the carrier:
        //
        //   * the carrier's watermark covers the row's seq — the writer had
        //     incorporated it (the 2026-08-03 conjunct); or
        //   * the carrier is a DELETE and the row was authored by the
        //     CARRIER'S OWN DEVICE (same-author rung, ruled 2026-08-04 — row
        //     156): a device's later row on a path causally descends from its
        //     own earlier rows there by single-writer linearity, provable
        //     from the batch rows alone. No watermark can stand in: the stamp
        //     is a SET-WIDE lower bound (bumping it to own-authored seqs
        //     would over-claim unseen peer rows below them), and a device
        //     that records create → delete before its own echoes return
        //     honestly stamps an anchor below its own create — the
        //     fresh-bind-onto-linear-history shape, where refusing the fold
        //     downloads the stale create and manufactures a spurious
        //     conflict review row. DELETE carriers only: a delete is never
        //     judged by the receiver rules and carries the whole folded
        //     information ("the path ends deleted"), while folding under a
        //     CONTENT carrier starves the ledger — the folded rows'
        //     skip-holds are what let the content rung absorb a later
        //     stale-watermarked lost-ack reissue, and without them the
        //     reissue reaches the merge arm and latest-wins destroys a
        //     peer's edit (schedule-fuzzer counterexample, 2026-08-04 — the
        //     content-carrier variant of this rung is REFUTED, do not widen).
        //
        // And over both rungs, one veto: the carrier must not be a row this
        // receiver would itself SKIP — see `carrier_stale` below.
        //
        // Per-path map: path → fold-licensed.
        let our_device_for_fold = self.client.device_id_hex();
        let fold_licensed: std::collections::HashMap<&str, bool> = {
            let mut m: std::collections::HashMap<&str, Vec<&fauna_protocol::sync::SyncChange>> =
                std::collections::HashMap::new();
            for c in changes {
                if let Some(p) = c.path.as_deref() {
                    m.entry(p).or_default().push(c);
                }
            }
            m.into_iter()
                .map(|(p, rows)| {
                    // The carrier: the latest superseding row — the same
                    // predicate as `content_superseding_seq_by_path` (every
                    // delete, every peer create/modify), never a self
                    // content echo, and never a RETENTION row (loser-row
                    // ruling, 2026-08-05: a retention row is accounted and
                    // skipped, so folding rows under it would consume their
                    // content undelivered).
                    let carrier = rows
                        .iter()
                        .filter(|c| {
                            (c.change_type.eq_ignore_ascii_case("delete")
                                || !Self::row_is_own(c, our_device_for_fold, &our_actor_for_fold))
                                && c.is_retention != Some(true)
                        })
                        .max_by_key(|c| c.seq);
                    // Carrier-never-DEFERRED (leg-4 ruling, 2026-08-05,
                    // beside carrier-never-stale below): while this device's
                    // own pending rows are unlisted, a content carrier's own
                    // judgement may DEFER-CAP, which would consume the
                    // folded rows' content under a carrier that never
                    // applied. Process per-row instead. The test is the
                    // DEFER's exact reachability, not any divergence: the
                    // report window (the in-flight flag), or the ack→echo
                    // window — local differs from the live base while the
                    // RECORDED witness matches it (an unrecorded divergence
                    // merges instead, and a merging carrier still applies —
                    // the fold behaviour is untouched). A
                    // DELETE carrier is exempt (never judged, cannot defer).
                    //
                    // The firing rule itself is NOT restated here: it lives
                    // once, in `causal::verbatim_adopt_deferred` (this gate
                    // kept an older copy without the held-bytes release, so
                    // a reverted-to-held file declined a fold the cap would
                    // never have deferred — safe, but a second rule). What
                    // this site owns is only the ack→echo window's
                    // reachability: the cap fires on a FAST-FORWARD, which
                    // there rides the recorded witness.
                    let (in_ack_echo_window, local_held_at_or_below_frontier) =
                        match std::fs::read(self.watch_dir.join(p)) {
                            Ok(local) => {
                                let local_hash = ContentHash::of_raw(&local);
                                (
                                    self.read_base(p).as_deref() != Some(&local[..])
                                        && self.db.get_entry(p).ok().flatten().is_some_and(|e| {
                                            e.recorded_content_hash.as_ref() == Some(&local_hash)
                                        }),
                                    self.causal().frontier(p).is_some_and(|f| {
                                        self.causal().holds_content_at_or_below(p, f, &local_hash)
                                    }),
                                )
                            }
                            Err(_) => (false, false),
                        };
                    let pending = crate::causal::verbatim_adopt_deferred(
                        self.own_novelty_in_flight(p),
                        !in_ack_echo_window,
                        local_held_at_or_below_frontier,
                    );
                    let licensed = carrier.is_some_and(|carrier| {
                        let carrier_is_delete = carrier.change_type.eq_ignore_ascii_case("delete");
                        if !carrier_is_delete && pending {
                            return false;
                        }
                        let same_author_delete_carrier =
                            carrier_is_delete && carrier.device_id.is_some();
                        // The stale-skippable-carrier conjunct (`conflicts.md`
                        // clause 5, fuzzer-measured 2026-08-03; the engine leg
                        // landed 2026-08-04, row 160). A carrier the receiver
                        // rules would SKIP consumes the folded rows' content
                        // undelivered — the folded rows never download, the
                        // carrier returns before its own fetch, and the anchor
                        // advances past all of them: a lost edit, not a
                        // strand. It is reachable whenever this device holds
                        // unechoed own novelty, because the record-ack site
                        // advances the edit-frontier immediately while the
                        // frontier waits for the echo.
                        //
                        // Asked of `judge_incoming_before_fetch` itself rather
                        // than re-derived, so the licence and the skip can
                        // never disagree — the same structural guarantee that
                        // function's own doc comment exists to give. That is
                        // not pedantry here: rule 2 only skips when a frontier
                        // is TRACKED, so the bare `w < effective_edit_frontier`
                        // comparison would also refuse the fold on an
                        // untracked path, where nothing is skipped — which is
                        // precisely the spurious-conflict-row harm the
                        // ruling had just closed. Only `StaleResolution`
                        // blocks: a `Duplicate` carrier sits at or below the
                        // frontier, so every row it folds is already reflected.
                        //
                        // A DELETE carrier is outside the conjunct's territory
                        // — it is never judged by the receiver rules, so it
                        // cannot be stale-skipped.
                        let carrier_stale = !carrier_is_delete
                            && matches!(
                                crate::causal::judge_incoming_before_fetch(
                                    carrier.seq,
                                    carrier.derived_through,
                                    carrier.is_resolution == Some(true),
                                    carrier.is_retention == Some(true),
                                    self.causal().frontiers(p),
                                ),
                                crate::causal::PreFetchVerdict::StaleResolution
                            );
                        !carrier_stale
                            && !Self::row_is_own(carrier, our_device_for_fold, &our_actor_for_fold)
                            && rows.iter().filter(|r| r.seq < carrier.seq).all(|r| {
                                // Bounded like every receiver read (a caller
                                // may hand rows in past `fetch_changes`).
                                crate::causal::bounded_watermark(
                                    carrier.seq,
                                    carrier.derived_through,
                                )
                                .is_some_and(|w| w >= r.seq)
                                    || (same_author_delete_carrier
                                        && r.device_id == carrier.device_id)
                            })
                    });
                    (p, licensed)
                })
                .collect()
        };

        // Apply-path degrade ruling (2026-08-02, `path-sealing.md` § Apply-path
        // degrade ruling; SPLIT 2026-09-20): a
        // change whose sealed path did NOT open under this engine's custody
        // (`fetch_changes` filled `path` for every seal that opened AND bound)
        // splits in two, and the two do OPPOSITE things to the anchor.
        //
        // - **`NoRoot` — transient.** No root this holder offers opens the
        //   envelope; an M2 generation can lag its changes. Advancing past it
        //   would silently lose the file on this device forever, so cap the
        //   batch at the first such change: everything below applies now, the
        //   anchor stays below the cap, and the next cadence/nudge pull
        //   re-fetches from there.
        // - **`Refused` — permanent for that change.** A malformed envelope, a
        //   `path_hash` that is not 32 hex bytes, non-UTF-8 plaintext, or a
        //   path that does not hash to its own row: no key and no later pull
        //   changes any of those. Capping on them held the anchor below the
        //   first bad row FOREVER — and since the nest cannot check a seal,
        //   ONE member's malformed row froze every other member's catch-up on
        //   a shared set. It is recorded and skipped instead, and the anchor
        //   moves past it (`file-sync.md` § *A failed change must not strand
        //   the device*).
        //
        // A change with NO seal at all is the `NoSeal` refusal — permanent by
        // construction (no current writer lands one on a plane whose plaintext
        // scrubs, and a plaintext plane serves its `path`), recorded like the
        // rest rather than skipped in silence; every refused row then falls out
        // at the missing-path guard below, its seq already accounted for.
        let mut seal_cap: Option<i64> = None;
        #[allow(clippy::type_complexity)]
        let mut refused_seals: Vec<(
            i64,
            &str,
            Option<&[u8]>,
            fauna_core::label_custody::ChangePathRefusal,
        )> = Vec::new();
        for change in changes
            .iter()
            .filter(|c| !c.path.as_deref().is_some_and(|p| !p.is_empty()))
        {
            match self.sealed_path_verdict(change) {
                fauna_core::label_custody::ChangePathOpen::NoRoot => {
                    seal_cap = Some(seal_cap.map_or(change.seq, |c: i64| c.min(change.seq)));
                }
                fauna_core::label_custody::ChangePathOpen::Refused(reason) => {
                    refused_seals.push((
                        change.seq,
                        change.path_hash.as_str(),
                        change.path_sealed.as_ref().map(|b| &b[..]),
                        reason,
                    ));
                }
                // Cannot reach here: an opened row carries a `path` and was
                // filtered out above.
                fauna_core::label_custody::ChangePathOpen::Opened(_) => {}
            }
        }
        if let Some(cap) = seal_cap {
            tracing::warn!(
                cap,
                "a sealed path did not open under this engine's custody; deferring \
                 changes at and past that seq to a later pull (key material may \
                 still be syncing)"
            );
        }
        // Record each permanently-refused row ONCE: only those the anchor
        // actually accounts for this pass (below any cap) — a row above the
        // cap re-lists on the next pull and would otherwise collect a
        // duplicate conflict row every time.
        for (seq, path_hash, path_sealed, reason) in &refused_seals {
            if seal_cap.is_some_and(|cap| *seq >= cap) {
                continue;
            }
            tracing::warn!(
                seq,
                reason = reason.as_str(),
                "a sealed path can never open here; recording the change as \
                 permanently un-appliable and advancing past it"
            );
            // No path exists to file this under — that IS the failure — so the
            // row's `path_hash` stands in as its only stable handle, exactly
            // the identifier the nest filed it under. `details` carries the
            // reason; neither ever carries label content (the S7 log scrub).
            // The report forwards the row's own label pair verbatim
            // (`conflicts.md` § Skipped catch-up changes reach the review list).
            self.record_and_report_skip(
                path_hash,
                true,
                *path_sealed,
                &format!("sealed path unusable: {}", reason.as_str()),
            )
            .await;
            // The anchor is about to account for a row whose content never
            // entered this device, so later edit stamps must not claim it
            // (`conflicts.md` § the causal watermark, the lower-bound law).
            // Keyed by the row's own `path_hash`: the shared sink takes it
            // as a per-path key only when it is 32 hex bytes, and a malformed
            // one (always the `Salt` refusal — the opener judges the hash
            // first) takes the set-wide floor.
            self.causal().note_permanent_skip(Some(path_hash), *seq);
        }

        // The UNRESOLVED-mode hold (`file-sync.md` § 4, direction ratified
        // 2026-08-02) — the same transient-class shape as the seal cap above:
        // while no authoritative answer says what this seat does with a peer's
        // delete, the delete is declined AND deferred, never accounted for.
        // Advancing past it was what made a transient `members.list` failure
        // permanent — there was no later pass that could have saved the files.
        // The tick that re-pulls re-resolves the mode first
        // (`always_resident::run_watch_loop`), so the held tombstone re-delivers
        // to a seat that then either applies it (it was a sync seat) or
        // declines it permanently with the anchor advancing (backup).
        let unresolved_cap = if self
            .sync_mode_resolution()
            .holds_anchor_on_declined_delete()
        {
            changes
                .iter()
                .filter(|c| c.change_type == "delete")
                .map(|c| c.seq)
                .min()
        } else {
            None
        };
        if let Some(cap) = unresolved_cap {
            tracing::warn!(
                cap,
                "sync mode is unresolved; declining a peer's delete and deferring \
                 changes at and past that seq (the tombstone re-delivers once the \
                 role is readable)"
            );
        }
        let cap = match (seal_cap, unresolved_cap) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };

        // Separate peer changes from self-echoes. Process peer changes first
        // so that merges use the correct base (before self-echoes update it).
        let our_device = self.client.device_id_hex();
        let our_actor = self.client.actor_id_hex();
        let (peer_changes, self_echoes): (Vec<_>, Vec<_>) = changes
            .iter()
            .partition(|c| !Self::row_is_own(c, our_device, &our_actor));

        // Fold own NON-resolution rows' seqs into the stored edit-frontier
        // BEFORE any peer row is judged: echoes process last (the partition
        // above), so without this a covering resolution in the same batch
        // would be judged against an edit-frontier that has not counted these
        // rows' novelty and adopt over the seat's own edit (`conflicts.md`
        // clause 5; measured live by the 3-seat cell, 2026-08-03). The seqs
        // are in hand here, so this is the EXACT accounting — it also
        // retires the in-flight flag for the path (the unknown-seq window
        // has closed: every own row recorded so far is in this listing).
        let mut seen_own_manifests: std::collections::HashSet<(&str, &str)> =
            std::collections::HashSet::new();
        for change in &self_echoes {
            if matches!(change.change_type.as_str(), "create" | "modify")
                && let Some(path) = change.path.as_deref().filter(|p| !p.is_empty())
            {
                // An own RETENTION row (loser-row ruling, 2026-08-05; the
                // same-anchor ruling's conjunct 4, same day): it binds no
                // novelty anywhere and retires NOTHING from the in-flight
                // window — the window's carrier is the winner
                // row/publication, and the pre-ruling clear here released
                // the covering adopt that dropped the live leg-4a append.
                if change.is_retention == Some(true) {
                    continue;
                }
                // Any other own row — the winner (resolution- or, since the
                // same-anchor ruling, edit-class), a reseal, an ordinary
                // echo — retires exactly ITS carrier entry, by manifest.
                if let Some(m) = change.manifest_hash.as_deref() {
                    self.retire_own_novel_in_flight(path, m);
                }
                if change.is_resolution == Some(true) {
                    continue;
                }
                // Content licence (gap-3 ruling): an own edit-stamped echo
                // that is a REISSUE — the same manifest already seen earlier
                // in this batch (the original + its retry listed together),
                // or a manifest the causal store remembers at seq ≤ the path
                // frontier (the cross-batch case) — carries no novel
                // content, so it must not advance the stored edit-frontier;
                // counting it at the reissue's seq is the inflation that
                // permanently stranded covering resolutions on the
                // same-anchor shape. Unprovable → advance (the safe,
                // at-worst-stranding direction of the upper-bound law).
                let reissue = change.manifest_hash.as_deref().is_some_and(|m| {
                    let batch_dup = !seen_own_manifests.insert((path, m));
                    batch_dup
                        || self.causal().frontier(path).is_some_and(|f| {
                            self.causal().manifest_remembered_at_or_below(path, f, m)
                        })
                });
                if !reissue {
                    self.causal().advance_edit_frontier(path, change.seq);
                }
            }
        }

        // The DYNAMIC cap (leg-4 ruling, 2026-08-05): a covering-adopt defer
        // discovered mid-loop joins the transient-class cap family — rows at
        // and past the deferred seq wait for the next pull, the anchor holds
        // below it, and the re-listing (which contains this device's own
        // pending rows, by log contiguity) retries with exact frontiers.
        let mut dyn_cap: Option<i64> = None;
        for change in peer_changes.iter().chain(self_echoes.iter()) {
            if cap.is_some_and(|cap| change.seq >= cap)
                || dyn_cap.is_some_and(|cap| change.seq >= cap)
            {
                continue;
            }
            if change.seq > max_seq {
                max_seq = change.seq;
            }

            let is_self_echo = Self::row_is_own(change, our_device, &our_actor);

            match change.change_type.as_str() {
                "create" | "modify" => {
                    let path = match &change.path {
                        Some(p) => p.clone(),
                        None => {
                            tracing::warn!(seq = change.seq, "change missing path, skipping");
                            continue;
                        }
                    };
                    // Superseded within this batch by a later change for the
                    // same path — the delete arm below has folded this way since
                    // the batch-latest guard landed, and
                    // `record_placeholders_from_changes` folds identically:
                    // **the highest-seq change per path wins** (§ Files Appear
                    // Automatically). The create arm was the one place that did
                    // not, and the asymmetry was destructive rather than merely
                    // wasteful: replaying `create@N` when `delete@N+1` follows
                    // downloads a stale version **over** whatever the user has at
                    // that path *before* the delete arm removes it. A folder
                    // freshly bound to a set with history hits exactly this —
                    // observed live 2026-07-24, `file downloaded and written`
                    // immediately followed by `applied remote delete` for the
                    // same path, with the seat's own newer file underneath.
                    //
                    // Folded over `content_superseding_seq_by_path`, NOT the
                    // plain batch-latest: this device's own create/modify echo
                    // writes nothing, so it must never suppress a peer's
                    // concurrent edit (that was silent data loss).
                    if content_superseding_seq
                        .get(path.as_str())
                        .is_some_and(|&latest| latest > change.seq)
                        && fold_licensed.get(path.as_str()).copied().unwrap_or(false)
                    {
                        // A folded EDIT's novel bytes arrive through the
                        // licensed carrier, so the edit-frontier advances
                        // past it (upper-bound law, `conflicts.md` clause 5:
                        // under-counting would let a later resolution that
                        // misses the folded edit adopt over it) — unless the
                        // folded row is a remembered reissue (gap-3's
                        // content licence: its novelty was counted at the
                        // bytes' earliest carrier, and a fold delivers no
                        // bytes to check, so the manifest memory is the
                        // witness here) or a RETENTION row (loser-row
                        // ruling: invisible to content, folded or not).
                        if change.is_resolution != Some(true)
                            && change.is_retention != Some(true)
                            && !change.manifest_hash.as_deref().is_some_and(|m| {
                                self.causal().frontier(&path).is_some_and(|f| {
                                    self.causal().manifest_remembered_at_or_below(&path, f, m)
                                })
                            })
                        {
                            self.causal().advance_edit_frontier(&path, change.seq);
                        }
                        continue;
                    }

                    // A RETENTION row (loser-row ruling, 2026-08-05 —
                    // `conflicts.md` § Concurrent resolution): account its
                    // seq into the path frontier and nothing else, own echo
                    // and peer row alike — never fetched, applied, merged,
                    // or base-advanced, and never an edit-frontier advance.
                    // The shared rung (`judge_incoming_before_fetch`) gives
                    // the same verdict on the download path; intercepting
                    // here covers the self-echo route too.
                    if change.is_retention == Some(true) {
                        self.causal().account_retention_row(&path, change.seq);
                        continue;
                    }

                    if let Some(ref manifest_hex) = change.manifest_hash {
                        let manifest_hash = Self::parse_manifest_hash(manifest_hex)?;

                        if is_self_echo {
                            // Self-echo: update the merge base if the local file
                            // still matches (no merge happened in this batch).
                            if let Err(e) = self
                                .apply_self_echo(
                                    &path,
                                    manifest_hash,
                                    change.content_key_version,
                                    Some(change.seq),
                                    change.derived_through,
                                    change.is_resolution,
                                )
                                .await
                            {
                                tracing::warn!(path = %fauna_core::log_redact::log_path(&path), error = %e, "self-echo base update failed");
                            }
                        } else {
                            // Backfill a thumbnail only when this change is the
                            // path's batch-latest live-no-thumbnail state (so we
                            // re-record once, against the current head).
                            let backfill = backfill_targets
                                .get(&path)
                                .filter(|t| t.seq == change.seq)
                                .map(|t| t.size_bytes);
                            // The honest-claim gap check (leg-4 ruling): the
                            // report's winner may claim the incoming seq only
                            // when every same-path row between this path's
                            // frontier and the incoming seq is this device's
                            // own — their content is reflected in local by
                            // the defer-arm invariant. The listing covers the
                            // whole gap when the frontier is at or past the
                            // pull anchor (rows below the anchor are
                            // consumed); otherwise pass false — the claim
                            // then falls back to the frontier, which only
                            // under-states (safe).
                            let claim_gap_all_own = {
                                let f = self.causal().frontier(&path).unwrap_or(0);
                                f >= anchor
                                    && changes
                                        .iter()
                                        .filter(|c| c.path.as_deref() == Some(path.as_str()))
                                        .filter(|c| c.seq > f && c.seq < change.seq)
                                        .all(|c| Self::row_is_own(c, our_device, &our_actor))
                            };
                            match self
                                .download_and_write_file(
                                    &path,
                                    manifest_hash,
                                    change.content_key_version,
                                    change.device_id.clone(),
                                    change.created_at,
                                    backfill,
                                    Some(change.seq),
                                    change.derived_through,
                                    change.is_resolution,
                                    change.is_retention,
                                    claim_gap_all_own,
                                )
                                .await
                            {
                                Ok(DownloadOutcome::Applied) => {
                                    count += 1;
                                }
                                Ok(DownloadOutcome::DeferredCap) => {
                                    // Hold the anchor below this row; the
                                    // rows already processed stay processed
                                    // (all below this seq — peers run in seq
                                    // order and own echoes below it still
                                    // process past the cap check). Since the
                                    // cap-release ruling (2026-09-21,
                                    // `conflicts.md` clause 5) a capped seq
                                    // always sits ABOVE every own row this
                                    // device has counted — the judge grants
                                    // the fast-forward the cap holds only to
                                    // a row dominating the effective
                                    // edit-frontier — so the releasing own
                                    // echo is below the cap and processes in
                                    // this very pass; the next pull retries
                                    // against the advanced base. The
                                    // pre-ruling untracked-frontier arm
                                    // capped BELOW the own echo and parked
                                    // every fresh bind onto a same-named
                                    // peer file for ever.
                                    dyn_cap = Some(change.seq);
                                    if max_seq >= change.seq {
                                        max_seq = change.seq - 1;
                                    }
                                }
                                Err(e) => {
                                    // ⚠ This arm used to `continue` — and the
                                    // anchor at the end of the pass then
                                    // advanced PAST a row this device never
                                    // read. One
                                    // network blip mid-fetch dropped a peer's
                                    // change here for good, and worse: every
                                    // later local edit stamps
                                    // `CausalStamp::edit(get_anchor())`, so
                                    // the anchor's over-reach became a
                                    // `derived_through` claiming the unread
                                    // row. A peer whose file still matched its
                                    // base then judged that edit fast-forward
                                    // and overwrote its OWN unread work, with
                                    // no conflict row, on every seat.
                                    //
                                    // The two classes now do what
                                    // `file-sync.md` § *A failed change must
                                    // not strand the device* rules
                                    // (classification: `apply_failure`, the
                                    // default transient).
                                    match fauna_core::apply_failure::permanent_reason(&e) {
                                        None => {
                                            tracing::error!(
                                                path = %fauna_core::log_redact::log_path(&path),
                                                seq = change.seq,
                                                error = ?e,
                                                "failed to download remote file; holding the \
                                                 anchor below it for a later pull"
                                            );
                                            dyn_cap =
                                                Some(dyn_cap.map_or(change.seq, |c: i64| {
                                                    c.min(change.seq)
                                                }));
                                            if max_seq >= change.seq {
                                                max_seq = change.seq - 1;
                                            }
                                        }
                                        Some(reason) => {
                                            // Every later pull fails
                                            // identically, so holding the
                                            // anchor here would strand every
                                            // change after it. Recorded, not
                                            // silent, and the anchor advances.
                                            tracing::warn!(
                                                path = %fauna_core::log_redact::log_path(&path),
                                                seq = change.seq,
                                                reason,
                                                error = ?e,
                                                "a remote change can never apply on this device; \
                                                 recording it and advancing past it"
                                            );
                                            // Once per path (the head
                                            // re-judge folds a head again
                                            // that still cannot apply), and
                                            // reported when recorded. The
                                            // content-free reason class is
                                            // the row's details; the error
                                            // itself rides the log above.
                                            self.record_and_report_skip(
                                                path.as_str(),
                                                false,
                                                None,
                                                reason,
                                            )
                                            .await;
                                            // …and the anchor must not lend
                                            // this row to a later edit's
                                            // claim: the bytes never entered
                                            // this device.
                                            self.causal().note_permanent_skip(
                                                Some(&change.path_hash),
                                                change.seq,
                                            );
                                        }
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                }
                "delete" => {
                    if let Some(ref path) = change.path {
                        // A backup-mode host never applies a peer's delete — its
                        // copy IS the historical record (`file-sync.md` § 4;
                        // `principles.md` § No user-data loss). First check in
                        // the arm, before the batch/tombstone reasoning below,
                        // because none of that reasoning can change the answer:
                        // in this mode no remote delete is ever applied,
                        // whatever its provenance.
                        //
                        // `continue`, not a `count` bump: nothing was applied,
                        // and the anchor still advances past the change (the
                        // caller's `set_anchor` is outside this loop) so the
                        // device does not re-fetch the tombstone forever.
                        if !self.applies_remote_deletes() {
                            tracing::info!(
                                path = %fauna_core::log_redact::log_path(path),
                                "backup mode: keeping the local copy of a path the source deleted"
                            );
                            continue;
                        }
                        // Superseded within this batch by a later create/modify:
                        // the path was deleted and recreated. Applying the delete
                        // would erase the *recreated* file, and a self-echoed
                        // create only re-bases (it downloads nothing), so those
                        // bytes would not come back.
                        if batch_latest_seq
                            .get(path.as_str())
                            .is_some_and(|&latest| latest > change.seq)
                        {
                            continue;
                        }

                        // Deliberately NOT gated on `is_self_echo`. The tombstone
                        // echoes back to the device that recorded it, but *which*
                        // device recorded a delete says nothing about whether the
                        // local file is still on disk — only the disk does, and
                        // two different actors record under this same id:
                        //
                        //   - the engine (`handle_delete`) removes the file and
                        //     its row *before* recording, so its echo finds
                        //     nothing and the `exists()` check below is already
                        //     the no-op the old self-echo skip was written for; and
                        //   - a client UI (`media-delete-button` →
                        //     `MediaMachine::delete` → `fauna.sync.changes.record`)
                        //     records the same self-echo having never touched the
                        //     disk.
                        //
                        // Skipping both orphaned the second case permanently: the
                        // nest tombstones the path and the explorer stops listing
                        // it while the bytes stay on disk, and `set_anchor` moves
                        // past the tombstone whether or not it was applied, so the
                        // delete never came back.
                        // …but `exists()` answers "is something here?", never
                        // "is this the file the tombstone refers to?". The
                        // tombstone's subject is the **row**, not the path. Two
                        // ways a *different* file occupies a tombstoned path,
                        // both of them user data that exists nowhere else:
                        //
                        //   - **no row at all** — a brand-new local file the
                        //     engine has never synced (a fresh bind replaying a
                        //     set's whole history meets exactly this); and
                        //   - **a tombstoned row** (`delete_entry` soft-deletes,
                        //     so the row outlives the file) with a file back on
                        //     disk — i.e. a RECREATION after the delete already
                        //     landed. A replayed tombstone must not erase it.
                        //
                        // Deleting either is a no-user-data-loss violation
                        // (`docs/goal/principles.md`), and it compounds: with no
                        // live row, reconcile's delete-detection never queues an
                        // upload, so the bytes are simply gone. Found live
                        // 2026-07-24 — the multiseat linux seat's phase-1 files
                        // were destroyed this way and `new=0` followed.
                        //
                        // Declining here does NOT reopen § Files Appear
                        // Automatically's "a skipped delete never comes back":
                        // that hazard is a *tracked* file stranded on disk while
                        // the nest shows it deleted. These files are untracked or
                        // recreated, so there is no such divergence to strand —
                        // reconcile sees an unknown/new local file and uploads it
                        // as a fresh create at a higher seq, which is precisely
                        // how every device converges on "the file exists".
                        // Tracked-ness alone is NOT enough, because a live row
                        // does not imply the nest has the bytes: the upload path
                        // writes its row *before* the upload lands ("Mark as
                        // uploading" — `local_hash` = the new bytes,
                        // `manifest_hash` = the OLD base). So a file recreated at
                        // a tombstoned path carries a live `Uploading` row for the
                        // whole in-flight window.
                        //
                        // And the ROW alone is not enough either — the DISK must
                        // agree with it:
                        // `local_hash` refreshes only when `upload_file` reaches
                        // "Mark as uploading", and a local edit sits behind the
                        // watcher debouncer first, whose timer resets on every
                        // event — a continuously written file stays unobserved
                        // for the whole write. The nudge-path pull deliberately
                        // runs no converge before applying. In that window the
                        // row reads Synced with matching hashes while the disk
                        // holds bytes the nest has never seen; unlinking on the
                        // row's word destroys their only copy with no conflict
                        // marker (`principles.md` § No user-data loss — the
                        // overwrite path already protects this case; the delete
                        // path must too).
                        //
                        // So a tombstone may only be applied when BOTH hold:
                        // the row's state says its content is settled
                        // (`Synced`/`Placeholder` — not mid-upload, not an
                        // observed local edit), AND the disk still hashes to the
                        // row's `local_hash`. Two identities that must NOT be
                        // compared to each other in the process: `local_hash` is
                        // the content's hash, `manifest_hash` is the hash of the
                        // manifest that reassembles to it. They differ for every
                        // real file, so testing `local_hash == manifest_hash` as
                        // a stand-in for "synced" declined every delete on every
                        // device that obtained the file by download — and
                        // reconcile then re-uploaded it as a fresh create,
                        // resurrecting deleted files fleet-wide.
                        let full_path = self.watch_dir.join(path);
                        // How the disk answers for the row's bytes. The four
                        // KEEP words differ in what they owe the user (below):
                        // `Unsynced`/`Unprovable` name content the nest lacks;
                        // `NoHead`/`Unreadable` name a lineage the tombstone
                        // cannot refer to, or an infra failure.
                        enum DiskWord {
                            /// disk == `local_hash`: the bytes ARE the last
                            /// synced version.
                            Synced,
                            /// disk != `local_hash`: an unobserved (e.g.
                            /// mid-debounce) edit — content the nest lacks.
                            Unsynced,
                            /// A materialized file with no local identity —
                            /// nothing proves it holds no edit.
                            Unprovable,
                            /// A cloud placeholder: no local bytes at stake.
                            NoLocalBytes,
                            /// No recorded head: nothing of this lineage ever
                            /// reached the nest.
                            NoHead,
                            /// The re-hash failed; fail CLOSED.
                            Unreadable,
                        }
                        enum Tombstone {
                            Apply,
                            /// Keep the bytes AND record the delete-vs-edit
                            /// conflict (file-sync.md § Conflicts, ratified
                            /// 2026-07-29): the lineage lost a delete to
                            /// content the nest lacks, and silence here reads
                            /// downstream as a sync bug when the survivor
                            /// re-uploads and "resurrects" the file.
                            KeepConflict,
                            /// Keep the bytes with no conflict row: there is
                            /// no shared lineage for the tombstone to lose to
                            /// (untracked, recreated after an applied
                            /// tombstone, headless), or an infra failure.
                            KeepSilent,
                        }
                        let verdict = match self.db.get_entry(path) {
                            Ok(Some(entry)) => {
                                let disk = match (entry.local_hash, entry.manifest_hash) {
                                    // A head is recorded, so something of this
                                    // file reached the nest — but only the disk
                                    // can say the bytes about to be destroyed are
                                    // still the synced ones. `local_hash` is the
                                    // content identity of the last synced
                                    // version, so disk == local_hash means no
                                    // unsynced edit is being unlinked. Fail
                                    // CLOSED on an unreadable file, like the
                                    // row-read arm below (a MISSING file also
                                    // lands here — moot, the exists() gate below
                                    // no-ops either way).
                                    //
                                    // ⚠ `local_hash` and `manifest_hash` are
                                    // DIFFERENT identities — the content's hash
                                    // vs. the hash of the manifest that
                                    // *reassembles* to that content
                                    // (`ContentHash::of_raw(&manifest_bytes)`;
                                    // see `download_file_bytes`, and
                                    // `db::clear_recorded_content_hash`). They
                                    // are equal only for a row seeded by a test
                                    // helper, never for a real downloaded file,
                                    // so they must NEVER be compared to each
                                    // other: doing so declined every delete on
                                    // every device that got the file by download
                                    // and let reconcile resurrect it as a new
                                    // create.
                                    (Some(local), Some(_head)) => {
                                        match fauna_core::chunker_stream::content_hash_streaming(
                                            &full_path,
                                        ) {
                                            Ok(disk) if disk == local => DiskWord::Synced,
                                            Ok(_) => DiskWord::Unsynced,
                                            Err(e) => {
                                                tracing::warn!(
                                                    path = %fauna_core::log_redact::log_path(path),
                                                    error = %e,
                                                    "cannot re-hash the file for a remote delete; keeping it"
                                                );
                                                DiskWord::Unreadable
                                            }
                                        }
                                    }
                                    // No recorded head yet: nothing of this file
                                    // has ever reached the nest.
                                    (_, None) => DiskWord::NoHead,
                                    // A head but no local identity. A cloud
                                    // PLACEHOLDER holds no local bytes, so the
                                    // tombstone costs nothing and applies. A
                                    // materialized file with no local identity
                                    // (a partial hydration that never stamped
                                    // one — `mark_hydrated`'s early returns) may
                                    // hold an unsynced edit, and nothing here can
                                    // prove otherwise, so keep it.
                                    (None, Some(_)) => {
                                        if crate::placeholder::path_is_cloud_placeholder(&full_path)
                                        {
                                            DiskWord::NoLocalBytes
                                        } else {
                                            DiskWord::Unprovable
                                        }
                                    }
                                };
                                // Only a row whose content is SETTLED can be
                                // spoken for by a tombstone. `Uploading` holds
                                // bytes in flight (its `local_hash` is already
                                // the new content while `manifest_hash` is still
                                // the old base), `LocallyModified` /
                                // `Conflicted` hold an observed local edit, and
                                // `RemotelyModified` / `Downloading` are
                                // mid-apply; none of that content is on the nest,
                                // so no tombstone can refer to it. `Deleted`
                                // already applied. The state is the honest
                                // discriminator here — a hash comparison is not,
                                // because a mid-upload row's `local_hash` DOES
                                // match the disk.
                                let settled = matches!(
                                    entry.state,
                                    SyncState::Synced | SyncState::Placeholder
                                );
                                if settled
                                    && matches!(disk, DiskWord::Synced | DiskWord::NoLocalBytes)
                                {
                                    Tombstone::Apply
                                } else {
                                    // Which keeps are CONFLICTS: exactly the
                                    // rows holding content the nest lacks. The
                                    // three in-flight local states hold it by
                                    // definition; a settled row holds it when
                                    // the disk diverged from the recorded
                                    // identity (the mid-debounce edit) or when
                                    // nothing proves it doesn't (the
                                    // identity-less materialized row). NOT
                                    // conflicts: `Deleted` (the tombstone
                                    // already applied — this decline is replay
                                    // against a recreation), the mid-apply
                                    // remote states (the local bytes are the
                                    // recoverable synced version), a headless
                                    // lineage (the tombstone cannot refer to
                                    // it), and infra failures.
                                    let local_only_content = !matches!(disk, DiskWord::NoHead)
                                        && match entry.state {
                                            SyncState::Uploading
                                            | SyncState::LocallyModified
                                            | SyncState::Conflicted => true,
                                            SyncState::Synced | SyncState::Placeholder => matches!(
                                                disk,
                                                DiskWord::Unsynced | DiskWord::Unprovable
                                            ),
                                            _ => false,
                                        };
                                    if local_only_content {
                                        Tombstone::KeepConflict
                                    } else {
                                        Tombstone::KeepSilent
                                    }
                                }
                            }
                            Ok(None) => Tombstone::KeepSilent,
                            Err(e) => {
                                // Fail CLOSED: if we cannot prove the row is
                                // ours to delete, keep the bytes.
                                tracing::error!(
                                    path = %fauna_core::log_redact::log_path(path),
                                    error = %e,
                                    "cannot read sync row for a remote delete; keeping the local file"
                                );
                                Tombstone::KeepSilent
                            }
                        };
                        if full_path.exists() && matches!(verdict, Tombstone::KeepConflict) {
                            tracing::info!(
                                path = %fauna_core::log_redact::log_path(path),
                                "declined a remote delete for locally modified content; \
                                 reporting a resolved delete-vs-edit conflict (edit wins)"
                            );
                            self.report_declined_delete(
                                path,
                                &full_path,
                                change.device_id.as_deref(),
                            )
                            .await;
                        } else if full_path.exists() && matches!(verdict, Tombstone::KeepSilent) {
                            tracing::info!(
                                path = %fauna_core::log_redact::log_path(path),
                                "declined a remote delete for an untracked or recreated local \
                                 file; reconcile will upload it as a new create"
                            );
                        } else if full_path.exists() {
                            // Arm the REMOVAL channel, not the write one: this
                            // path may still owe a `Created` event from the
                            // download earlier in this same batch, and a shared
                            // one-shot token would be consumed by it — leaving
                            // our own unlink to come back as a "user delete"
                            // and be re-recorded on the nest.
                            self.note_recent_removal(path);
                            if let Err(e) = tokio::fs::remove_file(&full_path).await {
                                tracing::error!(path = %fauna_core::log_redact::log_path(path), error = %e, "failed to delete local file");
                            } else {
                                self.db.delete_entry(path)?;
                                tracing::info!(path = %fauna_core::log_redact::log_path(path), "applied remote delete");
                                count += 1;
                            }
                        }
                    }
                }
                other => {
                    tracing::warn!(change_type = other, "unknown change type");
                }
            }
        }

        // The share leg's provisional-overlay reconcile (B2.4 — `p2p-shared-set-build.md`
        // § *Build design — the row half* → *Reconcile is retire-on-arrival*).
        // AFTER the fold and consulting none of its licensing: for every
        // overlaid path the nest spoke for in this batch (below the caps),
        // the provisional annotation retires — reconcile only ever REMOVES
        // rows, which is why it cannot fork. Deliberately ungated: overlay
        // rows are written by `p2p-share` builds, but ANY build of this
        // engine must retire them when the
        // nest speaks, or a mixed deployment strands provisional annotations
        // and blocked dehydration forever.
        self.reconcile_share_overlay(&peer_changes, &self_echoes, cap, dyn_cap)?;
        self.persist_head_signers(changes);

        Ok((
            AppliedBatch {
                applied: count,
                deferred: cap.is_some() || dyn_cap.is_some(),
            },
            max_seq,
        ))
    }

    /// The retire-on-arrival sweep [`Self::apply_remote_changes`] runs after
    /// its fold (B2.4; doc at the call site). The withheld dehydration proof
    /// is stamped only on a CONFIRM — the path's batch-latest content row IS
    /// the materialized manifest and the entry still tracks those bytes; then
    /// and only then does the nest provably hold what disk holds. Everything
    /// else (a different winner, a delete, an unmaterialized overlay row) is
    /// a plain retire: the batch's own apply already handled disk by the
    /// existing rules, and the materialized copy is neither uploaded nor
    /// retained as a conflict loser — it was never this replica's authorship.
    fn reconcile_share_overlay(
        &self,
        peer_changes: &[&fauna_protocol::sync::SyncChange],
        self_echoes: &[&fauna_protocol::sync::SyncChange],
        cap: Option<i64>,
        dyn_cap: Option<i64>,
    ) -> Result<()> {
        if self.db.list_share_overlay()?.is_empty() {
            return Ok(());
        }
        // The path's batch-latest row below the caps — deferred rows have not
        // been applied and must not retire an annotation early. Retention
        // rows are invisible to content (loser-row ruling) and never carry
        // the head, so they neither confirm nor name the latest manifest.
        let mut latest: std::collections::HashMap<&str, (i64, Option<&str>)> =
            std::collections::HashMap::new();
        for change in peer_changes.iter().chain(self_echoes.iter()) {
            if cap.is_some_and(|c| change.seq >= c)
                || dyn_cap.is_some_and(|c| change.seq >= c)
                || change.is_retention == Some(true)
            {
                continue;
            }
            let Some(path) = change.path.as_deref().filter(|p| !p.is_empty()) else {
                continue;
            };
            let slot = latest
                .entry(path)
                .or_insert((change.seq, change.manifest_hash.as_deref()));
            if change.seq >= slot.0 {
                *slot = (change.seq, change.manifest_hash.as_deref());
            }
        }
        let heads = latest
            .into_iter()
            .map(|(path, (_, manifest))| (path, manifest))
            .collect();
        // The resident apply has already handled disk by the ordinary rules;
        // only a host that owns its tree acts on the outcome.
        self.retire_share_overlay(&heads)?;
        Ok(())
    }

    /// The retire-on-arrival core both folds run — the resident apply
    /// ([`Self::reconcile_share_overlay`]) and the on-demand replica's
    /// populate fold ([`Self::record_placeholders_from_changes`]): every
    /// overlaid path `heads` names (path → the nest's latest manifest, `None`
    /// for a delete) retires, and a CONFIRM stamps the withheld proof. The
    /// outcome names the landed bodies it confirmed or superseded.
    fn retire_share_overlay(
        &self,
        heads: &HashMap<&str, Option<&str>>,
    ) -> Result<OverlayReconcile> {
        let mut outcome = OverlayReconcile::default();
        for overlay in self.db.list_share_overlay()? {
            let Some(&latest_manifest) = heads.get(overlay.path.as_str()) else {
                continue; // the nest has not spoken for this path yet
            };
            let confirmed = overlay.materialized
                && overlay.manifest_hash.is_some()
                && latest_manifest == overlay.manifest_hash.as_deref()
                && self.db.get_entry(&overlay.path)?.is_some_and(|e| {
                    e.manifest_hash.map(|m| hex::encode(m.digest())) == overlay.manifest_hash
                        && e.local_hash.map(|h| hex::encode(h.digest())) == overlay.content_hash
                });
            if confirmed {
                // The proof this stamps was earned by a body FETCHED from
                // another holder, never by this replica's own record.
                self.db.stamp_recorded_content_from_local(
                    &overlay.path,
                    crate::db::ProofOrigin::Fetched,
                )?;
                tracing::info!(
                    path = %fauna_core::log_redact::log_path(&overlay.path),
                    "share overlay CONFIRMED by the nest's sequenced row — dehydration proof stamped"
                );
                outcome.confirmed.push(overlay.path.clone());
            } else {
                tracing::info!(
                    path = %fauna_core::log_redact::log_path(&overlay.path),
                    "share overlay retired — the nest's row for the path applied by the ordinary rules"
                );
                if let (true, Some(content_hash)) = (overlay.materialized, overlay.content_hash) {
                    outcome.superseded.push(SupersededBody {
                        path: overlay.path.clone(),
                        content_hash,
                    });
                }
            }
            self.db.remove_share_overlay(&overlay.path)?;
        }
        // `list_share_overlay` is path-ordered, so both lists are sorted.
        Ok(outcome)
    }

    /// Decode a hex `changes.list` manifest hash into a [`ContentHash`]. Shared
    /// by [`Self::pull_remote_changes`] and [`Self::record_placeholders_from_changes`].
    fn parse_manifest_hash(manifest_hex: &str) -> Result<ContentHash> {
        let bytes = hex::decode(manifest_hex).context("decoding manifest hash")?;
        let digest: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("manifest hash wrong length"))?;
        Ok(ContentHash::from_digest_raw(digest))
    }

    /// **The placeholder fold's frontier check** (`mls-group-key-material.md`
    /// § M2 → *Writer-signed change records*, ruling (5) residual (i)): a
    /// signed row is genuine, but a nest can still serve an OLD genuine row
    /// after withholding what superseded it. Against the path's edit-frontier
    /// `f` (the seq through which this device's own edits entered — tracked
    /// only once it recorded there):
    ///
    /// - a row below `f` is never the path's head — this device already holds
    ///   later content, so folding it can only move the placeholder backwards;
    /// - a peer row above `f` whose (bounded, signed) `derived_through` is
    ///   below `f` never saw that content — a replay, skipped.
    ///
    /// Untracked path, own row, or a row with no watermark (unknown
    /// causality): not stale — the fold's latest-by-seq rule stands.
    pub(crate) fn placeholder_row_is_stale(
        change: &fauna_protocol::sync::SyncChange,
        edit_frontier: Option<i64>,
        own: bool,
    ) -> bool {
        let Some(f) = edit_frontier else {
            return false;
        };
        if change.seq < f {
            return true;
        }
        !own && change.seq > f && change.derived_through.is_some_and(|w| w < f)
    }

    /// Fold a `changes.list` log into [`SyncState::Placeholder`] rows for the
    /// on-demand hydration host, **without downloading any bytes**.
    ///
    /// The read-only host (`fauna-sync-agent` on Windows) runs no
    /// watcher/reconcile, so for a folder that is on-demand *from the start* its
    /// SyncDb is empty — nothing to list, nothing to hydrate. This learns the
    /// folder's current entries from the nest and records each as a
    /// placeholder carrying its `manifest_hash` (the anchor
    /// [`Self::download_file_bytes`] resolves on open) plus its size and a
    /// best-available mtime, but no chunk bytes.
    ///
    /// Folding rule: the highest-`seq` change per path wins; `manifest_hash`
    /// present ⟺ the file exists, `None` ⟺ deleted (the wire contract — see
    /// [`Self::record_change`]), so the latest state decides, robust to the
    /// log's order and to `change_type` casing.
    ///
    /// **The fold owns `Placeholder` rows and only those.** An already-tracked
    /// `Synced` row is left untouched — its bytes are on disk (the always→
    /// on-demand *switch* path populates such a db), and reconciling those is the
    /// full reconcile path's job. An already-tracked `Placeholder` is bytes-free,
    /// so it is **re-pointed** when the nest's head has moved: this host runs no
    /// [`Self::pull_remote_changes`], so without that a remote modify — or a
    /// restore this device recorded but crashed before applying locally
    /// (`docs/goal/behavior/file-sync.md` § Restore) — would leave the row
    /// anchored to a superseded manifest and serve stale bytes on the next open.
    /// An unchanged head writes nothing, so the fold stays idempotent.
    ///
    /// A latest-state *delete* reconciles the placeholder set: a delete folded
    /// *within* this call never records a row, and a delete that lands in a
    /// *later* pull than the create hard-removes the stale `Placeholder` row a
    /// prior call left, so the browser stops listing the gone file. A
    /// non-`Placeholder` row (e.g. a `Synced`/on-disk *switch*-case entry) is
    /// left to the full reconcile path — this bytes-free fold never deletes it.
    ///
    /// Advances the db anchor to the max seq so a later incremental pull resumes
    /// correctly. Returns the number of placeholder rows written — newly recorded
    /// or re-pointed at a moved head (removals are not counted).
    /// Test seam: [`Self::record_placeholders_from_changes`] for another crate's test,
    /// so an on-demand host's live test can fold a nest change through the REAL fold
    /// without a live `changes.list`. No production caller.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn fold_changes_for_test(
        &self,
        changes: &[fauna_protocol::sync::SyncChange],
    ) -> Result<PlaceholderFold> {
        self.record_placeholders_from_changes(changes)
    }

    pub(crate) fn record_placeholders_from_changes(
        &self,
        changes: &[fauna_protocol::sync::SyncChange],
    ) -> Result<PlaceholderFold> {
        use std::collections::HashMap;

        struct Folded {
            seq: i64,
            manifest: Option<String>,
            size: i64,
            /// Unix **seconds** — already normalized from the change's millisecond
            /// `created_at` by [`created_at_ms_to_unix_secs`].
            mtime: i64,
            content_key_version: Option<u64>,
            /// The head's thumbnail pointer, cached onto the row so a later
            /// re-seal can MOVE it rather than drop it (`SyncEntry::thumbnail_hash`).
            thumbnail: Option<String>,
        }

        let mut latest: HashMap<String, Folded> = HashMap::new();
        let mut max_seq = 0i64;
        let our_device = self.client.device_id_hex().to_string();
        let our_actor = self.client.actor_id_hex();
        for change in changes {
            max_seq = max_seq.max(change.seq);
            // A change whose path this reader could not open carries no plaintext
            // path; without it there is nothing to place in the file browser, so skip them.
            let Some(path) = change.path.clone() else {
                continue;
            };
            if Self::placeholder_row_is_stale(
                change,
                self.causal().edit_frontier(&path),
                Self::row_is_own(change, &our_device, &our_actor),
            ) {
                continue;
            }
            if let Some(prev) = latest.get(&path)
                && prev.seq > change.seq
            {
                continue;
            }
            latest.insert(
                path,
                Folded {
                    seq: change.seq,
                    manifest: change.manifest_hash.clone(),
                    size: change.size_bytes,
                    mtime: created_at_ms_to_unix_secs(change.created_at),
                    content_key_version: change.content_key_version,
                    thumbnail: change.thumbnail_hash.clone(),
                },
            );
        }

        let mut recorded = 0usize;
        let mut created: Vec<crate::enumerate::PlaceholderRow> = Vec::new();
        let mut stale_hydrated: Vec<StaleHydratedRow> = Vec::new();
        for (path, folded) in &latest {
            // Latest state is a delete (`manifest_hash = None`).
            let Some(manifest_hex) = folded.manifest.as_deref() else {
                // A remote delete that lands in a *later* pull than the create:
                // drop the stale placeholder this fold left earlier so the file
                // browser stops listing the gone file. Only a `Placeholder` row
                // is ours to remove — a `Synced`/on-disk row (the always→
                // on-demand *switch* case) carries local bytes whose deletion is
                // the full reconcile path's job, never this bytes-free fold's.
                // (A delete *within* one pull simply never records a row.)
                //
                // A `LocallyDeleted` row is removed too: the nest already holds a
                // delete for it, so the one this device owes is moot (decision (f)
                // of `delete-propagation.md` § *An offline placeholder delete
                // propagates* — nothing is left dangling).
                if let Some(entry) = self.db.get_entry(path)?
                    && matches!(
                        entry.state,
                        SyncState::Placeholder | SyncState::LocallyDeleted
                    )
                {
                    self.db.remove_entry(path)?;
                }
                continue;
            };
            let manifest_hash = Self::parse_manifest_hash(manifest_hex)?;
            let existing = self.db.get_entry(path)?;
            // Not on this disk: a brand-new row, or an owed offline delete the nest's
            // newer head resurrects (below) — either way its directory may already be
            // listed, so it is materialized eagerly like any other create.
            let mut is_new = existing.is_none();
            if let Some(existing) = existing {
                // The head comparison is over the *content* identity only —
                // deliberately not `remote_mtime`: a device that recorded its own
                // restore stamps the row from its local clock while the fold reads
                // the nest's `created_at`, so including mtime here would rewrite the
                // row on every start over a one-second skew.
                let head_matches = existing.manifest_hash.as_ref() == Some(&manifest_hash)
                    && existing.size_bytes == folded.size
                    && existing.content_key_version == folded.content_key_version;

                // An owed offline delete (`LocallyDeleted`) under a head the nest has
                // MOVED since: a remote edit landed after this device's delete. The
                // edit wins — no data is lost — so the row is re-pointed below as a
                // fresh placeholder, and its seen mark cleared: it is NOT on this
                // disk, and a mark left behind would read its absence as the old
                // delete all over again. An unchanged head leaves the delete owed.
                if existing.state == SyncState::LocallyDeleted {
                    if head_matches {
                        continue;
                    }
                    self.db.clear_seen(path)?;
                    is_new = true;
                } else if existing.state != SyncState::Placeholder {
                    // The fold *writes* `Placeholder` rows and only those — the same
                    // rule its delete arm above follows. A hydrated row's bytes are on
                    // disk (the always→on-demand *switch* case), and rewriting its
                    // manifest here would orphan them.
                    // ...but a `Synced` row whose head has moved is a real, stale
                    // local copy: the next open would serve the superseded bytes
                    // straight from disk, and no other pass on this host would ever
                    // notice. Invalidating it means freeing those bytes, which only
                    // the platform's placeholder surface knows how to do (Windows
                    // cfapi `dehydrate_placeholder`) — and which MUST fail when the
                    // file carries unsynced local edits. So the fold *reports* the
                    // row and leaves the DB untouched; the cfapi-aware caller applies
                    // it (`docs/goal/behavior/file-sync.md` § Restore, the local-apply
                    // orderings). Only `Synced` qualifies: a `Conflicted` /
                    // `LocallyModified` / in-flight row is some other pass's business.
                    if existing.state == SyncState::Synced && !head_matches {
                        stale_hydrated.push(StaleHydratedRow {
                            relative_path: path.clone(),
                            manifest_hash,
                            size_bytes: folded.size,
                            content_key_version: folded.content_key_version,
                            remote_mtime: folded.mtime,
                            version_num: existing.version_num,
                        });
                    }
                    continue;
                }
                // A `Placeholder` carries no local bytes, so re-pointing it at the
                // nest's current head can clobber nothing — and is *required*:
                // `download_file_bytes` resolves the hydration manifest from this
                // very row, so a head that moved would otherwise serve the
                // superseded manifest forever. Two ways it moves: an ordinary
                // remote modify (this host runs no `pull_remote_changes`), or a
                // restore this device recorded but crashed before applying locally
                // (`docs/goal/behavior/file-sync.md` § Restore).
                //
                // An unchanged head rewrites nothing, so a re-pull stays idempotent —
                // *except* for a 0-byte row still missing the empty-content
                // `local_hash` (see the size==0 arm below): an empty file, or a
                // file that shrank to empty, can carry `local_hash = None`,
                // which reconcile would read as a local edit the moment the OS
                // materializes it present-and-empty. Once stamped, the row is as
                // idempotent as any other — re-writing (and counting) it every pass
                // would make `refresh_from_nest` report "something new" forever for
                // any set holding an empty file, so the live-refresh tick would
                // signal + re-enumerate on every interval.
                if head_matches
                    && (folded.size != 0 || existing.local_hash == Some(ContentHash::of_raw(b"")))
                {
                    continue;
                }
            }
            // A 0-byte file has no bytes to fetch, so on an on-demand root cfapi fires **no**
            // FETCH_DATA when it is opened (measured on a live cfapi root, 2026-07-15 — opening
            // a 0-byte placeholder clears its OFFLINE/RECALL bits, so the file is genuinely
            // present-and-empty on disk, but delivers no callback). Its row therefore can never
            // flip to Synced via the fetch path. It stays a `Placeholder` deliberately: reconcile's
            // delete-detection iterates only `Synced` rows, so a Placeholder that has not yet
            // materialized is never mistaken for a user delete (a `Synced`-while-absent row would
            // be — that was the data-loss trap the "cheap fix" fell into). The overlay reads it as
            // `Synced` via `SyncState::effective_for_size` — a 0-byte placeholder is present-and-
            // empty. We stamp the empty-content identity as its `local_hash` so that once the OS
            // materializes it, reconcile sees hash==local_hash and leaves it stable — no spurious
            // "modified" upload of an empty file. Kept in shared Rust so a macOS File Provider
            // (identical empty-file case) inherits it (priority #2).
            let (local_hash, local_mtime) = if folded.size == 0 {
                (Some(ContentHash::of_raw(b"")), folded.mtime)
            } else {
                (None, 0)
            };
            self.db.upsert_entry(
                path,
                local_hash, // empty-content identity for a 0-byte file; None otherwise
                None,       // remote_hash: not carried by changes.list
                Some(manifest_hash), // the hydration anchor
                SyncState::Placeholder,
                local_mtime, // a 0-byte file is present-and-empty; a placeholder has no local mtime
                folded.mtime, // remote_mtime ≈ the change's created_at
                folded.size,
                1,                          // version_num
                folded.content_key_version, // the generation to decrypt this version
            )?;
            recorded += 1;
            if is_new {
                created.push(crate::enumerate::PlaceholderRow {
                    rel: path.clone(),
                    size: u64::try_from(folded.size).unwrap_or(0),
                    mtime: folded.mtime,
                });
            }
        }

        // Cache each live head's thumbnail pointer onto its row.
        //
        // A separate pass over the same fold, deliberately: the loop above exits
        // through five arms (delete, stale-hydrated, non-placeholder, unchanged
        // head, upsert) and the pointer belongs on the row in **all** of them —
        // including the unchanged-head arm, which is what heals a row written
        // before this column existed. Threading it through each arm would put a
        // thumbnail concern in five places that have nothing else to do with
        // thumbnails, to save one indexed UPDATE per pulled path.
        //
        // Only live heads: the delete arm removed its row, and a `UPDATE …
        // WHERE path` against a gone row is a no-op anyway.
        for (path, folded) in &latest {
            if folded.manifest.is_some() {
                self.db
                    .set_thumbnail_hash(path, folded.thumbnail.as_deref())?;
            }
        }

        if max_seq > self.db.get_anchor()? {
            self.db.set_anchor(max_seq)?;
        }
        // The share plane's reconcile, on this fold too
        // (`p2p-shared-set-build.md` § *Phone peers — design*, decision 1): an
        // on-demand replica never runs `apply_remote_changes`, so without
        // this an overlay row landed here would never be confirmed or
        // superseded and its body would sit in the kept root for ever. After
        // the rows above, so a confirm reads the entry as this fold left it.
        let heads: HashMap<&str, Option<&str>> = latest
            .iter()
            .map(|(path, folded)| (path.as_str(), folded.manifest.as_deref()))
            .collect();
        let overlay = self.retire_share_overlay(&heads)?;
        self.persist_head_signers(changes);

        // `latest` is a HashMap, so sort for a deterministic report (stable logs,
        // stable test assertions, stable apply order).
        stale_hydrated.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        created.sort_by(|a, b| a.rel.cmp(&b.rel));
        Ok(PlaceholderFold {
            recorded,
            created,
            stale_hydrated,
            overlay,
        })
    }

    /// Re-point a hydrated row the fold reported as stale ([`StaleHydratedRow`]) at
    /// the nest's head and mark it [`SyncState::Placeholder`] — the durable half of
    /// invalidating a stale local copy.
    ///
    /// **Call this only after the file's on-disk bytes have actually been freed.**
    /// The row says "no local bytes, hydrate from this manifest", so declaring it
    /// while the bytes are still on disk would strand the superseded content: the
    /// OS sees a full file and never asks the host to fetch. This inverts the
    /// ordering the *restore* path uses (row first, then dehydrate), and
    /// deliberately — see `docs/goal/behavior/file-sync.md` § Restore.
    ///
    /// `pinned` survives (`upsert_entry`'s `ON CONFLICT` omits it) and so does the
    /// row's `version_num`, carried on the report.
    pub fn repoint_hydrated_to_placeholder(&self, row: &StaleHydratedRow) -> Result<()> {
        self.db.upsert_entry(
            &row.relative_path,
            None,                    // local_hash: the bytes are gone
            None,                    // remote_hash: not carried by changes.list
            Some(row.manifest_hash), // the new hydration anchor
            SyncState::Placeholder,
            0, // local_mtime
            row.remote_mtime,
            row.size_bytes,
            row.version_num,
            row.content_key_version,
        )
    }

    /// Ensure the WS-RPC control plane is open — a no-op when it already is (the
    /// sync agent connects it before the build, which reads over it). The bearer
    /// authorizes the actor (no keypair signing), so a bearer-only engine can
    /// connect; the byte plane (chunk/manifest downloads) is plain HTTP and needs
    /// no connection. `changes.list` (placeholder population) rides this plane.
    /// The handshake completes asynchronously — the first RPC waits for it — so
    /// callers need not wait after this returns.
    pub async fn connect_control_plane(&self) -> Result<()> {
        self.nest_client
            .ensure_connected()
            .await
            .map_err(|e| anyhow::anyhow!("connect control plane: {e}"))
    }

    /// Pull the folder's current entries from the nest (`changes.list`,
    /// bearer-only, no MLS) and record them as [`SyncState::Placeholder`] rows —
    /// path + size + manifest hash, no bytes — via
    /// [`Self::record_placeholders_from_changes`]. The on-demand hydration host
    /// calls this after [`Self::connect_control_plane`] to learn the folder, and
    /// again on each remote-change apply pass. Idempotent.
    ///
    /// Returns the [`PlaceholderFold`]: rows written, plus the hydrated rows the
    /// fold reported rather than rewrote (see [`StaleHydratedRow`] — the caller
    /// owns their cfapi invalidation).
    pub async fn populate_placeholders_from_nest(&self) -> Result<PlaceholderFold> {
        // The accepts gate covers this rail too (phase 2 slice c) — a
        // placeholder IS a remote change landing on the seat, just without its
        // bytes. In practice an on-demand host's seat always accepts (the
        // whole shape is "remote files appear"), so this is the uniform gate,
        // not a live restriction anyone configures today.
        if !self.accepts_remote_changes() {
            tracing::debug!(
                "place does not accept remote changes; skipping placeholder population"
            );
            return Ok(PlaceholderFold::default());
        }
        // A folder-less engine (the restore walk) has no change log to read.
        let Some(folder) = self.folder.as_deref() else {
            return Ok(PlaceholderFold::default());
        };
        let changes = self
            .fetch_changes(folder, 0)
            .await
            .context("listing folder changes for placeholder population")?;
        self.record_placeholders_from_changes(&changes)
    }

    /// Re-pull the set from the nest **in session** and converge the local rows —
    /// the File Provider live-refresh tick's primitive. The app-dead FP host runs
    /// no watcher/loop, so without this a remote change made after construction is
    /// invisible until `fileproviderd` reconstructs the extension; the appex calls
    /// this on the set's nest-authoritative cadence (`BuiltEngine::rescan_interval`)
    /// and then `signalEnumerator(for: .rootContainer)` iff it returns `true`
    /// (`file-sync.md` § Apple File Provider binding — *a pulled remote change
    /// signals the enumerator*).
    ///
    /// It **applies** the stale-hydrated rows [`Self::populate_placeholders_from_nest`]
    /// reports (a remote *modify* of a file this host had hydrated): unlike the
    /// Windows cfapi host — which dehydrates the superseded local bytes — the File
    /// Provider owns no on-disk cache to free, so it re-points the row at the moved
    /// head as an un-hydrated placeholder ([`Self::repoint_hydrated_to_placeholder`])
    /// and clears the dehydration proof, so the row's `contentVersion`
    /// ([`crate::provider_face::content_version`]) changes to the new manifest and
    /// the OS re-fetches the current content on the next `fetchContents`.
    ///
    /// Returns whether anything changed (a new/moved placeholder was written, or a
    /// stale hydrated row was re-pointed), so the appex signals the enumerator only
    /// when there is something new to enumerate — a no-op tick stays silent.
    pub async fn refresh_from_nest(&self) -> Result<bool> {
        Ok(self.refresh_from_nest_reconciling().await?.0)
    }

    /// [`Self::refresh_from_nest`], also handing back what the fold's share
    /// reconcile retired — a host that owns its tree settles its peer-landed
    /// bodies by it (`provider_face::owned_tree::OwnedTree::settle_peer_bodies`).
    /// A retired provisional row counts as a change.
    pub async fn refresh_from_nest_reconciling(&self) -> Result<(bool, OverlayReconcile)> {
        let fold = self.populate_placeholders_from_nest().await?;
        let changed = self.apply_refresh_fold(&fold)?;
        Ok((changed || !fold.overlay.is_empty(), fold.overlay))
    }

    /// The DB-only half of [`Self::refresh_from_nest`], split out so it is
    /// unit-testable against a hand-folded [`PlaceholderFold`] without a live nest
    /// connection (the `changes.list` fetch that produces the fold is not
    /// unit-testable — see `populate_placeholders_test.rs`'s module doc).
    pub fn apply_refresh_fold(&self, fold: &PlaceholderFold) -> Result<bool> {
        for row in &fold.stale_hydrated {
            self.repoint_hydrated_to_placeholder(row)?;
            // `repoint_hydrated_to_placeholder` re-points the manifest but leaves the
            // (now-stale) recorded proof — harmless on Windows (which never reads
            // `content_version`), but on the FP surface the proof would keep the old
            // `contentVersion` and suppress the re-fetch. Clear it so the version
            // moves to the new manifest.
            self.db.clear_recorded_content_hash(&row.relative_path)?;
        }
        Ok(fold.recorded > 0 || !fold.stale_hydrated.is_empty())
    }

    /// Download a file's plaintext bytes on demand by its manifest hash.
    ///
    /// For Cloud-Files-API (Windows) hydration: the FETCH_DATA callback maps a
    /// placeholder path to its manifest hash and hands the returned bytes to
    /// `CfExecute(TRANSFER_DATA)`. Unlike [`Self::download_and_write_file`], this
    /// touches neither the local filesystem, the SyncDb, nor the merge
    /// machinery — it downloads the manifest + chunks, decrypts/decompresses,
    /// reassembles in memory, verifies the file hash, and returns the bytes.
    /// `relative_path` is used for the path-safety guard and progress labelling.
    /// Download a manifest by its content hash, decode it, and — when it
    /// carries sealed plaintext hashes (`sealed_hashes`) — open them under the same
    /// root that seals the set's chunks, so every downstream consumer sees the
    /// plaintext `file_hash`/`chunk_hashes` view. Refuses a sealed-chunk
    /// manifest that names its plaintext hashes (`check_hash_shape`); fails
    /// closed for a sealed manifest whose root this holder lacks; surfaces the typed
    /// `check_min_reader` error for a manifest from a future format bump.
    ///
    /// Shared by every download path ([`Self::download_file_bytes_by_manifest`],
    /// [`Self::download_and_write_file`], [`Self::apply_self_echo`]).
    /// `content_key_version` is the M2 generation stamped on the change record
    /// (`None` on the owner `BackupKey` path), mirroring
    /// [`Self::fetch_decoded_chunks`]'s root selection.
    async fn fetch_manifest(
        &self,
        manifest_hash: &ContentHash,
        content_key_version: Option<u64>,
    ) -> Result<fauna_core::chunk::ChunkManifest> {
        fauna_core::file_download::fetch_manifest(
            &self.blob_fetcher(),
            &self.download_keys_for_record(manifest_hash),
            manifest_hash,
            content_key_version,
        )
        .await
    }

    /// Download a manifest's chunks, then decrypt and decompress them, yielding
    /// the plaintext chunk payloads in manifest order (ready for reassembly).
    ///
    /// Shared by every download path. The seal discriminator, the key precedence
    /// and the fail-closed posture are owned by
    /// [`fauna_core::file_download::fetch_decoded_chunks`] — read it there; this
    /// is the engine's binding of that policy to its pooled transfer worker.
    ///
    /// `manifest_hash` names the record whose chunks these are: the chunks are
    /// opened under the same per-record bound as the manifest
    /// ([`Self::download_keys_for_record`]) — a manifest a predecessor's
    /// signature opened must not list chunks the current root sealed.
    async fn fetch_decoded_chunks(
        &self,
        manifest_hash: &ContentHash,
        manifest: &fauna_core::chunk::ChunkManifest,
        relative_path: &str,
        content_key_version: Option<u64>,
    ) -> Result<Vec<Vec<u8>>> {
        fauna_core::file_download::fetch_decoded_chunks(
            &self.blob_fetcher(),
            &self.download_keys_for_record(manifest_hash),
            manifest,
            relative_path,
            content_key_version,
        )
        .await
    }

    /// This engine's key material as the shared walk's reader half.
    /// [`Self::download_keys`] for a read of one named manifest: its
    /// unstamped record opens under the owner root iff that record verified as
    /// this account's own ([`Self::owner_signed_manifests`]).
    fn download_keys_for_record(
        &self,
        manifest_hash: &ContentHash,
    ) -> fauna_core::file_download::FileDownloadKeys {
        fauna_core::file_download::FileDownloadKeys {
            owner_signed_record: self.record_is_owner_signed(manifest_hash),
            // Ruling (8)(c): who signed the record bounds every open of it.
            record_signer: self.manifest_signer(manifest_hash),
            ..self.download_keys()
        }
    }

    /// [`Self::download_keys`] for a peer-served row's bytes, bounded by the
    /// identity the peer door recovered as its signer (ruling (8)(c)) — the
    /// ingest rewrote the row's `author_actor_id` to it before anything read
    /// the row ([`Self::ingest_peer_share_rows_landing`]).
    #[cfg(feature = "p2p-share")]
    fn peer_row_keys(
        &self,
        change: &fauna_protocol::sync::SyncChange,
    ) -> fauna_core::file_download::FileDownloadKeys {
        fauna_core::file_download::FileDownloadKeys {
            record_signer: self.record_signer_of_row(change),
            ..self.download_keys()
        }
    }

    fn download_keys(&self) -> fauna_core::file_download::FileDownloadKeys {
        fauna_core::file_download::FileDownloadKeys {
            backup_key: self.backup_key.clone(),
            // Read candidates only. `content_open_roots` suppresses these
            // together with `backup_key` for a bound set (FS-5DC) in the one
            // place that already decides it, so this assembly stays a plain
            // carry-through.
            predecessor_backup_keys: self.predecessor_backup_keys.clone(),
            mls_group_id: self.mls_group_id.clone(),
            // A served-but-keyless set never reaches an engine at all
            // (`EngineKeyBinding::ServedKeysMissing` → no engine), and a served
            // engine holds its content keys, which already suppress the owner
            // path — so the read-side marker has nothing left to say here.
            served: false,
            content_keys: self.content_keys.clone(),
            retired_content_keys: self.retired_content_keys.clone(),
            // Per record, never per engine: only a read that names its manifest
            // can be offered the owner root for an unstamped record
            // ([`Self::download_keys_for_record`]).
            owner_signed_record: false,
            // Per record too: a read that names its row or manifest carries
            // who signed it ([`Self::download_keys_for_record`],
            // [`Self::sealed_path_verdict`]); a read that names none — a
            // snapshot restore, the sealed selective-sync lists — is no
            // retired identity's row.
            record_signer: fauna_core::file_download::RecordSigner::Current,
            epoch_secret: self.epoch_secret,
            // No `..Default::default()`: every field is now named, so a future
            // addition to `FileDownloadKeys` breaks HERE, loudly, instead of
            // silently defaulting at the engine's one reader assembly — which is
            // the shape that let `predecessor_backup_keys` itself ship inert.
        }
    }

    /// Bind the shared walk's blob-fetch seam to this engine's pooled transfer
    /// worker, so a shared-Rust download keeps the native concurrency + bandwidth
    /// limiter rather than fetching chunks one at a time.
    fn blob_fetcher(&self) -> EngineBlobFetcher<'_> {
        EngineBlobFetcher {
            client: &self.client,
            transfer_pool: &self.transfer_pool,
        }
    }

    /// Download one file's plaintext bytes by manifest hash.
    ///
    /// A thin binding of [`fauna_core::file_download::download_file_bytes_by_manifest`]
    /// — the shared client-side walk every app runs (web over wasm, the natives
    /// over this engine). The path guard, seal discriminator, key precedence,
    /// decompression and whole-file content-address verify all live there.
    pub async fn download_file_bytes_by_manifest(
        &self,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
        relative_path: &str,
    ) -> Result<Vec<u8>> {
        fauna_core::file_download::download_file_bytes_by_manifest(
            &self.blob_fetcher(),
            &self.download_keys_for_record(&manifest_hash),
            manifest_hash,
            content_key_version,
            relative_path,
        )
        .await
    }

    /// Bytes `[offset, offset + len)` of one file by manifest hash — the thin
    /// binding of [`fauna_core::file_download::download_file_range_by_manifest`]
    /// (the archive-import machine's zip-member reads over a folder-resident
    /// export, `archive-import.md` § Storage).
    pub async fn download_file_range(
        &self,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
        relative_path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>> {
        fauna_core::file_download::download_file_range_by_manifest(
            &self.blob_fetcher(),
            &self.download_keys_for_record(&manifest_hash),
            manifest_hash,
            content_key_version,
            relative_path,
            offset,
            len,
        )
        .await
    }

    /// Download a tracked file's bytes on demand by its path, resolving the
    /// manifest hash from the SyncDb. This is the entry point for the Windows
    /// cfapi FETCH_DATA callback (path in → bytes out).
    ///
    /// Errors if the path is untracked or has no recorded manifest. Note that a
    /// freshly-uploaded *local* file keeps its merge-base manifest (see
    /// [`Self::upload_file`]); the recorded manifest tracks the last *remote*
    /// version, which is exactly what a cloud-only placeholder carries — so this
    /// targets placeholder / remote entries, the cfapi case.
    pub async fn download_file_bytes(&self, relative_path: &str) -> Result<Vec<u8>> {
        let entry = self.db.get_entry(relative_path)?.ok_or_else(|| {
            anyhow::anyhow!(
                "not tracked: {}",
                fauna_core::log_redact::log_path(relative_path)
            )
        })?;
        let manifest_hash = entry.manifest_hash.ok_or_else(|| {
            anyhow::anyhow!(
                "no manifest recorded for {} (cannot hydrate)",
                fauna_core::log_redact::log_path(relative_path)
            )
        })?;
        self.download_file_bytes_by_manifest(
            manifest_hash,
            entry.content_key_version,
            relative_path,
        )
        .await
    }

    /// [`Self::download_file_bytes`]'s bounded-memory twin: the tracked file's
    /// plaintext lands at `dest` (windowed fetch → open → append,
    /// `fauna_core::file_download::download_file_to_path_by_manifest`) instead
    /// of in a single returned buffer. The File Provider `fetchContents` path —
    /// an iOS appex runs under a hard memory cap, and even on macOS a large
    /// file should not cross UniFFI as one `Vec<u8>`. Returns the verified
    /// whole-file content hash; on any failure `dest` is removed.
    pub async fn download_file_to_path(
        &self,
        relative_path: &str,
        dest: &Path,
    ) -> Result<ContentHash> {
        let entry = self.db.get_entry(relative_path)?.ok_or_else(|| {
            anyhow::anyhow!(
                "not tracked: {}",
                fauna_core::log_redact::log_path(relative_path)
            )
        })?;
        let manifest_hash = entry.manifest_hash.ok_or_else(|| {
            anyhow::anyhow!(
                "no manifest recorded for {} (cannot hydrate)",
                fauna_core::log_redact::log_path(relative_path)
            )
        })?;
        fauna_core::file_download::download_file_to_path_by_manifest(
            &self.blob_fetcher(),
            &self.download_keys_for_record(&manifest_hash),
            manifest_hash,
            entry.content_key_version,
            relative_path,
            dest,
        )
        .await
    }

    /// Restore a snapshot's files to `output_dir` — this engine's binding of
    /// [`restore_snapshot_walk`], the client-side walk shared by every restore
    /// host: this FFI/macOS surface (`MacRestoreView`) drives the SAME walk
    /// (`docs/goal/behavior/backup-restore.md`
    /// § 4 + § Restoring Files).
    ///
    /// Per file the walk fetches the manifest + chunks by store key, opens them
    /// under the owner `BackupKey` root (a plaintext manifest passes
    /// through, a sealed manifest with no key fails closed), verifies the
    /// whole-file content address, then writes the plaintext into
    /// `output_dir/relative_path` through the shared containment guard +
    /// `atomic_write_file`. This is why the surface works on sealed snapshots
    /// where the server-side ZIP route now refuses loudly (§ 4): the plaintext
    /// only ever materializes where the keys live.
    pub async fn restore_snapshot_files_to_dir(
        &self,
        files: &[SnapshotFileToRestore],
        output_dir: &Path,
    ) -> Result<RestoreSummary> {
        restore_snapshot_walk(
            &self.blob_fetcher(),
            &self.download_keys(),
            files,
            output_dir,
            |_idx, _total, _relative_path| {},
        )
        .await
    }

    /// Record that an on-demand placeholder has been hydrated: transition its SyncDb entry to
    /// [`SyncState::Synced`] **and record the file's true content identity**.
    ///
    /// The on-demand hydration host serves a file's bytes via
    /// [`download_file_bytes`](Self::download_file_bytes) — in-memory reassembly handed to
    /// cfapi — which, unlike the always-on `download_and_write_file` path, does not itself
    /// touch the entry's state. Calling this after a successful fetch keeps the synchronous
    /// `GetFileStatus` query and
    /// [`list_placeholder_rows`](crate::enumerate::PlaceholderLister::list_placeholder_rows)
    /// consistent with the file's now-on-disk reality: a hydrated on-demand file is a real
    /// file (`Synced`), not a `Placeholder`.
    ///
    /// ## Why it must record `local_hash`, and not merely the state
    ///
    /// **This is the whole hydration→upload feedback loop, and its cause is a dishonest row.**
    /// A state-only mark leaves a `Synced` row carrying `local_hash: None` — and *both* upload
    /// paths compare exactly that field to decide whether a file changed locally:
    /// [`upload_file`](Self::upload_file)'s "already synced, skipping" short-circuit
    /// (`entry.local_hash == Some(file_hash) && state == Synced`) and [`reconcile`](Self::reconcile)'s
    /// re-hash branch. With `None` there, both read a freshly-hydrated file as *locally
    /// modified* — so a two-way root would **re-upload every file it just downloaded**, bumping
    /// a version on the nest per hydration and echoing it out to every other device.
    ///
    /// Recording the served bytes' hash closes both at once. A watcher-level suppression
    /// (`was_recent_download`) closes neither: it is consume-on-check and would not survive the
    /// next `reconcile`.
    ///
    /// `content_hash` is the hash of the file's **full** content as served — never a re-hash
    /// from disk, which would be wrong twice over: cfapi may hydrate only the byte range the
    /// reader asked for, and reading a still-partial placeholder from the provider's own
    /// process stalls for 60 s (`crate::placeholder`).
    ///
    /// **A partial hydration records no identity.** If the OS still reports the file as a
    /// placeholder, the bytes on disk are *not* `content_hash`'s preimage, so claiming that
    /// identity would be a lie the upload path would act on. The row keeps `local_hash: None`
    /// and the placeholder guard keeps the file out of the hash/upload paths entirely until it
    /// is genuinely local.
    pub fn mark_hydrated(&self, relative_path: &str, content_hash: ContentHash) -> Result<()> {
        let full_path = self.watch_dir.join(relative_path);
        let meta = std::fs::metadata(&full_path).ok();

        // Not fully local (a partial hydration, or gone again) → state only, no identity.
        let fully_local = meta
            .as_ref()
            .is_some_and(|m| !crate::placeholder::is_cloud_placeholder(m));
        let (Some(meta), true) = (meta, fully_local) else {
            return self.db.update_state(relative_path, SyncState::Synced);
        };
        let Some(entry) = self.db.get_entry(relative_path)? else {
            return self.db.update_state(relative_path, SyncState::Synced);
        };

        let mtime = meta
            .modified()
            .ok()
            .map(fauna_core::data::Timestamp::secs_or_zero)
            .unwrap_or(0);

        // The manifest + its generation are the hydration anchor and the merge base — carried
        // through untouched. Only the *local* identity is new.
        self.db.upsert_entry(
            relative_path,
            Some(content_hash),
            entry.remote_hash,
            entry.manifest_hash,
            SyncState::Synced,
            mtime,
            entry.remote_mtime,
            meta.len() as i64,
            entry.version_num,
            entry.content_key_version,
        )?;
        // The bytes just materialized from `manifest_hash` via FETCH_DATA, so the
        // head provably reassembles to this local content — stamp the dehydration
        // gate's proof so freeing a hydrated-on-open file again is allowed.
        self.db
            .stamp_recorded_content_from_local(relative_path, crate::db::ProofOrigin::Fetched)
    }

    /// Record that the OS dehydrated a hydrated file — Explorer's "Free up space"
    /// or Storage Sense freed its local bytes while its content on the nest is
    /// unchanged. Flip the row `Synced` → `Placeholder` so the badge/overlay is
    /// **honest** (the bytes really are gone), keeping the manifest identity — the
    /// hydration anchor — untouched so the next open re-hydrates the same content.
    ///
    /// The symmetric inverse of [`mark_hydrated`](Self::mark_hydrated)
    /// (`Placeholder` → `Synced`), and a **pure state transition**: NOT a re-point
    /// ([`repoint_hydrated_to_placeholder`](Self::repoint_hydrated_to_placeholder)
    /// is for a *moved* nest head; here the head is unchanged). A no-op when the
    /// row is already gone. `local_hash` is left as-is (a `Placeholder` row is
    /// never consulted for upload — reconcile skips it on OS attributes — and a
    /// later re-hydration overwrites it), matching the provider-initiated
    /// `FreeSpace` verb (`fauna-sync-agent` `pipe_server::mark_placeholder`),
    /// which this is the OS-initiated twin of.
    ///
    /// Shared on the engine so a macOS File Provider host (identical OS-dehydrate
    /// case) inherits it (priority #2).
    ///
    /// Under the off-disk posture ([`Self::set_placeholders_off_disk`]) the row
    /// is left **unseen** instead: nothing of the file is on the disk there, and
    /// the root never writes the mark (`on-demand-files.md` § Linux FUSE binding,
    /// the dehydrate rule). That root frees bytes through
    /// [`Self::dehydrate_off_disk`]; this arm only keeps a stray caller honest.
    pub fn mark_placeholder(&self, relative_path: &str) -> Result<()> {
        self.db
            .update_state(relative_path, SyncState::Placeholder)?;
        if self.placeholders_off_disk() {
            return self.db.clear_seen(relative_path);
        }
        // Freed IN PLACE: the placeholder is on the disk by this engine's own
        // hand, so the row is seen however it reached `Synced` — a file uploaded
        // from this disk was never transferred as a placeholder, and must still
        // count once freed (`delete-propagation.md` § *An offline placeholder
        // delete propagates*, decision (a)).
        self.db.mark_seen(&[relative_path])
    }

    /// The **off-disk dehydrate's row half** — the first of its two steps
    /// (`on-demand-files.md` § Linux FUSE binding, the dehydrate rule): if
    /// freeing `relative_path`'s bytes is provably lossless
    /// ([`Self::is_dehydration_safe`]), flip its row to `Placeholder` with the
    /// seen mark cleared and arm the recent-removal suppression, and answer
    /// `true` — the caller then unlinks the file. `false` (nothing written) when
    /// the gate refuses: the file holds content the engine has not recorded, and
    /// freeing it would lose it.
    ///
    /// **Row first, unlink second** is the crash-safe order. A crash in between
    /// leaves a `Placeholder` row over a file still on the disk, which the next
    /// [`Self::reconcile`] repairs toward the bytes (back to `Synced` when the
    /// disk is the recorded head, an ordinary local edit otherwise); the reverse
    /// order would leave a `Synced` row with no file — a delete to the next
    /// sweep. The manifest anchor and the recorded identity are kept, so the
    /// next open fetches the same content and the repair can prove it.
    pub fn dehydrate_off_disk(&self, relative_path: &str) -> Result<bool> {
        if !self.is_dehydration_safe(relative_path) {
            return Ok(false);
        }
        self.db
            .update_state(relative_path, SyncState::Placeholder)?;
        self.db.clear_seen(relative_path)?;
        self.note_recent_removal(relative_path);
        Ok(true)
    }

    /// **A superseded own-record body is replaced, never freed** (`file-sync.md`
    /// § Relay serving): follow the moved head the fold reported for a body the
    /// holder-keeps gate will not free, the way a resident root does — fetch the
    /// new head from a holder, write it over the old body, and only then let the
    /// row move. The seat holds one whole version throughout.
    ///
    /// `Ok(true)` when the row now stands at `row`'s head, `Synced` over the
    /// fetched body (which may be freed afterwards like any fetched one).
    /// `Ok(false)`, nothing touched, when this is not the rule's case: the row is
    /// not a `Synced` own record the gate keeps, or the disk no longer holds
    /// exactly the recorded body (a local edit, the upload rail's). `Err` when
    /// the fetch failed — no holder answered — with the old body and the old row
    /// as they were; the next pull reports the row again and retries.
    ///
    /// The write goes through the resident apply door
    /// ([`Self::download_and_write_file`]), which reads the disk again after the
    /// fetch and settles anything but the recorded body as a divergence
    /// (`conflicts.md`), so an edit landing during the fetch is not overwritten.
    /// No listing context rides the fold's report, so the door judges as it does
    /// for any caller without one.
    pub async fn replace_superseded_own_record(&self, row: &StaleHydratedRow) -> Result<bool> {
        let rel = row.relative_path.as_str();
        let Some(entry) = self.db.get_entry(rel)? else {
            return Ok(false);
        };
        if entry.state != SyncState::Synced
            || !Self::holder_keeps(&self.db, &entry)
            || entry.manifest_hash == Some(row.manifest_hash)
        {
            return Ok(false);
        }
        let full_path = self.watch_dir.join(rel);
        match fauna_core::chunker_stream::content_hash_streaming(&full_path) {
            Ok(disk) if entry.recorded_content_hash == Some(disk) => {}
            _ => return Ok(false),
        }
        self.download_and_write_file(
            rel,
            row.manifest_hash,
            row.content_key_version,
            None,
            row.remote_mtime.saturating_mul(1000),
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .await?;
        Ok(self
            .db
            .get_entry(rel)?
            .is_some_and(|e| e.manifest_hash == Some(row.manifest_hash)))
    }

    /// **A holder keeps what it wrote** (`file-sync.md` § Relay serving): is
    /// `entry`'s body one this device recorded itself in a folder whose nest
    /// took no bytes? This device's own record is a proof only where the nest
    /// took the bytes. The residency is the persisted reading as it stands NOW
    /// — a proof earned while the folder was full stops counting once it is
    /// metadata-only — and a reading no engine ever wrote, or one that cannot
    /// be read, keeps the body.
    fn holder_keeps(db: &SyncDb, entry: &crate::db::SyncEntry) -> bool {
        entry.recorded_proof_origin == crate::db::ProofOrigin::OwnRecord
            && !matches!(db.residency_reading(), Ok(Some(false)))
    }

    /// Is freeing this file's local bytes **provably lossless by this engine's own
    /// record** — the row says `Synced` and the on-disk content hashes to exactly
    /// the content the *recorded head* reassembles to (`recorded_content_hash`)?
    ///
    /// **Why not just `local_hash`.** A re-hydration fetches `manifest_hash` (the
    /// recorded head), so the only sound proof that freeing the bytes is lossless
    /// is that *the head* — not merely the last local identity — yields these exact
    /// disk bytes. `local_hash` is advanced **optimistically before** a record (see
    /// `upload_file`'s pre-record `upsert_entry`), so a record that FAILED leaves a
    /// `Synced` row whose `local_hash == disk` while `manifest_hash` still points at
    /// the OLD merge base: dehydrating it would re-anchor the placeholder on the OLD
    /// manifest and the next open would download the OLD bytes, destroying the
    /// un-recorded edit. Gating on `recorded_content_hash` (set only where the head
    /// is *proven* to match the content — record success, hydrate-on-open, download
    /// apply) closes that window; it also closes the edit-between-record-and-flip
    /// window, since the comparison is against the current *disk*, not a stale
    /// `local_hash`.
    ///
    /// This is the gate for *overriding* a platform's own dirty-file dehydrate
    /// refusal (on cfapi, `CF_INSYNC_POLICY_TRACK_ALL` flips a file not-in-sync on
    /// any local write and only the provider can flip it back): a refusal may be
    /// *stale* — the edit that tripped it has since been uploaded AND recorded — but
    /// the override must never out-guess an edit the engine hasn't seen yet (e.g.
    /// one still inside the watcher's debounce window) or an upload whose record
    /// never landed.
    ///
    /// `false` for: no row, any non-`Synced` state, a cloud-only placeholder
    /// (nothing local to free — and hashing one would stall the driving thread for
    /// the platform's full recall timeout), an unreadable file, a `local_hash`
    /// mismatch, or a `recorded_content_hash` that is absent (an
    /// unproven head) or does not match the disk. Fail-closed: every error path
    /// answers "not safe".
    ///
    /// **A holder keeps what it wrote** (`file-sync.md` § Relay serving). In a
    /// metadata-only folder the nest took no bytes, so a record proves nothing
    /// about the head being fetchable: `false` too for a row whose proof is
    /// this device's own record ([`crate::db::ProofOrigin::OwnRecord`], and
    /// every row that does not say) unless the folder's persisted residency
    /// reading is *full*. A body fetched from another holder is freed as
    /// before. Every caller inherits this — the off-disk dehydrate, the
    /// platform's in-sync assertion (so the OS never evicts such a body), the
    /// owned tree's demotion to its cache root.
    ///
    /// Shared on the engine (not the Windows host) so a macOS File Provider
    /// host's evict path inherits the identical gate (priority #2).
    pub fn is_dehydration_safe(&self, relative_path: &str) -> bool {
        Self::is_dehydration_safe_in(&self.db, relative_path, &self.watch_dir.join(relative_path))
    }

    /// [`Self::is_dehydration_safe`] over a bare `db` and the file's `full_path` — for
    /// a caller that holds the folder's state DB but no engine (the Windows service's
    /// shell *Free up space* verb), so every dehydrate path answers the one gate.
    pub fn is_dehydration_safe_in(db: &SyncDb, relative_path: &str, full_path: &Path) -> bool {
        let Ok(Some(entry)) = db.get_entry(relative_path) else {
            return false;
        };
        if entry.state != SyncState::Synced {
            return false;
        }
        if crate::placeholder::path_is_cloud_placeholder(full_path) {
            return false;
        }
        if Self::holder_keeps(db, &entry) {
            return false;
        }
        match fauna_core::chunker_stream::content_hash_streaming(full_path) {
            Ok(disk_hash) => {
                // The disk is unchanged since we recorded the local identity AND
                // the recorded head reassembles to exactly these bytes — a
                // record-failed / mid-edit row whose head still points at OLD
                // content fails the second check (fail-closed on a NULL head too).
                entry.local_hash == Some(disk_hash)
                    && entry.recorded_content_hash == Some(disk_hash)
            }
            Err(_) => false,
        }
    }

    /// **A resident root's pending downloads** — fetch every `Placeholder` row
    /// onto the disk (`on-demand-files.md` § Linux FUSE binding, the flips rule:
    /// *a `Placeholder` row on a resident root is a pending download, never a
    /// delete*). An on-demand→always flip leaves the folder's cloud-only files as
    /// rows with no bytes here, and the on-demand fold
    /// ([`Self::record_placeholders_from_changes`]) already moved the change
    /// anchor past them, so [`Self::pull_remote_changes`] never re-delivers them;
    /// this pass is what makes the flip whole. The resident watch loop runs it at
    /// start and on every rescan tick, so a fetch that fails is retried there.
    ///
    /// Per row, by what the disk holds at its path:
    /// - **nothing** → download through the pull's own write path (atomic,
    ///   echo-suppressed, `Synced` with the recorded proof);
    /// - **the row's own recorded bytes** (a dehydrate cut short between its
    ///   row flip and its unlink) → `Synced` again, no download;
    /// - **anything else** → left alone: those bytes are the user's, and
    ///   [`Self::reconcile`] reads them as a local edit. Never overwritten here.
    ///
    /// Under the off-disk posture ([`Self::set_placeholders_off_disk`]) the
    /// root's placeholders are rows by design, so the pass fetches nothing.
    /// Returns how many rows it downloaded.
    pub async fn materialize_placeholder_rows(&self) -> Result<usize> {
        if self.placeholders_off_disk() {
            return Ok(0);
        }
        let mut fetched = 0;
        for entry in self.db.list_by_state(SyncState::Placeholder)? {
            let full_path = self.watch_dir.join(&entry.path);
            match std::fs::symlink_metadata(&full_path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => continue,
                Ok(meta) => {
                    if meta.is_file()
                        && let Ok(disk) =
                            fauna_core::chunker_stream::content_hash_streaming(&full_path)
                        && entry.local_hash == Some(disk)
                        && entry.recorded_content_hash == Some(disk)
                    {
                        self.db.update_state(&entry.path, SyncState::Synced)?;
                    }
                    continue;
                }
            }
            let Some(manifest) = entry.manifest_hash else {
                continue;
            };
            match self
                .download_and_write_file(
                    &entry.path,
                    manifest,
                    entry.content_key_version,
                    None,
                    0,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .await
            {
                Ok(DownloadOutcome::Applied) => fetched += 1,
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    path = %fauna_core::log_redact::log_path(&entry.path),
                    error = %e,
                    "pending placeholder download failed; retrying on the next pass"
                ),
            }
        }
        Ok(fetched)
    }

    /// Download and write a file by its manifest hash.
    ///
    /// `content_key_version` is the M2 generation the change was sealed under
    /// (from the `sync_changes` row), used to select the open key and recorded on
    /// the resulting local entry so a later re-hydration picks the same key.
    /// `incoming_device_id` is the hex device id that produced this change
    /// (from the `sync_changes` row), used to attribute the incoming candidate
    /// if the download turns out to conflict with a divergent local copy.
    ///
    /// `backfill = Some(size_bytes)` requests a thumbnail **download-backfill**
    /// (Seam B) for a synced file recorded without one: the change carried no
    /// `thumbnail_hash`, and now that we hold the plaintext we generate + seal +
    /// upload the missing thumbnail and re-record the file with it, carrying the
    /// passed `size_bytes` so the nest's quota delta stays 0. The caller passes
    /// `Some` only for a change that is the batch-latest live state of its path
    /// (see [`Self::thumbnail_backfill_targets`]) — the live-only guard against
    /// resurrecting a same-batch delete. `None` skips the backfill.
    ///
    /// `seq`/`derived_through`/`is_resolution`/`is_retention` are the
    /// incoming row's identity and causal stamp where the caller has them
    /// (`conflicts.md` clause 5); `None`s degrade to the pre-ruling
    /// behaviour.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn download_and_write_file(
        &self,
        relative_path: &str,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
        incoming_device_id: Option<String>,
        incoming_created_at_ms: i64,
        backfill: Option<i64>,
        seq: Option<i64>,
        derived_through: Option<i64>,
        is_resolution: Option<bool>,
        is_retention: Option<bool>,
        claim_gap_all_own: bool,
    ) -> Result<DownloadOutcome> {
        // The watermark bound at the apply DOOR too (the listing boundary
        // covers `fetch_changes`' rows; a caller handing rows in directly
        // does not pass it): the merge-ancestor lookup below reads this
        // value outside the judge, and an unbounded forged claim would name
        // this device's own newest held version the "common ancestor".
        let derived_through = match seq {
            Some(s) => crate::causal::bounded_watermark(s, derived_through),
            None => derived_through,
        };
        if !crate::path_guard::is_safe_relative_path(relative_path) {
            // PERMANENT for this change: a property of the row's own path, so
            // no later pull changes it (`fauna_core::apply_failure`).
            return Err(fauna_core::apply_failure::permanent(
                fauna_core::apply_failure::PermanentApplyFailure::PATH_REFUSED,
                format!(
                    "unsafe path rejected: {}",
                    fauna_core::log_redact::log_path(relative_path)
                ),
            ));
        }
        // Causal receiver rules (`conflicts.md` clause 5), byte-free verdicts
        // first — BEFORE the fetch, so a duplicate, a stale resolution, or a
        // retention row never costs a download. The fast-forward vs sibling
        // decision needs the local file and runs at the divergence check
        // below.
        if let Some(seq_v) = seq {
            match crate::causal::judge_incoming_before_fetch(
                seq_v,
                derived_through,
                is_resolution == Some(true),
                is_retention == Some(true),
                self.causal().frontiers(relative_path),
            ) {
                crate::causal::PreFetchVerdict::Duplicate => {
                    tracing::debug!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        seq = seq_v,
                        "causal: duplicate delivery (seq <= frontier) — skipped"
                    );
                    return Ok(DownloadOutcome::Applied);
                }
                crate::causal::PreFetchVerdict::StaleResolution => {
                    tracing::info!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        seq = seq_v,
                        "causal: stale resolution (derives from rows already \
                         incorporated) — skipped, accounted for"
                    );
                    self.causal().advance_frontier(relative_path, seq_v);
                    return Ok(DownloadOutcome::Applied);
                }
                crate::causal::PreFetchVerdict::RetentionRow => {
                    // Belt for the caller's interception (loser-row ruling):
                    // account and skip, byte-free.
                    tracing::info!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        seq = seq_v,
                        "causal: retention row — accounted, skipped"
                    );
                    self.causal().account_retention_row(relative_path, seq_v);
                    return Ok(DownloadOutcome::Applied);
                }
                crate::causal::PreFetchVerdict::NeedsLocalBytes => {}
            }
        }
        let manifest = self
            .fetch_manifest(&manifest_hash, content_key_version)
            .await?;

        // The content-keyed idempotence rung (gap-1 ruling): a stale-
        // watermarked row whose exact CONTENT this device already holds is a
        // reissue (lost-ack retry whose record landed, a re-seal) — skip it before paying for chunks or a merge that would
        // read as overlapping-identical hunks and latest-wins over a peer's
        // edit. Content hashes, not manifest hashes: a re-seal mints a new
        // manifest for the same bytes. The verdict comes from the full
        // licence (never this block alone) because the rung must not preempt
        // an adopt — a covering resolution's bytes can match an earlier
        // identical publication and must still fast-forward; the licence
        // needs `local == base` for that call, so the local file is read
        // here, ahead of the chunk fetch the skip exists to save.
        if let Some(seq_v) = seq {
            let frontiers = self.causal().frontiers(relative_path);
            let content_already_held = frontiers.frontier.is_some_and(|f| {
                derived_through.is_some_and(|w| w < f)
                    && self.causal().holds_content_at_or_below(
                        relative_path,
                        f,
                        &manifest.file_hash,
                    )
            });
            // A missing local file falls through to the ordinary flow (the
            // apply path owns absent-file semantics — never skip a write
            // that would restore one).
            if content_already_held
                && let Ok(local_data) = tokio::fs::read(self.watch_dir.join(relative_path)).await
            {
                // Base-equality OR recorded-bytes equality (gap-3's
                // sharpened unpublished-work conjunct) — an early Reissue
                // skip here on published-but-unechoed local would eat a row
                // the divergence arm below is licensed to ADOPT.
                let local_matches_base = self.read_base(relative_path).as_deref()
                    == Some(&local_data[..])
                    || self
                        .db
                        .get_entry(relative_path)
                        .ok()
                        .flatten()
                        .is_some_and(|e| {
                            e.recorded_content_hash == Some(ContentHash::of_raw(&local_data))
                        });
                if matches!(
                    crate::causal::judge_incoming(
                        seq_v,
                        derived_through,
                        is_resolution == Some(true),
                        is_retention == Some(true),
                        frontiers,
                        true,
                        local_matches_base,
                    ),
                    crate::causal::IncomingVerdict::ReissueOfHeldContent
                ) {
                    tracing::info!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        seq = seq_v,
                        "causal: reissue of already-incorporated content — skipped \
                         (frontier advances; the edit-frontier does not)"
                    );
                    self.causal().advance_frontier(relative_path, seq_v);
                    return Ok(DownloadOutcome::Applied);
                }
            }
        }

        crate::progress::emit(
            &self.transfer_pool.progress_tx,
            crate::progress::ProgressEvent::FileStarted {
                path: relative_path.to_string(),
                size: manifest.total_size,
                chunk_count: manifest.chunk_hashes.len(),
            },
        );

        let chunk_data = self
            .fetch_decoded_chunks(
                &manifest_hash,
                &manifest,
                relative_path,
                content_key_version,
            )
            .await?;

        // For large files use the streaming reassembler to keep memory bounded.
        // Resolve the target against the filesystem BEFORE creating any parent
        // dirs: the lexical guard at the top of this fn rejects
        // `..`, but a symlinked *intermediate* directory in the watch tree would
        // still redirect the write outside the root — and `create_dir_all` would
        // create dirs out there too. `contained_apply_target` follows the
        // symlinks and refuses an escapee, so nothing is created beyond the
        // root — PERMANENT, same class as the lexical guard above — while a
        // root that is itself unavailable (an unmounted drive) stays transient:
        // marking that permanent skipped every change a pull delivered while
        // the root was away.
        let full_path =
            fauna_core::path_guard::contained_apply_target(&self.watch_dir, relative_path)?;
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "creating parent dirs for {}",
                    fauna_core::log_redact::log_path(relative_path)
                )
            })?;
        }

        let use_streaming = manifest.total_size >= fauna_core::chunker_stream::STREAMING_THRESHOLD;

        // Reassemble the incoming version WITHOUT touching the real path — a
        // divergent local copy must survive until the conflict is resolved and
        // its version durably retained (no-user-data-loss). Streaming mode
        // reassembles into a same-directory temp file promoted by rename.
        let mut streaming_temp: Option<tempfile::NamedTempFile> = None;
        let (file_data, actual_hash) = if use_streaming {
            // Write each chunk to a temp dir then reassemble in O(MAX_CHUNK) memory.
            let temp_dir =
                tempfile::tempdir().context("creating temp dir for streaming reassembly")?;
            for (hash, data) in manifest.chunk_hashes.iter().zip(chunk_data.iter()) {
                let chunk_path = temp_dir.path().join(hex::encode(hash.digest()));
                std::fs::write(&chunk_path, data).with_context(|| {
                    format!("writing chunk to temp dir {}", hex::encode(hash.digest()))
                })?;
            }
            let chunk_paths: Vec<std::path::PathBuf> = manifest
                .chunk_hashes
                .iter()
                .map(|h| temp_dir.path().join(hex::encode(h.digest())))
                .collect();
            let chunks_for_reassembly: Vec<(ContentHash, &std::path::Path)> = manifest
                .chunk_hashes
                .iter()
                .copied()
                .zip(chunk_paths.iter().map(|p| p.as_path()))
                .collect();

            let parent = full_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| self.watch_dir.clone());
            let target = tempfile::Builder::new()
                .prefix(".fauna-reassemble-")
                .tempfile_in(&parent)
                .context("creating streaming reassembly target")?;

            // Reassemble verifies the file hash internally.
            fauna_core::chunker_stream::reassemble_streaming(
                &chunks_for_reassembly,
                target.path(),
                &manifest.file_hash,
            )
            .context("streaming reassembly")?;

            // temp_dir dropped here — chunk files cleaned up.
            drop(temp_dir);

            let actual = fauna_core::chunker_stream::content_hash_streaming(target.path())
                .context("hashing reassembled file")?;
            // Read back for the conflict check (rare; O(file_size) like the
            // merge itself).
            let file_data = tokio::fs::read(target.path()).await.with_context(|| {
                format!(
                    "reading reassembled {}",
                    fauna_core::log_redact::log_path(relative_path)
                )
            })?;
            streaming_temp = Some(target);
            (file_data, actual)
        } else {
            let file_data = fauna_core::chunker::reassemble_chunks(&chunk_data);
            let actual = ContentHash::of_raw(&file_data);
            if actual != manifest.file_hash {
                // PERMANENT: the bytes at rest do not address the hash the
                // change recorded, and refetching reproduces the same
                // mismatch (`fauna_core::apply_failure`).
                return Err(fauna_core::apply_failure::permanent(
                    fauna_core::apply_failure::PermanentApplyFailure::CONTENT_UNADDRESSED,
                    format!(
                        "file hash mismatch: expected {}, got {}",
                        hex::encode(manifest.file_hash.digest()),
                        hex::encode(actual.digest())
                    ),
                ));
            }
            (file_data, actual)
        };

        // If this path has an unresolved local conflict of a kind a propagated
        // winner exists for, the incoming change is the winner the user chose
        // (file-sync.md § Conflicts): take it verbatim and skip conflict
        // detection, which would otherwise re-conflict against the local
        // divergent copy. The local conflict row is cleared only after the
        // write lands (below), so a failed write leaves it for the next
        // catch-up. Never on a `catchup_failed` skip row: it has no winner, and
        // arming here overwrote unpublished local edits with no conflict row
        // and no retained loser (`conflicts.md` § Skipped catch-up changes
        // reach the review list) — a later change on a skipped path goes
        // through ordinary detection, and cures the skip once it lands.
        let applying_winner = self
            .db
            .has_unresolved_winner_conflict_for_path(relative_path)
            .unwrap_or(false);

        // Conflict detection + auto-resolve (`conflicts.md`, ratified
        // 2026-07-10; causal licence per clause 5) — BEFORE any mutation of
        // the real path. A divergence exists when the local file differs from
        // the incoming version and EITHER it has diverged from the cached
        // merge base (unpublished local work — the pre-ruling rule) OR the
        // incoming row is a concurrent SIBLING (watermark below this path's
        // frontier: the writer never saw content this device reflects, so
        // `local == base` proves nothing).
        let resolved_apply = if applying_winner {
            ResolvedApply::ApplyIncoming
        } else {
            let local_pre = match tokio::fs::read(&full_path).await {
                Ok(bytes) => {
                    let mtime_ms = std::fs::metadata(&full_path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(fauna_core::data::Timestamp::millis_or_zero)
                        .unwrap_or(0);
                    Some((bytes, mtime_ms))
                }
                Err(_) => None,
            };
            match local_pre {
                Some((local_data, local_mtime_ms)) if local_data != file_data => {
                    // The unpublished-work conjunct, sharpened (gap-3
                    // ruling): base-equality OR recorded-bytes equality —
                    // local content whose record provably landed
                    // (`recorded_content_hash`) is recoverable nest-side, so
                    // the verbatim licences may treat it as published even
                    // before its echo advances the base. Without this, a
                    // covering resolution judged in the ack→echo window fell
                    // to a pre-publication-ancestor merge that DUPLICATED
                    // the published edit (the live leg-4a signature).
                    let local_matches_live_base =
                        self.read_base(relative_path).as_deref() == Some(&local_data[..]);
                    let local_matches_base = local_matches_live_base
                        || self
                            .db
                            .get_entry(relative_path)
                            .ok()
                            .flatten()
                            .is_some_and(|e| {
                                e.recorded_content_hash == Some(ContentHash::of_raw(&local_data))
                            });
                    // The gap-3 class upgrade needs the REAL held witness at
                    // this judgement (an upgraded row adopts where it used
                    // to merge), so the rung's lookup is repeated here — the
                    // manifest-step pass returns only when the verdict is a
                    // skip, and this arm handles the adopt half.
                    let content_already_held =
                        self.causal().frontier(relative_path).is_some_and(|f| {
                            self.causal().holds_content_at_or_below(
                                relative_path,
                                f,
                                &manifest.file_hash,
                            )
                        });
                    let diverged = if let Some(seq_v) = seq {
                        let verdict = crate::causal::judge_incoming(
                            seq_v,
                            derived_through,
                            is_resolution == Some(true),
                            is_retention == Some(true),
                            self.causal().frontiers(relative_path),
                            content_already_held,
                            local_matches_base,
                        );
                        // The DEFER arm (`conflicts.md` clause 5; widened
                        // + converted to a transient-class CAP by the
                        // leg-4 ruling, 2026-08-05): a verbatim adopt is
                        // NEVER taken while this device's own pending
                        // rows are unlisted — the unknown-seq report
                        // window (the in-flight flag) AND the ordinary
                        // ack→echo window, where the licence rode the
                        // RECORDED witness alone (local != live base).
                        // Adopting in either window drops own novelty
                        // whose carrier then falsely advances the
                        // frontiers at its echo — the poisoned state
                        // every downstream claim inherited (the leg-4
                        // lost line). The caller holds the anchor below
                        // this seq ([`DownloadOutcome::DeferredCap`]);
                        // the next pull re-lists it with the pending
                        // rows in hand and the retried judgement is
                        // exact — the gap-3 anti-duplication adopt is
                        // delayed, never weakened. (The pre-ruling arm
                        // consumed-and-dropped the row: the leg-4
                        // repair winners died here.)
                        // The firing rule itself lives once, in
                        // `causal::verbatim_adopt_deferred` (held-bytes
                        // release, 2026-09-21): bytes the ledger already
                        // holds at or below the frontier — a revert's — are
                        // not novelty the cap protects.
                        let local_held_at_or_below_frontier =
                            self.causal().frontier(relative_path).is_some_and(|f| {
                                self.causal().holds_content_at_or_below(
                                    relative_path,
                                    f,
                                    &ContentHash::of_raw(&local_data),
                                )
                            });
                        if verdict == crate::causal::IncomingVerdict::FastForward
                            && crate::causal::verbatim_adopt_deferred(
                                self.own_novelty_in_flight(relative_path),
                                local_matches_live_base,
                                local_held_at_or_below_frontier,
                            )
                        {
                            tracing::info!(
                                path = %fauna_core::log_redact::log_path(relative_path),
                                seq = seq_v,
                                "causal: verbatim adopt deferred — own pending \
                                 rows unlisted (transient cap; anchor holds)"
                            );
                            return Ok(DownloadOutcome::DeferredCap);
                        }
                        // Reachable here (unlike before the upgrade) because
                        // this judgement now carries the real held witness:
                        // a stale-watermarked reissue is skipped, never
                        // applied nor merged (the manifest-step pass catches
                        // most of these ahead of the chunk fetch; this arm is
                        // the belt for its local-read race).
                        if verdict == crate::causal::IncomingVerdict::ReissueOfHeldContent {
                            tracing::info!(
                                path = %fauna_core::log_redact::log_path(relative_path),
                                seq = seq_v,
                                "causal: reissue of already-incorporated content — skipped \
                                 (frontier advances; the edit-frontier does not)"
                            );
                            self.causal().advance_frontier(relative_path, seq_v);
                            return Ok(DownloadOutcome::Applied);
                        }
                        matches!(verdict, crate::causal::IncomingVerdict::Diverged { .. })
                    } else {
                        !local_matches_base
                    };
                    if diverged {
                        let incoming = IncomingVersion {
                            manifest_hash,
                            size_bytes: file_data.len() as i64,
                            device_id: incoming_device_id,
                            created_at_ms: incoming_created_at_ms,
                            content_key_version,
                        };
                        // The causally-justified ancestor: the newest held
                        // version the incoming writer provably had; absent →
                        // the ladder's next rung (the live base) inside
                        // `auto_resolve_conflict`.
                        let ancestor = derived_through
                            .and_then(|w| self.causal().ancestor_at_or_below(relative_path, w));
                        self.auto_resolve_conflict(
                            relative_path,
                            &local_data,
                            local_mtime_ms,
                            &file_data,
                            &incoming,
                            seq,
                            ancestor,
                            claim_gap_all_own,
                        )
                        .await
                    } else {
                        ResolvedApply::ApplyIncoming
                    }
                }
                _ => ResolvedApply::ApplyIncoming,
            }
        };

        // The LEDGER holds the ROW's bytes (entry seq → row content), not the
        // merge result — capture before the outcome match moves them. Capped
        // like every hold; an over-cap row simply never becomes an ancestor.
        let incoming_bytes_for_ledger = (seq.is_some()
            && file_data.len() <= crate::causal::LEDGER_MAX_BYTES)
            .then(|| file_data.clone());

        // Apply the outcome to the local file + entry. The winner's nest-side
        // rows (retention + head) already landed inside the report transaction.
        // The thumbnail backfill re-records `manifest_hash` verbatim, which is
        // only correct while the incoming version stayed the head — a merged /
        // local-wins resolution moved the head past it, so backfill is gated
        // to the plain-apply outcome.
        let mut backfill_bytes: Option<Vec<u8>> = None;
        // The merged head's manifest, when the apply writes a merge rather
        // than the incoming bytes — what the store-key index records.
        let mut merged_manifest_written: Option<Box<fauna_core::chunk::ChunkManifest>> = None;
        let (final_data, write_hash, entry_manifest, entry_ckv) = match resolved_apply {
            ResolvedApply::ApplyIncoming => {
                if backfill.is_some() {
                    backfill_bytes = Some(file_data.clone());
                }
                (file_data, actual_hash, manifest_hash, content_key_version)
            }
            ResolvedApply::WriteMerged {
                merged,
                manifest: merged_manifest_body,
                manifest_hash: merged_manifest,
                content_key_version: merged_ckv,
            } => {
                // The merged head contains this device's pre-merge candidate
                // — novel content whose carrier (the report's WINNER row)
                // has not listed yet, at a seq this side cannot know: arm
                // the in-flight window on the winner's manifest until it is.
                self.note_own_novel_in_flight(
                    relative_path,
                    &hex::encode(merged_manifest.digest()),
                );
                merged_manifest_written = Some(merged_manifest_body);
                let h = ContentHash::of_raw(&merged);
                (merged, h, merged_manifest, merged_ckv)
            }
            ResolvedApply::KeepLocal {
                manifest_hash: local_manifest,
                content_key_version: local_ckv,
            } => {
                // The local file IS the winner: no disk write. Re-point the
                // entry at the uploaded local manifest (now the propagated
                // head) and refresh the base so the next incoming change
                // fast-forwards.
                let local_data = tokio::fs::read(&full_path).await.with_context(|| {
                    format!(
                        "re-reading local winner {}",
                        fauna_core::log_redact::log_path(relative_path)
                    )
                })?;
                let local_hash = ContentHash::of_raw(&local_data);
                self.save_base(relative_path, &local_data);
                let mtime = std::fs::metadata(&full_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(fauna_core::data::Timestamp::secs_or_zero)
                    .unwrap_or(0);
                self.db.upsert_entry(
                    relative_path,
                    Some(local_hash),
                    Some(local_hash),
                    Some(local_manifest),
                    SyncState::Synced,
                    mtime,
                    mtime,
                    local_data.len() as i64,
                    1,
                    local_ckv,
                )?;
                // The local winner IS the propagated head — stamp the dehydration
                // gate's proof (`local_manifest` reassembles to `local_hash`).
                self.db.stamp_recorded_content_from_local(
                    relative_path,
                    crate::db::ProofOrigin::OwnRecord,
                )?;
                // The kept head is this device's candidate, riding the
                // report's winner row at a seq this side cannot know: arm
                // the in-flight window on the winner's manifest until it
                // lists.
                self.note_own_novel_in_flight(relative_path, &hex::encode(local_manifest.digest()));
                // Causal accounting: the incoming row is resolved-against
                // (its bytes retained nest-side as a version) — the frontier
                // covers it, and its content is a held ancestor. A judged
                // EDIT advances the edit-frontier too (upper-bound law — the
                // row was latest-wins-judged, so this device's state accounts
                // for it), mirroring the model's uniform resolver-arm rule.
                if let Some(seq_v) = seq {
                    self.causal().advance_frontier(relative_path, seq_v);
                    // Content-licensed (gap-3 ruling): a judged edit whose
                    // bytes are already held at a lower seq carries no novel
                    // content — only the frontier advances.
                    if is_resolution != Some(true)
                        && !self.causal().holds_content_at_or_below(
                            relative_path,
                            seq_v - 1,
                            &ContentHash::of_raw(&file_data),
                        )
                    {
                        self.causal().advance_edit_frontier(relative_path, seq_v);
                    }
                    if let Some(row_bytes) =
                        (file_data.len() <= crate::causal::LEDGER_MAX_BYTES).then_some(&file_data)
                    {
                        self.causal().hold(relative_path, seq_v, row_bytes);
                    }
                    self.causal().remember_manifest(
                        relative_path,
                        seq_v,
                        &hex::encode(manifest_hash.digest()),
                    );
                }
                tracing::info!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    "conflict auto-resolved: local version wins (kept, propagated as head)"
                );
                // The kept copy is the path's head: a skip recorded here is
                // no longer true.
                self.cure_skipped_path(relative_path).await;
                return Ok(DownloadOutcome::Applied);
            }
            ResolvedApply::Unresolved => {
                // Fail-closed: local file kept; the unresolved conflict row +
                // unresolved report own the divergence (chooser flow / retry).
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    "conflict could not be auto-resolved — local file kept, reported unresolved"
                );
                return Ok(DownloadOutcome::Applied);
            }
        };
        let write_size = final_data.len() as i64;

        // Mark as recently written so the watcher doesn't re-upload
        self.note_recent_download(relative_path);

        // Commit to disk: promote the streaming temp by rename when it holds
        // exactly the bytes to write; else one atomic write.
        match streaming_temp {
            Some(temp) if write_hash == actual_hash => {
                temp.persist(&full_path)
                    .map_err(|e| anyhow::anyhow!("promoting reassembled file: {e}"))?;
            }
            _ => {
                crate::atomic_write::atomic_write_file(&full_path, &final_data)
                    .await
                    .with_context(|| {
                        format!(
                            "atomic write {}",
                            fauna_core::log_redact::log_path(relative_path)
                        )
                    })?;
            }
        }

        // Save the written content as the merge base for future 3-way merges.
        // This is set on every download so the base tracks the last-synced version.
        self.save_base(relative_path, &final_data);
        // Causal accounting (`conflicts.md` clause 5): the applied row is now
        // the newest content this path reflects, and its bytes are a held
        // version a later sibling can merge from. An applied/merged EDIT
        // advances the edit-frontier; a resolution does not (no novel
        // content — the gap-2 ruling's covering test depends on this), and
        // neither does an edit-stamped row whose bytes are already held at a
        // lower seq (gap-3's content licence).
        if let Some(seq_v) = seq {
            self.causal().advance_frontier(relative_path, seq_v);
            if is_resolution != Some(true)
                && !self.causal().holds_content_at_or_below(
                    relative_path,
                    seq_v - 1,
                    &manifest.file_hash,
                )
            {
                self.causal().advance_edit_frontier(relative_path, seq_v);
            }
            if let Some(row_bytes) = &incoming_bytes_for_ledger {
                self.causal().hold(relative_path, seq_v, row_bytes);
            }
            self.causal().remember_manifest(
                relative_path,
                seq_v,
                &hex::encode(manifest_hash.digest()),
            );
        }

        // Update local state
        let mtime = std::fs::metadata(&full_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(fauna_core::data::Timestamp::secs_or_zero)
            .unwrap_or(0);

        self.db.upsert_entry(
            relative_path,
            Some(write_hash),
            Some(write_hash),
            Some(entry_manifest),
            SyncState::Synced,
            mtime,
            mtime,
            write_size,
            1,
            entry_ckv, // pairs with the entry's manifest generation
        )?;
        // The written bytes ARE the applied head (`entry_manifest` reassembles to
        // `write_hash`) — stamp the dehydration gate's proof so freeing this
        // downloaded/merged file is allowed. Incoming bytes came from another
        // holder; a merge is a body this device made and recorded itself.
        let origin = if merged_manifest_written.is_some() {
            crate::db::ProofOrigin::OwnRecord
        } else {
            crate::db::ProofOrigin::Fetched
        };
        self.db
            .stamp_recorded_content_from_local(relative_path, origin)?;
        // Index the body just written — the incoming manifest's content, or the
        // merged head's — so this seat serves it.
        match &merged_manifest_written {
            Some(merged) => self.index_held_body(relative_path, merged, entry_ckv),
            None => self.index_held_body(relative_path, &manifest, content_key_version),
        }

        // Winner applied and synced: clear the local conflict so it no longer
        // shows as unresolved (the nest already recorded the winner on resolve).
        if applying_winner && self.db.resolve_winner_conflicts_for_path(relative_path)? {
            tracing::info!(
                path = %fauna_core::log_redact::log_path(relative_path),
                "applied propagated conflict winner and cleared local conflict"
            );
        }
        // A later change on this path landed: a skip recorded here is no
        // longer true.
        self.cure_skipped_path(relative_path).await;

        tracing::info!(
            path = %fauna_core::log_redact::log_path(relative_path),
            chunks = manifest.chunk_hashes.len(),
            size = write_size,
            streaming = use_streaming,
            "file downloaded and written"
        );

        crate::progress::emit(
            &self.transfer_pool.progress_tx,
            crate::progress::ProgressEvent::FileDone {
                path: relative_path.to_string(),
            },
        );
        // Content moved (download side): stamp the honest `last_transfer_at`.
        if let Err(e) = self.db.mark_transfer_completed() {
            tracing::warn!(error = ?e, "failed to stamp last_transfer_at");
        }

        // Seam B: back-fill a thumbnail for a file whose change was recorded
        // without one. We hold the just-downloaded plaintext
        // (`file_data`, which matches `manifest_hash` — not `final_data`, a
        // possible text-merge result), so generate + seal + upload the missing
        // thumbnail and re-record the file with it. `manifest_hash` + the
        // caller's `size_bytes` are re-recorded verbatim so the nest's quota
        // delta stays 0 (only `thumbnail_hash` changes).
        if let (Some(size_bytes), Some(incoming_plaintext)) = (backfill, backfill_bytes) {
            self.maybe_backfill_thumbnail(
                relative_path,
                &incoming_plaintext,
                manifest_hash,
                size_bytes,
                content_key_version,
            )
            .await;
        }

        Ok(DownloadOutcome::Applied)
    }

    /// Handle a self-echo: download the file data and update the merge base,
    /// but only if the local file hasn't been modified by a merge in this batch.
    /// `content_key_version` is the generation the echoed change was sealed under
    /// (selects the open key, fail-closed for a bound set).
    async fn apply_self_echo(
        &self,
        relative_path: &str,
        manifest_hash: ContentHash,
        content_key_version: Option<u64>,
        seq: Option<i64>,
        derived_through: Option<i64>,
        is_resolution: Option<bool>,
    ) -> Result<()> {
        let manifest = self
            .fetch_manifest(&manifest_hash, content_key_version)
            .await?;
        let chunk_data = self
            .fetch_decoded_chunks(
                &manifest_hash,
                &manifest,
                relative_path,
                content_key_version,
            )
            .await?;
        let file_data = fauna_core::chunker::reassemble_chunks(&chunk_data);

        // Causal accounting rides the SAME event as the base advance
        // (`conflicts.md` clause 5's pairing rule): the echo proves the log
        // holds this device's row at `seq`, so the frontier covers it and the
        // published bytes become a held ancestor for a later sibling merge.
        // An own EDIT echo advances the edit-frontier too; an own resolution
        // (or proven-reissue) echo does not — no novel content — and neither
        // does an own edit-stamped echo whose bytes the ledger already holds
        // at a lower seq (gap-3's content licence: the lost-ack retry whose
        // original landed; counting it at the reissue's seq is the inflation
        // that permanently stranded covering resolutions on the same-anchor
        // shape).
        let reissue_of_held = seq.is_some_and(|s| {
            self.causal().holds_content_at_or_below(
                relative_path,
                s - 1,
                &ContentHash::of_raw(&file_data),
            )
        });
        if let Some(seq) = seq {
            self.causal().advance_frontier(relative_path, seq);
            if is_resolution != Some(true) && !reissue_of_held {
                self.causal().advance_edit_frontier(relative_path, seq);
            }
            self.causal().hold(relative_path, seq, &file_data);
            self.causal().remember_manifest(
                relative_path,
                seq,
                &hex::encode(manifest_hash.digest()),
            );
        }

        // Only update the base if the local file still matches what we uploaded.
        // If a merge already changed the file, we must NOT overwrite the new
        // base — EXCEPT under the author-blind supersession arm (gap-2
        // ruling, extended by gap-3's class upgrade): an own novel-content-
        // free row — a stamped resolution OR a reissue of held bytes — is
        // still a log entry, and when it is LATER than whatever row local
        // currently sits on it re-adopts — even when the frontier already
        // passed it (same-batch skip-accounts run ahead of the echoes, so
        // the tail's own echo routinely arrives "late"; without this arm the
        // publisher of the log-tail permutation is the one device that never
        // converges to it). The ordering guard is a ledger lookup — the
        // newest held row whose bytes equal local — because seq-vs-frontier
        // cannot say which row local came from. Deliberately NOT
        // `judge_incoming`: its rule 1 reads a late own echo as a duplicate.
        //
        // Resolve against the filesystem (for uniformity with
        // the two attacker-fed doors): own rows are not attacker-controlled, so
        // an escapee here is not expected, but the same containment guard runs
        // at every `watch_dir.join` write door rather than one door's shape
        // diverging.
        let Some(full_path) =
            crate::path_guard::resolved_target_within_root(&self.watch_dir, relative_path)
        else {
            anyhow::bail!(
                "self-echo path escapes the sync root after symlink resolution: {}",
                fauna_core::log_redact::log_path(relative_path)
            );
        };
        if let Ok(local_data) = tokio::fs::read(&full_path).await {
            let log_later_tail = (is_resolution == Some(true) || reissue_of_held)
                && local_data != file_data
                && seq.is_some_and(|s| {
                    let local_at = self
                        .causal()
                        .newest_held_seq_matching(relative_path, &local_data)
                        .unwrap_or(0);
                    s > local_at
                        && self.read_base(relative_path).as_deref() == Some(&local_data[..])
                        && !self.own_novelty_in_flight(relative_path)
                });
            let covering = derived_through.is_some_and(|w| {
                self.causal()
                    .frontiers(relative_path)
                    .effective_edit_frontier()
                    .is_some_and(|ef| w >= ef)
            });
            if local_data == file_data {
                self.save_base(relative_path, &file_data);
                // The in-flight flag clears exactly here: an own row's bytes
                // came back equal to local, so everything local reflects is
                // provably in the log at seqs the frontiers now account for.
                self.retire_own_novel_in_flight(
                    relative_path,
                    &hex::encode(manifest_hash.digest()),
                );
                tracing::debug!(path = %fauna_core::log_redact::log_path(relative_path), "self-echo: updated merge base");
            } else if log_later_tail && covering {
                self.note_recent_download(relative_path);
                crate::atomic_write::atomic_write_file(&full_path, &file_data)
                    .await
                    .with_context(|| {
                        format!(
                            "re-adopting own resolution {}",
                            fauna_core::log_redact::log_path(relative_path)
                        )
                    })?;
                self.save_base(relative_path, &file_data);
                // The row follows the disk, exactly as the download path's
                // does: the re-adopted bytes ARE this row's head. Left on the
                // previous permutation's identity, the row read as a local
                // edit to the delete arm, which declined the next genuine
                // delete and filed an edit-wins report that recreated the file
                // on every seat (`delete-propagation.md` — a tombstone applies
                // only where the disk still hashes to the row).
                let head_hash = ContentHash::of_raw(&file_data);
                let mtime = std::fs::metadata(&full_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(fauna_core::data::Timestamp::secs_or_zero)
                    .unwrap_or(0);
                self.db.upsert_entry(
                    relative_path,
                    Some(head_hash),
                    Some(head_hash),
                    Some(manifest_hash),
                    SyncState::Synced,
                    mtime,
                    mtime,
                    file_data.len() as i64,
                    1,
                    content_key_version,
                )?;
                self.db.stamp_recorded_content_from_local(
                    relative_path,
                    crate::db::ProofOrigin::OwnRecord,
                )?;
                // Same clear as the arm above: local now equals an own
                // published row's bytes.
                self.retire_own_novel_in_flight(
                    relative_path,
                    &hex::encode(manifest_hash.digest()),
                );
                tracing::info!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    "self-echo: own covering resolution re-adopted (author-blind \
                     supersession, conflicts.md clause 5)"
                );
            } else if log_later_tail {
                // RE-ASSERT on a stale-declined own tail (gap-3 ruling): this
                // own novel-content-free row would re-adopt by log order, and
                // the ONLY refusal is the staleness guard — a stamp-vs-content
                // comparison that is genuinely undecidable here (the
                // edit-frontier honestly counts an unprovable reissue's seq;
                // the row's stamp honestly under-claims). A resolution is an
                // ordinary change (clause 1), so the device re-asserts its
                // CURRENT bytes at the current anchor by re-driving the
                // ordinary upload: the ledger holds local (it came from an
                // adopted row), so the widened proven-reissue proof stamps
                // the record `is_resolution = true` and the fresh log-tail
                // row is covering by construction wherever this one was in
                // content — the fleet converges onto it.
                match self.upload_file(relative_path).await {
                    Ok(_) => tracing::info!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        "self-echo: own tail declined stale — re-asserted local at the \
                         current anchor (gap-3 ruling)"
                    ),
                    Err(e) => tracing::warn!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        error = %format!("{e:#}"),
                        "self-echo: re-asserting stale-declined own tail failed \
                         (the rescan retries)"
                    ),
                }
            } else {
                tracing::debug!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    "self-echo: skipped base update (file modified by merge)"
                );
            }
        }

        Ok(())
    }

    /// Directory for storing merge-base versions (hidden, ignored by watcher).
    fn base_dir(&self) -> PathBuf {
        self.base_dir.clone()
    }

    /// The two resolved state roots, for the scoping pins.
    #[cfg(test)]
    pub(crate) fn base_dir_for_test(&self) -> PathBuf {
        self.base_dir.clone()
    }

    #[cfg(test)]
    pub(crate) fn causal_dir_for_test(&self) -> PathBuf {
        self.causal_dir.clone()
    }

    /// Per-path causal state (frontier + ledger — the 2026-08-02 ruling,
    /// `conflicts.md` clause 5), beside the merge bases and hidden the same
    /// way.
    pub(crate) fn causal(&self) -> crate::causal::CausalStore {
        crate::causal::CausalStore::new(self.causal_dir.clone())
    }

    /// Resolve one of this engine's two per-path state roots to its
    /// per-`(device, set)` subdirectory (nothing is carried in from the root:
    /// the pre-scoping flat layout predates the compat-remnant sweep).
    ///
    /// ⚠ **Both roots are keyed by `path_hash` alone while `seq` is per SET**,
    /// so a watch directory bound to a second set read the first set's state
    /// as its own. For the causal store that means the frontier only grows and
    /// rule 1 skips the new set's lower seqs as duplicates — rows never
    /// applied, with the anchor advancing past them; for the merge bases it is
    /// the data-loss shape `conflicts.md` § Implementation status today spells
    /// out (a foreign base that happens to equal the local bytes reads as "no
    /// local change" and overwrites a genuinely diverged edit silently). One
    /// construction closes both: the scope lives in the DIRECTORY, so the collision is
    /// unrepresentable rather than merely documented.
    ///
    /// Resolved once, at construction: it creates a directory, and both
    /// accessors above are read on hot paths. An engine with **no** set
    /// (`folder: None` — one-shot fixtures and the plaintext-era shape) has
    /// nothing to scope by and keeps the flat root exactly as before.
    pub(crate) fn resolve_state_dir(
        watch_dir: &Path,
        folder: Option<&str>,
        device_id: &[u8; 32],
        leaf: &str,
    ) -> PathBuf {
        let root = watch_dir.join(leaf);
        match folder {
            Some(folder) => {
                crate::causal::open_scoped_store_dir(&root, &hex::encode(device_id), folder)
            }
            None => root,
        }
    }

    /// Arm the in-flight window for `path` on one carrier MANIFEST (see the
    /// field doc on `own_novel_in_flight`). Per-carrier since the
    /// same-anchor ruling (2026-08-05, conjunct 4): a single per-path bit
    /// let an OLDER report's rows clear the window while a NEWER report's
    /// novelty-carrying winner was still unlisted, releasing the covering
    /// adopt over it.
    fn note_own_novel_in_flight(&self, relative_path: &str, manifest_hex: &str) {
        self.own_novel_in_flight.note(relative_path, manifest_hex);
    }

    /// Retire ONE carrier from the path's in-flight window, by manifest —
    /// called when the carrier's row lists (the partition pre-pass) or
    /// echoes. A RETENTION row retires nothing (the loser is not the
    /// carrier — the same-anchor ruling, conjunct 4).
    fn retire_own_novel_in_flight(&self, relative_path: &str, manifest_hex: &str) {
        self.own_novel_in_flight.retire(relative_path, manifest_hex);
    }

    /// Is an own novel publication still awaiting its listing on this path?
    fn own_novelty_in_flight(&self, relative_path: &str) -> bool {
        self.own_novel_in_flight.is_in_flight(relative_path)
    }

    /// Save the current file content as the merge base for future 3-way merges.
    fn save_base(&self, relative_path: &str, data: &[u8]) {
        let base_path = self.base_dir().join(relative_path);
        if let Some(parent) = base_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            // Best-effort base cache; failure just falls back to a 2-way merge
            // next time (observability.md category 3).
            tracing::debug!(error = ?e, "save_base: create base dir failed");
        }
        if let Err(e) = std::fs::write(&base_path, data) {
            tracing::debug!(error = ?e, "save_base: write merge base failed");
        }
    }

    /// Read the stored merge base for a file, if any.
    fn read_base(&self, relative_path: &str) -> Option<Vec<u8>> {
        let base_path = self.base_dir().join(relative_path);
        std::fs::read(&base_path).ok()
    }

    /// Report a conflict (with candidate versions) to the nest over WS-RPC
    /// (`fauna.sync.conflicts.report`). Best-effort: a failed report degrades
    /// to local-only (the conflict is still recorded in the engine's `SyncDb`).
    /// Send one `fauna.sync.conflicts.report`. With `resolution = Some(..)`
    /// this is the AUTO-RESOLVE shape (ratified 2026-07-10): the nest lands
    /// the conflict resolved AND transactionally retains the loser + records
    /// the winner head row (`file-sync.md` § Conflicts) — so the caller must
    /// treat an `Err` as "nothing landed" and fail closed. `None` = the unresolved
    /// (chooser) report.
    async fn report_conflict_ws(
        &self,
        folder: &str,
        relative_path: &str,
        conflict_type: &str,
        details: Option<String>,
        candidates: Vec<ConflictCandidate>,
        resolution: Option<ReportResolution>,
    ) -> anyhow::Result<i64> {
        let (
            resolution,
            winning_manifest_hash,
            winning_size_bytes,
            winning_content_key_version,
            winning_derived_through,
            losing_derived_through,
            winning_carries_novelty,
        ) = match resolution {
            Some(r) => (
                Some(r.kind.to_string()),
                Some(r.winning_manifest_hex),
                r.winning_size_bytes,
                r.winning_content_key_version,
                r.winning_derived_through,
                r.losing_derived_through,
                r.winning_carries_novelty,
            ),
            None => (None, None, None, None, None, None, None),
        };
        // Seal the conflict's path + details under this engine's label root
        // (path-sealing S6-a). Same root, salt and tag as every other path this
        // engine records — `seal_recorded_path` and this share one funnel — so a
        // conflict row renders for exactly the audience its file rows do.
        //
        // `label_seal_root` returning `None` is the unbound owner-only engine
        // that holds no root at all; it reports plaintext-only exactly as it
        // records plaintext-only. Its `Err` is FS-BIND-5 (bound set, content
        // keys not loaded) and propagates: a bound keyless engine refuses to
        // report a name the nest could read, the same fail-closed posture
        // `record_change` takes.
        let root = self.label_seal_root()?;
        let (path_sealed, details_sealed) = match &root {
            Some(root) => (
                Some(fauna_protocol::ByteBuf::from(
                    fauna_core::label_custody::seal_path(root, relative_path)?,
                )),
                details
                    .as_deref()
                    .map(|d| {
                        fauna_core::label_custody::seal_conflict_details(root, relative_path, d)
                            .map(fauna_protocol::ByteBuf::from)
                    })
                    .transpose()?,
            ),
            None => (None, None),
        };
        let mut req = ConflictReportRequest {
            folder: folder.to_string(),
            device_id: hex::encode(self.device_id),
            path: relative_path.to_string(),
            conflict_type: conflict_type.to_string(),
            details,
            candidates,
            resolution,
            winning_manifest_hash,
            winning_size_bytes,
            winning_content_key_version,
            winning_derived_through,
            losing_derived_through,
            winning_carries_novelty,
            path_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::sync::path_hash(relative_path).to_vec(),
            )),
            path_sealed,
            details_sealed,
            ..Default::default()
        };
        // Sign LAST: every covered field above holds its final value.
        self.sign_conflict_report(&mut req);
        let control = Arc::clone(&*self.control.read().unwrap());
        let id = control
            .report_conflict(req)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        tracing::info!(
            path = %fauna_core::log_redact::log_path(relative_path),
            conflict_id = id,
            "reported conflict to nest (WS-RPC)"
        );
        Ok(id)
    }

    /// Record a permanently un-appliable catch-up change and report it to the
    /// nest (`conflicts.md` § Skipped catch-up changes reach the review list).
    /// Once per path: a skip already on record is neither recorded nor
    /// reported again here — an unacknowledged one is re-sent by
    /// [`Self::flush_skip_reports`].
    async fn record_and_report_skip(
        &self,
        path: &str,
        path_is_hash: bool,
        path_sealed: Option<&[u8]>,
        details: &str,
    ) {
        match self
            .db
            .record_skipped_change(path, path_is_hash, path_sealed, Some(details))
        {
            Ok(Some(skip)) => self.report_skipped_change(&skip).await,
            Ok(None) => {}
            Err(e) => tracing::warn!(error = ?e, "recording a skipped change failed"),
        }
    }

    /// Report one recorded skip to the nest as an unresolved, candidate-free
    /// `catchup_failed` conflict and stamp the reply's id on the local row.
    /// Best-effort: a failed report leaves `nest_id` NULL and the next catch-up
    /// pass re-sends it.
    ///
    /// A row with a plaintext path reports through the ordinary funnel
    /// ([`Self::report_conflict_ws`]); a row whose sealed path never opened
    /// forwards that change row's label pair ([`Self::report_sealed_skip_ws`]);
    /// a seal-less one cannot be filed anywhere and stays local. A cross-nest
    /// writer's skip stays local until the cross-nest conflict-report relay
    /// exists (`ui/folders.md` § Sharing a folder, the named v1 gap).
    async fn report_skipped_change(&self, skip: &crate::db::SkippedChange) {
        let Some(folder) = self.folder.clone() else {
            return;
        };
        if self.foreign_routing().is_some() {
            return;
        }
        let sent = if skip.path_is_hash {
            let Some(path_sealed) = skip.path_sealed.clone() else {
                return;
            };
            self.report_sealed_skip_ws(&folder, &skip.path, path_sealed, skip.details.clone())
                .await
        } else {
            self.report_conflict_ws(
                &folder,
                &skip.path,
                crate::db::CATCHUP_FAILED,
                skip.details.clone(),
                Vec::new(),
                None,
            )
            .await
        };
        match sent {
            Ok(nest_id) => {
                if let Err(e) = self.db.set_conflict_nest_id(skip.id, nest_id) {
                    tracing::warn!(error = ?e, "stamping a skip report's nest id failed");
                }
            }
            Err(e) => tracing::warn!(
                error = %e,
                "reporting a skipped change failed; the next catch-up pass re-sends it"
            ),
        }
    }

    /// The sealed-path sibling of [`Self::report_conflict_ws`]: the skipped
    /// change's sealed path never opened here, so there is no plaintext to
    /// seal. It forwards the change row's own label pair — `path_sealed` as
    /// the nest served it, `path_hash` the row's own when it is 32 hex bytes,
    /// else the BLAKE3 of its raw string — with `path` empty and `details`
    /// sealed by that same hash. Unresolved, so unsigned.
    async fn report_sealed_skip_ws(
        &self,
        folder: &str,
        row_path_hash: &str,
        path_sealed: Vec<u8>,
        details: Option<String>,
    ) -> anyhow::Result<i64> {
        let path_hash: [u8; 32] = hex::decode(row_path_hash)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .unwrap_or_else(|| *blake3::hash(row_path_hash.as_bytes()).as_bytes());
        let root = self.label_seal_root()?;
        let details_sealed = match (&root, details.as_deref()) {
            (Some(root), Some(d)) => Some(fauna_protocol::ByteBuf::from(
                fauna_core::label_custody::seal_conflict_details_by_hash(root, &path_hash, d)?,
            )),
            _ => None,
        };
        let req = ConflictReportRequest {
            folder: folder.to_string(),
            device_id: hex::encode(self.device_id),
            path: String::new(),
            conflict_type: crate::db::CATCHUP_FAILED.to_string(),
            details,
            path_hash: Some(fauna_protocol::ByteBuf::from(path_hash.to_vec())),
            path_sealed: Some(fauna_protocol::ByteBuf::from(path_sealed)),
            details_sealed,
            ..Default::default()
        };
        let control = Arc::clone(&*self.control.read().unwrap());
        let id = control
            .report_conflict(req)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        tracing::info!(
            conflict_id = id,
            "reported a skipped sealed-path change to nest"
        );
        Ok(id)
    }

    /// Re-send every skip report an earlier attempt could not land, then every
    /// candidate-free resolve a cured skip still owes its nest row.
    pub(crate) async fn flush_skip_reports(&self) {
        if self.folder.is_none() || self.foreign_routing().is_some() {
            return;
        }
        match self.db.list_unreported_catchup_failures() {
            Ok(rows) => {
                for skip in &rows {
                    self.report_skipped_change(skip).await;
                }
            }
            Err(e) => tracing::warn!(error = ?e, "listing unreported skips failed"),
        }
        match self.db.list_catchup_failures_owing_nest_resolve() {
            Ok(rows) => {
                for skip in &rows {
                    self.resolve_skip_on_nest(skip).await;
                }
            }
            Err(e) => tracing::warn!(error = ?e, "listing skips owing a nest resolve failed"),
        }
    }

    /// The cure: a later change to `relative_path` landed on this device, so
    /// every skip recorded under it — by path, or by its hash when the sealed
    /// path never opened — is resolved locally, and each reported one's nest
    /// row gets the candidate-free resolve.
    async fn cure_skipped_path(&self, relative_path: &str) {
        let hash_hex = hex::encode(fauna_core::sync::path_hash(relative_path));
        let cured = match self
            .db
            .cure_catchup_failures_for_path(relative_path, &hash_hex)
        {
            Ok(cured) => cured,
            Err(e) => {
                tracing::warn!(error = ?e, "curing a skipped path failed");
                return;
            }
        };
        for skip in &cured {
            tracing::info!(
                path = %fauna_core::log_redact::log_path(relative_path),
                "a later change landed on a skipped path; the skip is cured"
            );
            self.resolve_skip_on_nest(skip).await;
        }
    }

    /// Send the candidate-free `conflicts.resolve` for a cured skip's nest
    /// row. A row never reported (no `nest_id`) owes nothing; a failure leaves
    /// the resolve owed for [`Self::flush_skip_reports`].
    async fn resolve_skip_on_nest(&self, skip: &crate::db::SkippedChange) {
        let Some(nest_id) = skip.nest_id else {
            return;
        };
        let control = Arc::clone(&*self.control.read().unwrap());
        let req = fauna_protocol::folders::ConflictResolveRequest {
            id: nest_id,
            ..Default::default()
        };
        match control.resolve_conflict(req).await {
            // `false` = the nest row is already resolved (or gone): nothing
            // more is owed either way.
            Ok(_) => {
                if let Err(e) = self.db.set_conflict_nest_resolved(skip.id) {
                    tracing::warn!(error = ?e, "stamping a skip's nest resolve failed");
                }
            }
            Err(e) => tracing::warn!(
                conflict_id = nest_id,
                error = %e,
                "resolving a cured skip on the nest failed; the next catch-up pass re-sends it"
            ),
        }
    }

    /// Auto-resolve a detected divergence between the local file and an
    /// incoming version (`file-sync.md` § Conflicts, ratified 2026-07-10) and
    /// report it to the nest — which transactionally retains the losing
    /// version + propagates the winner (slice 3a), so this fn does NO change
    /// recording of its own. Returns what the caller must do to the local
    /// file; **nothing on disk is mutated here**.
    ///
    /// Fail-closed: the local version's upload and the resolved report must
    /// both succeed BEFORE the caller may overwrite the local file. On any
    /// failure this degrades to today's posture — a local unresolved conflict
    /// row + an unresolved (chooser) report, local file kept — and returns
    /// [`ResolvedApply::Unresolved`].
    /// `incoming_seq` — the incoming row's set seq when known (the in-order
    /// apply pass makes it the honest `winning_derived_through` claim for the
    /// propagated resolution — `conflicts.md` clause 5). `ancestor_override` —
    /// a causally-justified `(seq, bytes)` ancestor from the ledger; `None`
    /// falls back to the live base slot exactly as before the ruling.
    #[allow(clippy::too_many_arguments)]
    async fn auto_resolve_conflict(
        &self,
        relative_path: &str,
        local_data: &[u8],
        local_modified_at_ms: i64,
        incoming_bytes: &[u8],
        incoming: &IncomingVersion,
        incoming_seq: Option<i64>,
        ancestor_override: Option<(i64, Vec<u8>)>,
        claim_gap_all_own: bool,
    ) -> ResolvedApply {
        let Some(folder) = self.folder.clone() else {
            // No folder ⇒ nowhere to report; keep local, surface locally.
            let _ = self
                .db
                .record_conflict(relative_path, "concurrent_edit", None);
            return ResolvedApply::Unresolved;
        };

        let adapter = self.format_registry.adapter_for(relative_path);
        let (ancestor_seq, base) = match ancestor_override {
            Some((seq, bytes)) => (Some(seq), Some(bytes)),
            None => (None, self.read_base(relative_path)),
        };
        let resolution = crate::conflict_resolver::resolve_conflict(
            self.conflict_policy,
            adapter,
            base.as_deref(),
            local_data,
            local_modified_at_ms,
            incoming_bytes,
            incoming.created_at_ms,
        );

        // The HONEST winner claim — `CausalStore::honest_winner_claim`, the
        // one shared derivation, exactly as
        // the edit and resolution stamps already share `honest_anchor`. It
        // holds the leg-4 frontier bound (2026-08-05) AND both skip floors.
        // The arm used to be exempt from both, on an argument — a live skip
        // sits above the path's frontier, and this claim is frontier-bounded —
        // that covers neither the set-wide floor (no release, belongs to no
        // path) nor the own-gap-widened branch (not frontier-bounded at all)
        // .
        let honest_w = incoming_seq.map(|s| {
            self.causal()
                .honest_winner_claim(relative_path, s, claim_gap_all_own)
        });

        // The same-anchor ruling (2026-08-05), conjunct 2: a report that
        // consumes UNPUBLISHED local novelty — pre-merge content with no seq
        // anywhere (≠ the live base slot, ≠ the recorded bytes) — makes the
        // winner row that novelty's only carrier, so it must mint EDIT-class
        // (`winning_carries_novelty`; a resolution stamp on it licensed the
        // byte-free stale-skips that killed the live leg-4a append).
        let consumed_unpublished = self.read_base(relative_path).as_deref() != Some(local_data)
            && !self
                .db
                .get_entry(relative_path)
                .ok()
                .flatten()
                .is_some_and(|e| e.recorded_content_hash == Some(ContentHash::of_raw(local_data)));

        // The local version must be durably uploaded before anything else —
        // it is the version that would otherwise exist nowhere but this disk.
        let local_uploaded = match self
            .upload_chunked_bytes(local_data, relative_path, /* enqueue_resume = */ false)
            .await
        {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    error = %e,
                    "conflict: uploading the local version failed — keeping local, reporting unresolved"
                );
                let _ = self
                    .db
                    .record_conflict(relative_path, "concurrent_edit", None);
                let _ = self
                    .report_conflict_ws(
                        &folder,
                        relative_path,
                        "concurrent_edit",
                        Some("local-version upload failed; unresolved".to_string()),
                        Vec::new(),
                        None,
                    )
                    .await;
                return ResolvedApply::Unresolved;
            }
        };
        let local_manifest_hex = hex::encode(local_uploaded.manifest_hash.digest());
        let candidates = build_conflict_candidates(
            local_manifest_hex.clone(),
            local_data.len() as i64,
            local_uploaded.content_key_version,
            hex::encode(self.device_id),
            incoming,
            now_unix_secs(),
        );

        // Fall back to the unresolved report (chooser flow) when the
        // resolved report cannot land — the candidates are uploaded, so the
        // user can still pick either version.
        macro_rules! fallback_unresolved {
            ($err:expr) => {{
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    error = %$err,
                    "conflict: resolved report failed — keeping local, reporting unresolved"
                );
                let _ = self
                    .db
                    .record_conflict(relative_path, "concurrent_edit", None);
                let _ = self
                    .report_conflict_ws(
                        &folder,
                        relative_path,
                        "concurrent_edit",
                        Some("auto-resolve report failed; unresolved".to_string()),
                        candidates.clone(),
                        None,
                    )
                    .await;
                return ResolvedApply::Unresolved;
            }};
        }

        match resolution {
            crate::conflict_resolver::ConflictResolution::Merged(merged) => {
                // The merged head must also be fetchable before it is reported
                // as the winner.
                let merged_uploaded = match self
                    .upload_chunked_bytes(&merged, relative_path, false)
                    .await
                {
                    Ok(u) => u,
                    Err(e) => fallback_unresolved!(e),
                };
                let report = ReportResolution {
                    kind: "merged",
                    winning_manifest_hex: hex::encode(merged_uploaded.manifest_hash.digest()),
                    winning_size_bytes: Some(merged.len() as i64),
                    winning_content_key_version: merged_uploaded.content_key_version,
                    winning_derived_through: honest_w,
                    losing_derived_through: ancestor_seq,
                    winning_carries_novelty: consumed_unpublished.then_some(true),
                };
                match self
                    .report_conflict_ws(
                        &folder,
                        relative_path,
                        "concurrent_edit",
                        Some("auto-resolved: clean three-way merge".to_string()),
                        candidates.clone(),
                        Some(report),
                    )
                    .await
                {
                    Ok(_) => ResolvedApply::WriteMerged {
                        merged,
                        manifest: Box::new(merged_uploaded.manifest),
                        manifest_hash: merged_uploaded.manifest_hash,
                        content_key_version: merged_uploaded.content_key_version,
                    },
                    Err(e) => fallback_unresolved!(e),
                }
            }
            crate::conflict_resolver::ConflictResolution::LocalWins => {
                let report = ReportResolution {
                    kind: "latest_wins",
                    winning_manifest_hex: local_manifest_hex,
                    winning_size_bytes: None,
                    winning_content_key_version: None,
                    winning_derived_through: honest_w,
                    losing_derived_through: ancestor_seq,
                    winning_carries_novelty: consumed_unpublished.then_some(true),
                };
                match self
                    .report_conflict_ws(
                        &folder,
                        relative_path,
                        "concurrent_edit",
                        Some("auto-resolved: local version is the latest writer".to_string()),
                        candidates.clone(),
                        Some(report),
                    )
                    .await
                {
                    Ok(_) => ResolvedApply::KeepLocal {
                        manifest_hash: local_uploaded.manifest_hash,
                        content_key_version: local_uploaded.content_key_version,
                    },
                    Err(e) => fallback_unresolved!(e),
                }
            }
            crate::conflict_resolver::ConflictResolution::IncomingWins => {
                let report = ReportResolution {
                    kind: "latest_wins",
                    winning_manifest_hex: hex::encode(incoming.manifest_hash.digest()),
                    winning_size_bytes: None,
                    winning_content_key_version: None,
                    winning_derived_through: honest_w,
                    losing_derived_through: ancestor_seq,
                    // The winner is the peer's existing row's bytes — never a
                    // novelty carrier.
                    winning_carries_novelty: None,
                };
                match self
                    .report_conflict_ws(
                        &folder,
                        relative_path,
                        "concurrent_edit",
                        Some("auto-resolved: incoming version is the latest writer".to_string()),
                        candidates.clone(),
                        Some(report),
                    )
                    .await
                {
                    Ok(_) => ResolvedApply::ApplyIncoming,
                    Err(e) => fallback_unresolved!(e),
                }
            }
        }
    }

    /// A remote delete was declined in favor of local content the nest lacks —
    /// the **delete-vs-edit conflict** (file-sync.md § Conflicts, ratified
    /// 2026-07-29). The edit's survival is forced, not policy: the bytes exist
    /// nowhere but this disk, so applying the tombstone would destroy their
    /// only copy (`principles.md` § No user-data loss). What is NOT forced is
    /// the silence this path used to have — the survivor's re-upload made the
    /// file "reappear" on the deleting device with no explanation.
    ///
    /// So the conflict is reported ALREADY RESOLVED, latest-wins, with the
    /// surviving content as the winner: upload it first (retention-first, the
    /// same order `auto_resolve_conflict` uses), then send one resolved
    /// report. The losing version — the deletion — needs no retention step of
    /// its own: the tombstone is a recorded change, and every recorded change
    /// IS a version (§ File Versions). The nest's propagate step writes the
    /// winner as the new head row, which is what re-materializes the file on
    /// the deleting device — explained by the review row instead of
    /// resurrecting as an anonymous fresh create. The row carries ONE
    /// candidate (the survivor), so the review surface is informational for
    /// this class: there is no non-winning candidate to re-point to, and the
    /// shared `use_other_version` declines the gesture gracefully. A user who
    /// still wants the file gone deletes it again — the same action,
    /// available everywhere.
    ///
    /// Best-effort at every rung, and the bytes are kept regardless: an
    /// upload or report failure degrades to the local conflict record + an
    /// unresolved report (the same ladder `auto_resolve_conflict`
    /// uses), and reconcile's fresh-create upload remains the propagation.
    async fn report_declined_delete(
        &self,
        relative_path: &str,
        full_path: &std::path::Path,
        deleting_device_hex: Option<&str>,
    ) {
        let details = Some(format!(
            "auto-resolved: declined a remote delete{}; kept this device's copy",
            deleting_device_hex
                .map(|d| format!(" from device {d}"))
                .unwrap_or_default(),
        ));
        let Some(folder) = self.folder.clone() else {
            // No folder ⇒ nowhere to report; surface locally.
            let _ = self
                .db
                .record_conflict(relative_path, "delete_declined", details.as_deref());
            return;
        };
        let local_data = match tokio::fs::read(full_path).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    error = %e,
                    "declined delete: cannot read the surviving file; recording the conflict locally"
                );
                let _ =
                    self.db
                        .record_conflict(relative_path, "delete_declined", details.as_deref());
                return;
            }
        };
        // The surviving version must be durably uploaded before the report
        // names it the winner — it exists nowhere but this disk.
        let uploaded = match self
            .upload_chunked_bytes(&local_data, relative_path, false)
            .await
        {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    error = %e,
                    "declined delete: uploading the surviving version failed — recording the conflict locally"
                );
                let _ =
                    self.db
                        .record_conflict(relative_path, "delete_declined", details.as_deref());
                let _ = self
                    .report_conflict_ws(
                        &folder,
                        relative_path,
                        "delete_declined",
                        details,
                        Vec::new(),
                        None,
                    )
                    .await;
                return;
            }
        };
        let manifest_hex = hex::encode(uploaded.manifest_hash.digest());
        let candidates = vec![ConflictCandidate {
            manifest_hash: manifest_hex.clone(),
            device_id: hex::encode(self.device_id),
            size_bytes: local_data.len() as i64,
            created_at: now_unix_secs(),
            content_key_version: uploaded.content_key_version,
            ..Default::default()
        }];
        let report = ReportResolution {
            kind: "latest_wins",
            winning_manifest_hex: manifest_hex,
            winning_size_bytes: None,
            winning_content_key_version: None,
            // A declined delete keeps LOCAL content — no incoming row was
            // resolved into it; causality honestly unknown.
            winning_derived_through: None,
            // Pre-ruling stamp on purpose: the surviving content's anchor is
            // this DISK (which refuses the delete), not the winner row — the
            // delete-vs-edit flow's own machinery owns its convergence.
            winning_carries_novelty: None,
            losing_derived_through: None,
        };
        match self
            .report_conflict_ws(
                &folder,
                relative_path,
                "delete_declined",
                details.clone(),
                candidates.clone(),
                Some(report),
            )
            .await
        {
            Ok(_) => {
                // The report's propagate wrote the winner as the nest head.
                // Mirror the `ResolvedApply::KeepLocal` apply: re-point the row
                // at the uploaded manifest with the survivor as its local
                // identity and refresh the merge base, so the echo of the
                // propagated head reads as current instead of queueing the
                // survivor for dehydration as a "stale hydrated copy".
                let local_hash = ContentHash::of_raw(&local_data);
                self.save_base(relative_path, &local_data);
                let mtime = std::fs::metadata(full_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(fauna_core::data::Timestamp::secs_or_zero)
                    .unwrap_or(0);
                let committed = self
                    .db
                    .upsert_entry(
                        relative_path,
                        Some(local_hash),
                        Some(local_hash),
                        Some(uploaded.manifest_hash),
                        SyncState::Synced,
                        mtime,
                        mtime,
                        local_data.len() as i64,
                        1,
                        uploaded.content_key_version,
                    )
                    .and_then(|_| {
                        self.db.stamp_recorded_content_from_local(
                            relative_path,
                            crate::db::ProofOrigin::OwnRecord,
                        )
                    });
                if let Err(e) = committed {
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(relative_path),
                        error = %e,
                        "declined delete: committing the propagated head to the row failed"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    error = %e,
                    "declined delete: resolved report failed — recording the conflict locally"
                );
                let _ =
                    self.db
                        .record_conflict(relative_path, "delete_declined", details.as_deref());
                let _ = self
                    .report_conflict_ws(
                        &folder,
                        relative_path,
                        "delete_declined",
                        details,
                        candidates,
                        None,
                    )
                    .await;
            }
        }
    }

    /// The hex device id that recorded `relative_path`'s nest head
    /// (`head_manifest`), read off `fauna.files.versions.list` — the one
    /// projection that carries a version's recording device — **judged**: the
    /// id is signed into the winner row this device then signs and decides
    /// which device reads the head as its own echo, so it is taken only from
    /// a version this engine's own reader verified
    /// (`writer-signed-change-records.md` ruling (10)), never off the nest's
    /// word. `None` when it cannot be learned (a foreign set, whose history is
    /// not relayed; a read that fails; no VERIFIED listed version carrying
    /// that manifest): the resolved report then cannot be signed if the
    /// incoming side wins, and the resolve degrades to its unresolved
    /// fallback — the local copy kept, never lost.
    async fn head_recording_device(
        &self,
        relative_path: &str,
        head_manifest: &ContentHash,
    ) -> Option<String> {
        if self.foreign_routing().is_some() {
            return None;
        }
        let reply = fauna_client_sync::SyncClient::new(self.nest_client.clone())
            .versions_list(
                fauna_core::sync::path_hash(relative_path),
                self.folder.clone(),
            )
            .await
            .map_err(|e| {
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(relative_path),
                    "conflict: the head's recording device could not be read: {e}"
                )
            })
            .ok()?;
        let want = head_manifest.digest();
        let want_path = fauna_core::sync::path_hash(relative_path);
        // The latest version naming the head, kept only if its row verifies
        // (a device-less row carries no statement and is no answer).
        let (rows, devices): (Vec<_>, Vec<_>) = reply
            .versions
            .iter()
            .rev()
            .filter(|v| {
                v.manifest_hash.as_ref() == &want[..] && v.path_hash.as_ref() == &want_path[..]
            })
            .filter_map(|v| Some((v.as_change_row()?, v.device_id.clone()?)))
            .unzip();
        let verdicts = self.judge_served_rows(&rows, &reply.signer_certs);
        devices
            .into_iter()
            .zip(verdicts)
            .find(|(_, verdict)| {
                matches!(
                    verdict,
                    fauna_protocol::sync_row_verify::RowVerdict::Verified { .. }
                )
            })
            .map(|(d, _)| d)
            .filter(|d| d.len() == 32)
            .map(|d| hex::encode(&d[..]))
    }

    /// The File Provider ingest path when the OS's `baseVersion` no longer matches
    /// the row's current head — a concurrent writer advanced the nest head while
    /// the OS edited an older version
    /// ([`crate::provider_face::serve_ingest_with_base`] detected the mismatch).
    /// Routes the staged local write through the **same** shared auto-resolve the
    /// download path uses ([`Self::auto_resolve_conflict`]), in the File Provider
    /// direction: `local` = the staged bytes the OS just wrote, `incoming` = the
    /// current nest head. The loser is always uploaded + reported, so this is
    /// non-destructive by construction (`file-sync.md` § Conflicts — version
    /// retention makes auto-resolution risk-free).
    ///
    /// The row is left reflecting the resolved head so the extension's returned
    /// item re-points the OS at it:
    /// - **local wins** — the OS already holds these bytes; stamp the row as a
    ///   hydrated head (`recorded_content_hash` = the local content), so no
    ///   re-fetch. Mirrors the download path's `KeepLocal` apply.
    /// - **merge / incoming wins** — the head is content the OS does not hold, so
    ///   re-point the row at the winning manifest as an un-hydrated placeholder
    ///   (proof cleared); its `contentVersion` then differs from the OS's local
    ///   bytes and the OS re-fetches the winner on the next `fetchContents`.
    /// - **unresolved** (report could not land) — `recorded = false`, so the OS is
    ///   not acked and keeps the local copy pending for retry (the offline queue).
    pub(crate) async fn ingest_conflicting(&self, relative_path: &str) -> Result<UploadOutcome> {
        let full_path = self.watch_dir.join(relative_path);
        let Ok(local_data) = tokio::fs::read(&full_path).await else {
            // No staged bytes to ingest (should not happen for a modify carrying
            // contents); fall back to the plain path, which no-ops / skips.
            return self.upload_file(relative_path).await;
        };
        let local_mtime_ms = std::fs::metadata(&full_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(fauna_core::data::Timestamp::millis_or_zero)
            .unwrap_or(0);

        // The current nest head is the "incoming" side of the resolve. Without a
        // row — or without a recorded head manifest — there is nothing to conflict
        // against, so this is a plain (re-)create.
        let Some(entry) = self.db.get_entry(relative_path)? else {
            return self.upload_file(relative_path).await;
        };
        let Some(head_manifest) = entry.manifest_hash else {
            return self.upload_file(relative_path).await;
        };
        let incoming_bytes = self.download_file_bytes(relative_path).await?;
        let head_device = self
            .head_recording_device(relative_path, &head_manifest)
            .await;
        let incoming = IncomingVersion {
            manifest_hash: head_manifest,
            size_bytes: entry.size_bytes,
            // The row does not carry the head's recording device, so it is read
            // back off the version history: a winning incoming candidate's
            // device is a field of the winner statement the reporter signs
            // (writer-signed change records, ruling (1)(ii)) — an empty one
            // cannot be signed, and an unsigned resolved report is refused.
            device_id: head_device,
            // `remote_mtime` is Unix **seconds** (the head's `changes.list` time);
            // the latest-writer clock is milliseconds, so scale up.
            created_at_ms: entry.remote_mtime.saturating_mul(1_000),
            content_key_version: entry.content_key_version,
        };

        match self
            .auto_resolve_conflict(
                relative_path,
                &local_data,
                local_mtime_ms,
                &incoming_bytes,
                &incoming,
                // This rail resolves against the HEAD projection, which does
                // not surface the head row's seq — causality honestly unknown
                // (pre-ruling ancestor ladder), and with no seq there is no
                // claim to widen (the gap flag is vacuous).
                None,
                None,
                false,
            )
            .await
        {
            ResolvedApply::KeepLocal {
                manifest_hash,
                content_key_version,
            } => {
                // The OS's local bytes are the winner and stay on disk: stamp the
                // row as a hydrated head (mirrors the download path's KeepLocal).
                let local_hash = ContentHash::of_raw(&local_data);
                let mtime = local_mtime_ms / 1_000;
                self.db.upsert_entry(
                    relative_path,
                    Some(local_hash),
                    Some(local_hash),
                    Some(manifest_hash),
                    SyncState::Synced,
                    mtime,
                    mtime,
                    local_data.len() as i64,
                    entry.version_num.max(1),
                    content_key_version,
                )?;
                self.db.stamp_recorded_content_from_local(
                    relative_path,
                    crate::db::ProofOrigin::OwnRecord,
                )?;
                Ok(UploadOutcome {
                    recorded: true,
                    // The OS's own bytes won — nothing to re-fetch.
                    content_changed: false,
                })
            }
            ResolvedApply::WriteMerged {
                merged,
                manifest: _,
                manifest_hash,
                content_key_version,
            } => {
                self.point_row_at_unhydrated_head(
                    relative_path,
                    manifest_hash,
                    merged.len() as i64,
                    content_key_version,
                    entry.version_num,
                )?;
                Ok(UploadOutcome {
                    recorded: true,
                    // The head is merged content the OS does not hold: the
                    // caller must tell the OS to fetch it (`shouldFetchContent`)
                    // — otherwise the OS associates its loser bytes with the
                    // winning version and the next edit fast-forwards over the
                    // resolved head.
                    content_changed: true,
                })
            }
            ResolvedApply::ApplyIncoming => {
                self.point_row_at_unhydrated_head(
                    relative_path,
                    incoming.manifest_hash,
                    incoming.size_bytes,
                    incoming.content_key_version,
                    entry.version_num,
                )?;
                Ok(UploadOutcome {
                    recorded: true,
                    // The incoming head won — same re-fetch obligation as the
                    // merged arm.
                    content_changed: true,
                })
            }
            ResolvedApply::Unresolved => Ok(UploadOutcome::default()),
        }
    }

    /// Re-point `relative_path` at `manifest_hash` as an un-hydrated placeholder
    /// head, clearing the dehydration proof so
    /// [`crate::provider_face::content_version`] falls back to the manifest — the
    /// changed `contentVersion` is what drives the OS to re-fetch the winning
    /// content it does not yet hold (File Provider conflict, merge / incoming
    /// winner).
    fn point_row_at_unhydrated_head(
        &self,
        relative_path: &str,
        manifest_hash: ContentHash,
        size_bytes: i64,
        content_key_version: Option<u64>,
        version_num: i64,
    ) -> Result<()> {
        self.db.upsert_entry(
            relative_path,
            None, // local_hash: the winning bytes are not on this device
            None, // remote_hash
            Some(manifest_hash),
            SyncState::Placeholder,
            0, // local_mtime
            now_unix_secs(),
            size_bytes,
            version_num.max(1),
            content_key_version,
        )?;
        self.db.clear_recorded_content_hash(relative_path)
    }
}

/// The production [`FileHydrator`](crate::hydrator::FileHydrator): the Windows
/// cfapi FETCH_DATA bridge holds an `Arc<dyn FileHydrator>` and calls
/// `download_file_bytes` (path in → bytes out) without naming the concrete
/// engine type. Delegates to the inherent [`SyncEngine::download_file_bytes`],
/// which resolves the manifest from the SyncDb and downloads + decrypts +
/// reassembles in memory.
#[async_trait::async_trait(?Send)]
impl crate::hydrator::FileHydrator for SyncEngine {
    async fn download_file_bytes(&self, relative_path: &str) -> Result<Vec<u8>> {
        SyncEngine::download_file_bytes(self, relative_path).await
    }
}

/// The production [`PlaceholderLister`](crate::enumerate::PlaceholderLister): an
/// on-demand file provider's directory-enumeration callback lists every tracked
/// placeholder (`SyncState::Placeholder`) row from the SyncDb, then computes the
/// browsed directory's immediate children via
/// [`immediate_children`](crate::enumerate::immediate_children). Only
/// `Placeholder` rows are listed — `Synced` entries are real files on disk that
/// the OS enumerates itself, so re-creating placeholders for them would clash. (A
/// 0-byte file stays a `Placeholder` row too — see
/// [`Self::record_placeholders_from_changes`] — so it is enumerated here like any
/// other; its overlay reads `Synced` via [`SyncState::effective_for_size`].)
#[async_trait::async_trait(?Send)]
impl crate::enumerate::PlaceholderLister for SyncEngine {
    async fn list_placeholder_rows(&self) -> Result<Vec<crate::enumerate::PlaceholderRow>> {
        let rows = self
            .db
            .list_by_state(crate::db::SyncState::Placeholder)?
            .into_iter()
            .map(|e| crate::enumerate::PlaceholderRow {
                rel: e.path,
                size: e.size_bytes as u64,
                mtime: e.remote_mtime,
            })
            .collect();
        Ok(rows)
    }
}

/// The replicated-File-Provider engine seam (macOS/iOS on-demand surface).
///
/// Unlike [`PlaceholderLister`](crate::enumerate::PlaceholderLister) — Windows'
/// *placeholder-only* lister, because cfapi lets the OS enumerate materialized
/// files from real disk — `provider_rows` returns **every live row** (placeholder
/// *and* materialized), because a replicated File Provider owns the whole tree and
/// the OS reads nothing itself. Each row carries its `contentVersion`
/// ([`crate::provider_face::content_version`]). The other five methods are thin
/// delegations to the same inherent primitives the Windows host drives (priority
/// #2/#4 — lift, don't reimplement); the mapping semantics live in
/// [`crate::provider_face`], tier-1 tested there.
#[async_trait::async_trait(?Send)]
impl crate::provider_face::ProviderEngine for SyncEngine {
    async fn provider_rows(&self) -> Result<Vec<crate::provider_face::ProviderRow>> {
        let rows = self
            .db
            .list_all()?
            .into_iter()
            // Tombstones are not enumerated — the honest inverse of the delete path —
            // and neither is a delete the user made here that the nest has yet to
            // ack (`LocallyDeleted`).
            .filter(|e| {
                !matches!(
                    e.state,
                    crate::db::SyncState::Deleted | crate::db::SyncState::LocallyDeleted
                )
            })
            .map(|e| crate::provider_face::ProviderRow {
                rel: e.path,
                size: e.size_bytes as u64,
                mtime: e.remote_mtime,
                content_version: crate::provider_face::content_version(
                    e.recorded_content_hash,
                    e.manifest_hash,
                ),
            })
            .collect();
        Ok(rows)
    }

    async fn fetch_bytes(&self, rel: &str) -> Result<Vec<u8>> {
        self.download_file_bytes(rel).await
    }

    async fn fetch_to_path(&self, rel: &str, dest: &Path) -> Result<ContentHash> {
        self.download_file_to_path(rel, dest).await
    }

    fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> Result<()> {
        // Inherent method (resolved by inherent-priority, not this trait method).
        SyncEngine::mark_hydrated(self, rel, content_hash)
    }

    fn mark_placeholder(&self, rel: &str) -> Result<()> {
        SyncEngine::mark_placeholder(self, rel)
    }

    fn hydrated_rels(&self) -> Result<Vec<String>> {
        Ok(self
            .db
            .list_by_state(crate::db::SyncState::Synced)?
            .into_iter()
            .map(|e| e.path)
            .collect())
    }

    fn row_presence(&self, rel: &str) -> Result<crate::provider_face::RowPresence> {
        use crate::provider_face::RowPresence;
        Ok(match self.db.get_entry(rel)?.map(|e| e.state) {
            None | Some(SyncState::Deleted | SyncState::LocallyDeleted) => RowPresence::Absent,
            Some(SyncState::Placeholder) => RowPresence::Placeholder,
            Some(SyncState::Synced) => RowPresence::Hydrated,
            Some(_) => RowPresence::Other,
        })
    }

    fn is_dehydration_safe(&self, rel: &str) -> bool {
        SyncEngine::is_dehydration_safe(self, rel)
    }

    fn holds_provisional_peer_body(&self, rel: &str) -> Result<bool> {
        SyncEngine::holds_provisional_peer_body(self, rel)
    }

    fn is_ignored(&self, rel: &str) -> bool {
        SyncEngine::is_ignored(self, rel)
    }

    async fn ingest(&self, rel: &str) -> Result<UploadOutcome> {
        self.upload_file(rel).await
    }

    async fn ingest_conflicting(&self, rel: &str) -> Result<UploadOutcome> {
        SyncEngine::ingest_conflicting(self, rel).await
    }

    async fn delete(&self, rel: &str) -> Result<UploadOutcome> {
        self.handle_delete(rel).await
    }

    fn read_only(&self) -> bool {
        self.is_read_only()
    }
}

/// Outcome of [`SyncEngine::apply_held_deletes`] — the explicit user-confirmed
/// propagation of a mass-delete-floor hold. The app renders the three shapes:
/// applied-in-full (`applied > 0, remaining_held == 0`), partial-with-retry
/// (`remaining_held > 0` — the record path failed for some rows; re-invoke to
/// resume), and nothing-was-held (`floor_was_active == false` — the files came
/// back, or a partial state ordinary reconcile owns; the surface should clear).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedHeldDeletes {
    /// Rows whose delete record reached the nest (or was vacuously complete).
    pub applied: u64,
    /// Rows still held after this apply — each survives `Synced` for retry.
    pub remaining_held: u64,
    /// Whether the floor was actually holding a set when the verb ran. `false`
    /// means the verb applied nothing *by design* (re-derive-now rule).
    pub floor_was_active: bool,
}

/// Statistics returned by [`SyncEngine::reconcile`].
#[derive(Debug, Default)]
pub struct ReconcileStats {
    pub new_files: usize,
    pub modified_files: usize,
    pub unchanged_files: usize,
    pub deleted_files: usize,
    /// Cloud-only placeholders seen and deliberately left alone ([`crate::placeholder`]).
    ///
    /// Counted rather than merely skipped so the skip is *auditable*: an on-demand root whose
    /// files are all still on the nest should report them here, and a file that silently
    /// appears in neither this count nor any other is exactly the kind of gap that lets a
    /// placeholder slip into the hash or delete path.
    pub placeholder_files: usize,
    /// Deletes the **mass-delete floor** held this pass (file-sync.md § Files
    /// Appear Automatically, ratified 2026-08-02): every synced row of a
    /// multi-folder was missing from the scan at once — the wholesale-vanish
    /// shape (unmounted volume, dir removed under the engine) — so NOTHING was
    /// recorded. Derived per pass, never stored: reappearing files clear it by
    /// construction. Non-zero is the signal the status surface reports so the
    /// user can reconnect the folder (or explicitly apply the deletions —
    /// the captured follow-on affordance).
    pub deletes_held: usize,
    /// `Synced` rows this pass refused to even CONSIDER for deletion because
    /// the scan could not enumerate where they live — a
    /// subdirectory turned mode-0, a failing disk, an `ESTALE` network or FUSE
    /// mount, or the watch root itself gone unreadable.
    ///
    /// Distinct from [`Self::deletes_held`], and the distinction is the
    /// diagnosis: a *hold* means the pass looked and found the folder empty
    /// (reconnect it, or confirm the deletions); this means the pass could not
    /// look at all (fix the permissions or the mount — there is nothing to
    /// confirm, and no count a user should ever be offered). Derived per pass,
    /// never stored: the rows resume ordinary sync the moment the path reads.
    pub deletes_skipped_unreadable: usize,
}

/// Wire size of a `fauna.web.files.prune_sealed` declaration's path list — the
/// number [`SyncEngine::declare_live_web_corpus`] weighs against
/// [`fauna_protocol::web::MAX_PRUNE_SEALED_PATH_BYTES`].
///
/// Deliberately an **over**-estimate: each path costs its own bytes plus a
/// fixed allowance for its CBOR header, and the envelope's own fields ride the
/// constant's headroom. Erring high is the safe direction — the penalty for
/// over-estimating is that one improbably large site keeps the pre-fix
/// residual, and the penalty for under-estimating is a refused frame.
fn web_declaration_bytes(paths: &[String]) -> usize {
    paths.iter().map(|p| p.len() + 8).sum()
}

/// One retained own row's field map onto the wire shape — shared by the
/// sequenced and pending serves, which differ only in the coordinate (`seq`
/// vs the pending `0`), and by the engine's signing funnel, which signs
/// exactly this shape (`SyncEngine::sign_own_row`) so the row verifies as it
/// travels. Lives here, ungated, because the signing funnel runs on every
/// retained row whether or not the share leg (`p2p-share`) is compiled in.
/// Public so a fixture signs a retained row exactly as served.
#[doc(hidden)]
pub fn wire_change(r: &crate::db::OwnChangeRow, seq: i64) -> fauna_protocol::sync::SyncChange {
    fauna_protocol::sync::SyncChange {
        seq,
        path_hash: r.path_hash.clone(),
        manifest_hash: r.manifest_hash.clone(),
        size_bytes: r.size_bytes,
        change_type: r.change_type.clone(),
        created_at: r.created_at,
        path: Some(r.path.clone()),
        device_id: Some(r.device_id.clone()),
        content_key_version: r.content_key_version,
        thumbnail_hash: r.thumbnail_hash.clone(),
        // The writer: this replica, by construction — the signed actor on a
        // signed row, the channel-proven serving peer's own on an unsigned one.
        author_actor_id: Some(r.author_actor_id.clone()),
        path_sealed: r.path_sealed.clone().map(fauna_protocol::ByteBuf::from),
        derived_through: r.derived_through,
        is_resolution: r.is_resolution,
        signature: r.signature.clone().map(fauna_protocol::ByteBuf::from),
        signer_key: r.signer_key.clone().map(fauna_protocol::ByteBuf::from),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::web_declaration_bytes;

    /// the declaration either fits one frame or is not sent.
    ///
    /// The failure mode this guards is specific and severe: the request says
    /// *"these are ALL the live paths"*, so a set truncated to fit would tell
    /// the nest to delete every page it could not name. There is no partial
    /// declaration — an oversized corpus keeps the stale-row residual instead,
    /// which is the pre-fix behaviour and strictly the lesser harm.
    #[test]
    fn an_ordinary_site_fits_the_declaration_and_an_absurd_one_does_not() {
        let ordinary: Vec<String> = (0..2_000)
            .map(|i| format!("blog/{i:04}/index.html"))
            .collect();
        assert!(
            web_declaration_bytes(&ordinary) <= fauna_protocol::web::MAX_PRUNE_SEALED_PATH_BYTES,
            "a 2000-page site must still be declarable — otherwise the fix does \
             not reach the sites it is for"
        );

        let absurd: Vec<String> = (0..200_000)
            .map(|i| format!("deep/nested/tree/{i:06}/page.html"))
            .collect();
        assert!(
            web_declaration_bytes(&absurd) > fauna_protocol::web::MAX_PRUNE_SEALED_PATH_BYTES,
            "and the guard must actually trip somewhere below the frame cap"
        );

        // The estimate is an over-estimate of the paths' own bytes, never an
        // under-estimate — that direction is what keeps a fitting declaration
        // from being refused on the wire.
        let one = vec!["index.html".to_string()];
        assert!(web_declaration_bytes(&one) > "index.html".len());
    }

    use super::*;

    /// A `SyncEngine` wired to `control` and bound to folder `"my_site"` — the
    /// minimum `declare_live_web_corpus` touches (`self.folder` + `self.control`;
    /// it reaches neither `self.client` nor `self.nest_client`), so every other
    /// field is a harmless placeholder and the URL is never dialed.
    fn test_engine_for_declare(control: crate::nest_api::FakeSyncControl) -> SyncEngine {
        let kp = fauna_core::identity::ActorKeypair::generate();
        let bearer: Arc<dyn fauna_nest_http::BearerSource> =
            Arc::new(fauna_nest_http::StaticBearer("test.bearer".to_string()));
        let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
            "http://127.0.0.1:1".to_string(),
            kp,
            bearer,
            reqwest::Client::new(),
        ));
        let device_id = [0x11u8; 32];
        let client = SyncClient::new(auth, &device_id);
        let db = SyncDb::open_in_memory().unwrap();
        let nest_client = fauna_client::NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        );
        let transfer_pool = crate::transfer::TransferPool::new(
            Arc::new(crate::adaptive::AdaptiveConcurrency::fixed(4)),
            None,
        );
        let engine = SyncEngine::new(
            PathBuf::from("/tmp/nonexistent"),
            db,
            client,
            Some("my_site".to_string()),
            device_id,
            None, // mls
            None, // epoch_secret
            None, // backup_key
            None, // mls_group_id
            None, // content_keys
            fauna_core::format::ConflictPolicy::default(),
            fauna_core::format::FormatRegistry::new(),
            crate::ignore::IgnoreMatcher::default(),
            4,
            transfer_pool,
            nest_client,
            crate::config::SyncMode::Sync,
        );
        engine.set_control_api(Arc::new(control));
        engine
    }

    /// the oversize guard has a witness beyond the pure-function
    /// arithmetic: a corpus over the frame cap must not reach the wire at all.
    ///
    /// Red-verify: replace `budget > fauna_protocol::web::MAX_PRUNE_SEALED_PATH_BYTES`
    /// in `declare_live_web_corpus` with `budget > usize::MAX` (a condition that
    /// can never hold) — this test fails because the fake records the oversized
    /// declaration instead of nothing.
    #[tokio::test]
    async fn an_oversized_corpus_declares_nothing() {
        let control = crate::nest_api::FakeSyncControl::accepting();
        let engine = test_engine_for_declare(control.clone());

        let absurd: Vec<String> = (0..200_000)
            .map(|i| format!("deep/nested/tree/{i:06}/page.html"))
            .collect();
        assert!(
            web_declaration_bytes(&absurd) > fauna_protocol::web::MAX_PRUNE_SEALED_PATH_BYTES,
            "the fixture must actually be over the cap, or this test proves nothing"
        );

        engine.declare_live_web_corpus(absurd).await;

        assert!(
            control.prune_declarations().is_empty(),
            "an over-cap corpus must never reach the wire — a truncated set would \
             tell the nest to delete live content it was never shown"
        );
    }

    /// The twin of the above: an ordinary, under-cap corpus IS declared, with
    /// exactly the paths handed in — proving the guard's absence (not a wiring
    /// bug elsewhere) is what the sibling test's silence demonstrates.
    #[tokio::test]
    async fn an_ordinary_corpus_declares_every_path() {
        let control = crate::nest_api::FakeSyncControl::accepting();
        let engine = test_engine_for_declare(control.clone());

        let ordinary: Vec<String> = (0..2_000)
            .map(|i| format!("blog/{i:04}/index.html"))
            .collect();
        assert!(
            web_declaration_bytes(&ordinary) <= fauna_protocol::web::MAX_PRUNE_SEALED_PATH_BYTES,
            "the fixture must actually fit the cap, or this test proves nothing"
        );

        engine.declare_live_web_corpus(ordinary.clone()).await;

        let sent = control.prune_declarations();
        assert_eq!(sent.len(), 1, "exactly one declaration, sent");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &sent[0], "my_site"
        ));
        assert_eq!(sent[0].paths, ordinary);
    }

    /// One row on the changes feed, named by who wrote it.
    fn row(
        seq: i64,
        path: &str,
        device: &str,
        author: Option<&str>,
    ) -> fauna_protocol::sync::SyncChange {
        fauna_protocol::sync::SyncChange {
            seq,
            path: Some(path.to_string()),
            device_id: Some(device.to_string()),
            author_actor_id: author.map(str::to_string),
            change_type: "update".to_string(),
            ..Default::default()
        }
    }

    const OUR_DEVICE: &str = "0123456789abcdef";
    const OUR_ACTOR: &str = "aa";
    const PEER_ACTOR: &str = "bb";

    #[test]
    fn a_peer_sharing_our_device_id_is_not_our_own_row() {
        // The row 306 shape (measured 2026-08-19): two actors in one shared set
        // carrying the SAME device id. `sync_devices` is keyed (actor, device), so
        // this is legal — and reading it as a self-echo loses the peer's file
        // permanently, because the anchor advances past a row that applied nothing.
        let peer = row(13, "shared.txt", OUR_DEVICE, Some(PEER_ACTOR));
        assert!(
            !SyncEngine::row_is_own(&peer, OUR_DEVICE, OUR_ACTOR),
            "a peer's row must stay a PEER row even when the device ids collide"
        );

        let ours = row(13, "shared.txt", OUR_DEVICE, Some(OUR_ACTOR));
        assert!(SyncEngine::row_is_own(&ours, OUR_DEVICE, OUR_ACTOR));
    }

    #[test]
    fn a_different_device_is_never_our_own_row() {
        let other = row(13, "shared.txt", "fedcba9876543210", Some(OUR_ACTOR));
        assert!(!SyncEngine::row_is_own(&other, OUR_DEVICE, OUR_ACTOR));
    }

    #[test]
    fn an_absent_author_keeps_the_device_only_answer() {
        // The nest strips `author_actor_id` on the public plane, so absence means
        // "unknown", not "mismatch" — widening it to a mismatch there would turn
        // every one of this seat's own echoes back into peer rows.
        let unattributed = row(13, "shared.txt", OUR_DEVICE, None);
        assert!(SyncEngine::row_is_own(&unattributed, OUR_DEVICE, OUR_ACTOR));

        let other_device = row(13, "shared.txt", "fedcba9876543210", None);
        assert!(!SyncEngine::row_is_own(
            &other_device,
            OUR_DEVICE,
            OUR_ACTOR
        ));
    }

    #[test]
    fn a_colliding_peer_create_still_supersedes() {
        // The same collision one layer up: a peer create/modify must keep its place
        // in the superseding fold. Read as a self content echo it is dropped, and
        // then nothing downstream ever downloads it.
        let changes = vec![row(13, "shared.txt", OUR_DEVICE, Some(PEER_ACTOR))];
        let superseding =
            SyncEngine::content_superseding_seq_by_path(&changes, OUR_DEVICE, OUR_ACTOR);
        assert_eq!(
            superseding.get("shared.txt"),
            Some(&13),
            "a peer create sharing our device id must still supersede"
        );

        let own = vec![row(13, "shared.txt", OUR_DEVICE, Some(OUR_ACTOR))];
        let superseding_own =
            SyncEngine::content_superseding_seq_by_path(&own, OUR_DEVICE, OUR_ACTOR);
        assert!(
            superseding_own.is_empty(),
            "our own create echo applies no content, so it supersedes nothing"
        );
    }

    #[test]
    fn build_conflict_candidates_local_then_incoming() {
        let incoming = IncomingVersion {
            manifest_hash: ContentHash::from_digest_raw([0xbb; 32]),
            size_bytes: 222,
            device_id: Some("dd".repeat(32)),
            created_at_ms: 1_700_000_000_000,
            content_key_version: Some(2),
        };
        let candidates = build_conflict_candidates(
            "aa".repeat(32),
            111,
            Some(5),
            "cc".repeat(32),
            &incoming,
            1_700_000_000,
        );

        // Order is [local, incoming] — the resolve handler attributes the
        // propagated change to the winning candidate's device, so the slot
        // ordering must be stable.
        assert_eq!(candidates.len(), 2);

        let local = &candidates[0];
        assert_eq!(local.manifest_hash, "aa".repeat(32));
        assert_eq!(local.device_id, "cc".repeat(32));
        assert_eq!(local.size_bytes, 111);
        assert_eq!(local.created_at, 1_700_000_000);
        assert_eq!(local.content_key_version, Some(5));

        let inc = &candidates[1];
        // hex of [0xbb; 32]
        assert_eq!(inc.manifest_hash, "bb".repeat(32));
        assert_eq!(inc.device_id, "dd".repeat(32));
        assert_eq!(inc.size_bytes, 222);
        assert_eq!(inc.created_at, 1_700_000_000);
        assert_eq!(
            inc.content_key_version,
            Some(2),
            "incoming candidate echoes its change's sealed generation"
        );
    }

    #[test]
    fn build_conflict_candidates_unknown_incoming_device_is_empty_string() {
        let incoming = IncomingVersion {
            manifest_hash: ContentHash::from_digest_raw([0x11; 32]),
            size_bytes: 0,
            device_id: None,
            created_at_ms: 0,
            content_key_version: None,
        };
        let candidates =
            build_conflict_candidates("00".repeat(32), 0, None, "ff".repeat(32), &incoming, 42);
        // A missing incoming device id degrades to an empty hex string rather
        // than panicking — the candidate is still listable/choosable.
        assert_eq!(candidates[1].device_id, "");
    }
}
