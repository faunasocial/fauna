//! Replicated-file-provider serving face — the callback→engine primitives a
//! macOS/iOS `NSFileProviderReplicatedExtension` calls.
//!
//! `docs/goal/behavior/file-sync.md` § On-Demand Files → Apple File Provider
//! binding (ratified 2026-07-18) owns the callback→engine table + milestone chain.
//! This is **M1**: the shared,
//! platform-agnostic, tier-1-tested provider face; the appex consumes it in M2
//! (read) / M3 (write). No Swift here.
//!
//! ## Why this is not the Windows hydration loop
//!
//! The Windows cfapi host owns a real on-disk directory, watches it, and answers
//! streaming cfapi callbacks through [`crate::always_resident::LocalWrites`] +
//! `run_hydration_loop` (a `select!` over a watcher, rescan/reconnect ticks, and a
//! chunked `TransferSink`). A **replicated** File Provider is *control-inverted*
//! (`file-sync.md` § Apple File Provider binding, first bullet): the OS owns the
//! on-disk tree under `~/Library/CloudStorage/` and **calls** the extension —
//! there is no watcher and no disk scan on this surface, and `fetchContents` hands
//! back a whole materialized file, not chunked sink pushes. So the face is a set
//! of discrete request/response primitives, not a driving loop; it reuses the same
//! **shared engine primitives** the Windows host does (priority #2/#4 — lift, don't
//! reimplement) rather than the Windows *loop*.
//!
//! ## The six primitives (plan doc § Architecture table)
//!
//! | FP callback | Engine call |
//! |---|---|
//! | enumerate container | [`serve_enumerate`] — [`ProviderEngine::provider_rows`] folded by [`enumerate_children`]; item `contentVersion` = `recorded_content_hash` (`or` the manifest head) |
//! | `item(for:)` | [`serve_item`] — one row (or a synthesized directory) by identifier |
//! | `fetchContents` | [`serve_fetch_to_path`] — bounded-memory `download_file_to_path` + `mark_hydrated` (preferred; [`serve_fetch`] remains the whole-buffer variant for small reads) |
//! | `createItem` / `modifyItem` | [`serve_ingest`] — sealed per-file `upload_file`; ack **only on `UploadOutcome::recorded`** |
//! | `deleteItem` | [`serve_delete`] — `handle_delete` record-first tombstone; ack **only on `UploadOutcome::recorded`** |
//! | rename / move | [`serve_rename`] — tombstone the old path + ingest the new (there is no dedicated rename record; a rename is a delete+create pair, `engine.rs`) |
//! | eviction observed | [`serve_evict`] — `mark_placeholder` (the honest-row inverse) |
//!
//! A binding whose provider owns its tree (android's SAF `DocumentsProvider` —
//! no OS keeps the replica for it) drives these same cores through
//! [`owned_tree`], which adds what the OS does for apple: the kept and cache
//! roots, hydrate-for-open, prepare-for-write, close → ingest, the start sweep
//! and eviction observation (`on-demand-files.md` § Android SAF
//! DocumentsProvider binding).
//!
//! ## A reader's host is read-only
//!
//! A set shared with the account that it may only read is served by the same
//! cores with **no write half** (`on-demand-files.md` § Shared sets on a
//! capability host, decision 3): the engine says so
//! ([`ProviderEngine::read_only`]), and every write core — ingest, delete,
//! rename, and the owned tree's open-for-write, close, create, delete and
//! rename — refuses with the typed [`ReadOnlyHost`] before it touches the
//! engine or the disk. The platform advertises no write capability on such a
//! set, so the OS never hands it an edit; the refusal is what makes that hold
//! by construction rather than by the caller's good manners.
//!
//! `ProviderEngine` mirrors the Windows `HydrationHost` composite (minus the
//! loop-only methods a control-inverted provider never calls). `SyncEngine` is the
//! production impl; the tier-1 tests fake it. A future session may migrate the
//! Windows serving cores (`serve_fetch`/`serve_populate` in `fauna-sync-agent`)
//! onto this same face — captured as a follow-on, gated on Windows compilation.

use std::path::Path;

use anyhow::Result;

use fauna_core::data::ContentHash;

use crate::engine::UploadOutcome;
use crate::enumerate::{PlaceholderRow, immediate_children};

pub mod owned_tree;

/// One live tracked row for a set — a placeholder *or* a materialized file — as
/// the replicated enumerator needs it. Unlike [`PlaceholderRow`] (Windows'
/// placeholder-only lister) this carries the item's `content_version`, because a
/// replicated File Provider enumerates **the whole tree** (the OS reads nothing
/// from disk itself), and the OS drives re-fetch off a changed `contentVersion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    /// Forward-slash, folder-relative path (the `path_hash` key).
    pub rel: String,
    pub size: u64,
    /// Unix seconds (the recorded head's mtime).
    pub mtime: i64,
    /// The FP `NSFileProviderItemVersion.contentVersion` bytes for this file:
    /// `recorded_content_hash` when the head is hydration-proven, else the
    /// manifest head hash (always present for a live row), else empty. See
    /// [`content_version`].
    pub content_version: Vec<u8>,
}

/// One enumerated item handed back to the OS: a tracked file, or a synthesized
/// directory for an intermediate path segment of a deeper file (directories carry
/// no own row — a folder holds them only implicitly via child paths).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderItem {
    /// Full forward-slash, folder-relative path — the FP item identifier.
    pub rel: String,
    /// Final path component only (the FP `filename`), e.g. `photo.jpg` or `sub`.
    pub name: String,
    /// Byte size; `0` for a directory.
    pub size: u64,
    /// Unix seconds. For a directory: the newest mtime among its descendants.
    pub mtime: i64,
    pub is_dir: bool,
    /// FP `contentVersion` bytes (empty for a directory).
    pub content_version: Vec<u8>,
}

/// The result of [`serve_fetch`] — the materialized bytes plus the item's new
/// `contentVersion` (the served content's hash), so the caller can hand the OS a
/// consistent version alongside the file it just hydrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedContent {
    pub bytes: Vec<u8>,
    pub content_version: Vec<u8>,
}

/// The result of a write primitive ([`serve_ingest`] / [`serve_rename`]): whether
/// the local change reached the nest **and** the row was stamped
/// (`UploadOutcome::recorded`). The FP extension acks the OS **only** when this is
/// `true` — the OS then never discards an un-acked local copy (`file-sync.md`
/// § Apple File Provider binding, *recorded-head gate*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteAck {
    pub acked: bool,
    /// The row moved to a head the OS's on-disk bytes are NOT (a conflicted
    /// ingest resolved to a merge / incoming winner). The extension passes this
    /// as `modifyItem`'s `shouldFetchContent`, so the OS re-fetches the winner
    /// instead of associating its loser bytes with the winning version
    /// ([`UploadOutcome::content_changed`]).
    pub content_changed: bool,
    /// The rel is ignored (dotfile component, built-in default ignore, or
    /// `.faunaignore` pattern — [`ProviderEngine::is_ignored`]) and the write
    /// was deliberately NOT ingested: no engine row, no upload, no change
    /// record. The extension maps this to
    /// `NSFileProviderError.excludedFromSync` — the OS's purpose-built answer:
    /// the file stays on the user's disk, the system stops trying to sync it,
    /// and re-evaluates via a fresh `createItem` when it changes
    /// (`file-sync.md` § Built-in default ignores).
    pub excluded: bool,
}

impl WriteAck {
    /// The ignored-rel refusal: not acked, nothing ingested, `excluded` set.
    pub fn excluded() -> Self {
        Self {
            acked: false,
            content_changed: false,
            excluded: true,
        }
    }
}

/// The typed refusal every write core gives on a read-only host — a set the
/// account holds as a reader (`on-demand-files.md` § Shared sets on a
/// capability host, decision 3). Nothing was sealed, uploaded, recorded or
/// changed on disk. Recover it from a core's error with
/// `err.downcast_ref::<ReadOnlyHost>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this folder is shared with this account read-only; it cannot be changed here")]
pub struct ReadOnlyHost;

/// Refuse a write on a read-only host — the first line of every write core.
pub(crate) fn refuse_read_only<E: ProviderEngine>(engine: &E) -> Result<()> {
    if engine.read_only() {
        return Err(ReadOnlyHost.into());
    }
    Ok(())
}

/// What the engine's row for one rel says about its body — the question a
/// provider that owns its tree ([`owned_tree`]) asks before trusting a body it
/// finds on disk: a cache-root body is the file's only when the row is
/// [`Hydrated`](Self::Hydrated), and a kept-root body over a
/// [`Placeholder`](Self::Placeholder) row is an edit the nest's head moved
/// under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowPresence {
    /// No row, or a tombstone.
    Absent,
    /// A cloud-only row: the recorded head's bytes are not on this device.
    Placeholder,
    /// A `Synced` row: the recorded head's bytes were served to this device.
    Hydrated,
    /// Any other live state (an upload in flight, a local modification, a
    /// conflict) — a live row whose body is some write path's business.
    Other,
}

/// The engine seam a replicated File Provider bridges its callbacks to — the
/// control-inverted analog of the Windows `HydrationHost`. `SyncEngine` is the
/// production impl (lives in `engine.rs` to reach the private DB + inherent
/// methods); the tier-1 tests below fake it.
///
/// `?Send` mirrors [`crate::FileHydrator`] / [`crate::enumerate::PlaceholderLister`]:
/// `SyncEngine` is `Send + !Sync` (its rusqlite `Connection`) and the extension
/// serializes callbacks onto its single driving task.
#[async_trait::async_trait(?Send)]
pub trait ProviderEngine {
    /// Every live (non-tombstone) tracked row for the set, each with its
    /// `contentVersion`. Placeholders *and* materialized files — the replicated
    /// enumerator owns the whole tree.
    async fn provider_rows(&self) -> Result<Vec<ProviderRow>>;

    /// Reassemble + decrypt one file's recorded head (`download_file_bytes`).
    async fn fetch_bytes(&self, rel: &str) -> Result<Vec<u8>>;

    /// Bounded-memory twin of [`Self::fetch_bytes`]: the file's plaintext lands
    /// at `dest` (windowed fetch → open → append, `download_file_to_path`) and
    /// the verified whole-file content hash is returned. The `fetchContents`
    /// default — an iOS appex runs under a hard memory cap, and even on macOS a
    /// large file should not cross the FFI as one `Vec<u8>`. On failure `dest`
    /// is removed (no partial file survives).
    async fn fetch_to_path(&self, rel: &str, dest: &Path) -> Result<ContentHash>;

    /// Record the served content's identity so the dehydration gate is honest
    /// (`mark_hydrated`).
    fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> Result<()>;

    /// Demote a materialized file back to a cloud-only placeholder row, on
    /// observed OS eviction (`mark_placeholder`).
    fn mark_placeholder(&self, rel: &str) -> Result<()>;

    /// Every rel the host believes is materialized on the OS's disk (`Synced`
    /// rows) — the minuend of the eviction-observation diff: a rel here that the
    /// OS's materialized-item enumerator no longer lists was evicted behind the
    /// host's back and must be demoted ([`Self::mark_placeholder`]) so the
    /// dehydration bookkeeping stays honest.
    fn hydrated_rels(&self) -> Result<Vec<String>>;

    /// What the row for `rel` says about its body ([`RowPresence`]).
    fn row_presence(&self, rel: &str) -> Result<RowPresence>;

    /// Is freeing the local body at `rel` provably lossless by the engine's own
    /// record — a `Synced` row whose on-disk bytes hash to exactly the recorded
    /// head's content (`SyncEngine::is_dehydration_safe`)? The owned tree
    /// demotes a kept-root body to the cache root only on this proof.
    fn is_dehydration_safe(&self, rel: &str) -> bool;

    /// Is the body at `rel` one the share plane landed from a peer and the
    /// nest has not confirmed yet (`SyncEngine::holds_provisional_peer_body`)?
    /// Such a body rests in an owned tree's kept root but is NOT a write
    /// intent: it was never this replica's authorship, so the owned tree
    /// never ingests it. `false` for an engine that runs no share plane.
    fn holds_provisional_peer_body(&self, _rel: &str) -> Result<bool> {
        Ok(false)
    }

    /// Is this rel excluded from the set — a dotfile component, a built-in
    /// default ignore, or a `.faunaignore` pattern (`SyncEngine::is_ignored`)?
    /// The write cores consult this BEFORE ingesting: the litter the built-in
    /// ignores exist to filter (`file-sync.md` § Built-in default ignores)
    /// reaches this face as ordinary `createItem`/`modifyItem` callbacks, not
    /// filesystem events, so the watcher-path gate (`is_user_write`) never
    /// sees it.
    fn is_ignored(&self, rel: &str) -> bool;

    /// Seal + upload one local file through the ordinary chunk pipeline +
    /// `changes.record` (`upload_file`). The `recorded` flag is the ack signal.
    async fn ingest(&self, rel: &str) -> Result<UploadOutcome>;

    /// Ingest a local write whose OS `baseVersion` no longer matches the row's
    /// current head — a concurrent writer advanced the nest head while the OS
    /// edited an older version. Routes the staged local write through the engine's
    /// shared conflict auto-resolve (retention-first: the loser is always uploaded
    /// + reported, so it is non-destructive by construction, `file-sync.md`
    /// § Conflicts) instead of last-writer-wins clobbering the newer head. The
    /// default is a plain [`Self::ingest`] — a single-writer surface (or a host
    /// that does not track a merge base) cannot diverge, so it never reaches here.
    async fn ingest_conflicting(&self, rel: &str) -> Result<UploadOutcome> {
        self.ingest(rel).await
    }

    /// Tombstone one path (`handle_delete` → a `manifest_hash = None` change
    /// record). Record-first: `recorded` is the ack signal, exactly like
    /// [`Self::ingest`] — a not-recorded delete leaves the row in place so the
    /// OS's pending change (un-acked) retries it.
    async fn delete(&self, rel: &str) -> Result<UploadOutcome>;

    /// Is this engine a reader's — built over a set the account may only read,
    /// so it has no write half (module doc, *A reader's host is read-only*)?
    /// Every write core refuses [`ReadOnlyHost`] when it is. `false` for every
    /// two-way host, which is the default.
    fn read_only(&self) -> bool {
        false
    }
}

/// Derive the FP `contentVersion` bytes for a row: the hydration-proven
/// `recorded_content_hash` when present, else the manifest head hash (always set
/// for a live head — so an un-hydrated placeholder still gets a stable, superseding
/// version), else empty (defensive — a versionless item forces a re-fetch).
///
/// The goal doc names `recorded_content_hash` as `contentVersion`; the manifest
/// fallback fills the un-hydrated-placeholder gap that would otherwise leave a
/// freshly-populated row versionless (the fold stamps `manifest_hash`, not
/// `recorded_content_hash`, until a hydrate/record proves the head — `engine.rs`
/// `record_placeholders_from_changes`).
pub fn content_version(recorded: Option<ContentHash>, manifest: Option<ContentHash>) -> Vec<u8> {
    recorded
        .or(manifest)
        .map(|h| h.digest().to_vec())
        .unwrap_or_default()
}

/// Fold the flat live-row list into the immediate children of `parent_rel`,
/// carrying each file's `contentVersion`. Reuses the shared, pure
/// [`immediate_children`] directory-synthesis (one fold, priority #4) and joins the
/// version back on by full path; synthesized directories carry an empty version.
///
/// `parent_rel` is forward-slash, folder-relative; `""` is the sync root.
pub fn enumerate_children(rows: &[ProviderRow], parent_rel: &str) -> Vec<ProviderItem> {
    let parent = parent_rel.trim_matches('/');
    let ph_rows: Vec<PlaceholderRow> = rows
        .iter()
        .map(|r| PlaceholderRow {
            rel: r.rel.clone(),
            size: r.size,
            mtime: r.mtime,
        })
        .collect();
    let versions: std::collections::HashMap<&str, &[u8]> = rows
        .iter()
        .map(|r| (r.rel.as_str(), r.content_version.as_slice()))
        .collect();

    immediate_children(&ph_rows, parent)
        .into_iter()
        .map(|c| {
            let rel = if parent.is_empty() {
                c.name.clone()
            } else {
                format!("{parent}/{}", c.name)
            };
            let content_version = if c.is_dir {
                Vec::new()
            } else {
                versions
                    .get(rel.as_str())
                    .map(|v| v.to_vec())
                    .unwrap_or_default()
            };
            ProviderItem {
                rel,
                name: c.name,
                size: c.size,
                mtime: c.mtime,
                is_dir: c.is_dir,
                content_version,
            }
        })
        .collect()
}

/// Resolve one FP item identifier (`rel`) against the live rows: an exact match is
/// a file; a path that is a strict prefix of some row is a directory; anything
/// else is unknown. The root (`""`) is a directory when the set has any rows.
///
/// Pure — the metadata half of the `item(for:)` callback (`serve_item` wraps it
/// with the row read).
pub fn find_item(rows: &[ProviderRow], rel: &str) -> Option<ProviderItem> {
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        return (!rows.is_empty()).then(|| ProviderItem {
            rel: String::new(),
            name: String::new(),
            size: 0,
            mtime: rows.iter().map(|r| r.mtime).max().unwrap_or(0),
            is_dir: true,
            content_version: Vec::new(),
        });
    }
    if let Some(row) = rows.iter().find(|r| r.rel == rel) {
        let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
        return Some(ProviderItem {
            rel: rel.to_string(),
            name,
            size: row.size,
            mtime: row.mtime,
            is_dir: false,
            content_version: row.content_version.clone(),
        });
    }
    let dir_prefix = format!("{rel}/");
    let newest = rows
        .iter()
        .filter(|r| r.rel.starts_with(&dir_prefix))
        .map(|r| r.mtime)
        .max();
    newest.map(|mtime| {
        let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
        ProviderItem {
            rel: rel.to_string(),
            name,
            size: 0,
            mtime,
            is_dir: true,
            content_version: Vec::new(),
        }
    })
}

/// The ack decision, isolated so the invariant is a one-line pure function: the
/// OS may be told a local change landed **only** when the change record reached
/// the nest and stamped the row.
pub fn upload_ack(outcome: &UploadOutcome) -> WriteAck {
    WriteAck {
        acked: outcome.recorded,
        content_changed: outcome.content_changed,
        excluded: false,
    }
}

/// Is this rel excluded from sync ([`ProviderEngine::is_ignored`])? The
/// extension asks up front in `createItem` — before staging bytes, and for the
/// folder shape, which never reaches a write core: an ignored folder is
/// excluded whole, so the OS never sends `createItem` for its children
/// (`.git/`, `~$dir/`).
pub fn serve_is_ignored<E: ProviderEngine>(engine: &E, rel: &str) -> bool {
    engine.is_ignored(rel)
}

// ---- Serving cores (generic over the engine seam) ----

/// enumerate container → the immediate children of `parent_rel`.
pub async fn serve_enumerate<E: ProviderEngine>(
    engine: &E,
    parent_rel: &str,
) -> Result<Vec<ProviderItem>> {
    let rows = engine.provider_rows().await?;
    Ok(enumerate_children(&rows, parent_rel))
}

/// `item(for:)` → the metadata for one identifier (file, directory, or unknown).
pub async fn serve_item<E: ProviderEngine>(engine: &E, rel: &str) -> Result<Option<ProviderItem>> {
    let rows = engine.provider_rows().await?;
    Ok(find_item(&rows, rel))
}

/// `fetchContents` → the materialized bytes plus the item's new `contentVersion`.
/// Fetches the recorded head, records the served content's identity so the row is
/// dehydration-honest, and returns the served hash as the version (identical to the
/// Windows `serve_fetch` hydrate step: `ContentHash::of_raw(&bytes)` →
/// `mark_hydrated`).
pub async fn serve_fetch<E: ProviderEngine>(engine: &E, rel: &str) -> Result<FetchedContent> {
    let bytes = engine.fetch_bytes(rel).await?;
    let hash = ContentHash::of_raw(&bytes);
    engine.mark_hydrated(rel, hash)?;
    Ok(FetchedContent {
        content_version: hash.digest().to_vec(),
        bytes,
    })
}

/// `fetchContents` → the file's plaintext written to `dest` with bounded memory
/// ([`ProviderEngine::fetch_to_path`]); the returned bytes are the item's new
/// `contentVersion` (the verified whole-file hash). Same dehydration-honesty
/// stamp as [`serve_fetch`] — this is the preferred `fetchContents` serving
/// core: the extension hands the OS-provided temp URL and no file ever crosses
/// the FFI as a single buffer (iOS appex memory cap).
pub async fn serve_fetch_to_path<E: ProviderEngine>(
    engine: &E,
    rel: &str,
    dest: &Path,
) -> Result<Vec<u8>> {
    let hash = engine.fetch_to_path(rel, dest).await?;
    engine.mark_hydrated(rel, hash)?;
    Ok(hash.digest().to_vec())
}

/// `createItem` / `modifyItem` → seal + upload the file, acking **only on
/// `UploadOutcome::recorded`**. An ignored rel ([`ProviderEngine::is_ignored`])
/// never reaches the engine: it answers [`WriteAck::excluded`] — Office
/// lock/`.tmp` litter and dotfiles must not become permanent folder content
/// through this door any more than through the watcher's
/// (`file-sync.md` § Built-in default ignores).
pub async fn serve_ingest<E: ProviderEngine>(engine: &E, rel: &str) -> Result<WriteAck> {
    refuse_read_only(engine)?;
    if engine.is_ignored(rel) {
        return Ok(WriteAck::excluded());
    }
    let outcome = engine.ingest(rel).await?;
    Ok(upload_ack(&outcome))
}

/// `modifyItem` carrying the OS's `baseVersion` → seal + upload, but first compare
/// the base `contentVersion` the OS believed it was editing against the row's
/// **current** head. Equal — or an empty base (a create, or an OS that supplied
/// none) — is a plain fast-forward ([`serve_ingest`]): the head has not moved since
/// the OS last read it, so this is an ordinary single-writer ingest. A **mismatch**
/// means a concurrent writer advanced the head while the OS held an older version,
/// so the staged local write is routed through the engine's shared conflict
/// auto-resolve ([`ProviderEngine::ingest_conflicting`]) rather than clobbering the
/// newer head. Acks off the recorded-head gate either way (`upload_ack`).
///
/// The comparison is exact: the OS's `baseVersion.contentVersion` is a value this
/// same layer handed it earlier (via [`serve_item`] / [`serve_fetch`] / enumerate,
/// all deriving through [`content_version`]), so `base == current` holds iff the
/// row is still at the version the OS last saw. A cross-hash-space false mismatch
/// (a re-folded placeholder vs a hydrated base) resolves through the same
/// retention-first path and is therefore non-destructive; the loser is retained
/// and the head converges.
pub async fn serve_ingest_with_base<E: ProviderEngine>(
    engine: &E,
    rel: &str,
    base_version: &[u8],
) -> Result<WriteAck> {
    refuse_read_only(engine)?;
    // Ignored rels never ingest — not even through the conflict arm (a tracked row
    // for an ignored rel — WebDAV writes bypass the client ignore gate — must
    // exclude its modify, not auto-resolve; see `serve_ingest`).
    if engine.is_ignored(rel) {
        return Ok(WriteAck::excluded());
    }
    let current = serve_item(engine, rel)
        .await?
        .map(|i| i.content_version)
        .unwrap_or_default();
    if base_version.is_empty() || base_version == current.as_slice() {
        return serve_ingest(engine, rel).await;
    }
    let outcome = engine.ingest_conflicting(rel).await?;
    Ok(upload_ack(&outcome))
}

/// `deleteItem` → tombstone the path, acking **only on
/// `UploadOutcome::recorded`** — the same gate as [`serve_ingest`]: a
/// not-recorded delete must stay pending OS-side (the offline queue) instead of
/// being acked locally gone while the nest and every other device still hold
/// the file.
pub async fn serve_delete<E: ProviderEngine>(engine: &E, rel: &str) -> Result<WriteAck> {
    refuse_read_only(engine)?;
    let outcome = engine.delete(rel).await?;
    Ok(upload_ack(&outcome))
}

/// rename / move → tombstone the old path, then ingest the new one (there is no
/// dedicated rename record — a rename is a delete+create pair). Acks only when
/// **both** halves recorded; a not-recorded delete short-circuits (the new path
/// is not ingested — recording the create first would duplicate the file on
/// every other device while the old path lives on). The un-acked rename stays
/// pending OS-side and the retry re-drives both halves: a by-then-recorded
/// delete resolves vacuously (`handle_delete` on a gone row) and a by-then
/// recorded ingest skips honestly (`upload_file`'s recorded-head-proof skip),
/// so the retry converges to an ack instead of spinning.
pub async fn serve_rename<E: ProviderEngine>(
    engine: &E,
    from_rel: &str,
    to_rel: &str,
) -> Result<WriteAck> {
    Ok(rename_halves(engine, from_rel, to_rel).await?.ack)
}

/// The two halves of a rename, reported apart — [`serve_rename`] is its ack;
/// [`owned_tree`] also needs to know whether the old path's tombstone landed,
/// because a provider that owns its tree must put the body back when it did not.
pub(crate) struct RenameHalves {
    /// The old path's delete reached the nest (or had nothing to record).
    pub delete_recorded: bool,
    /// The rename's ack, exactly as [`serve_rename`] reports it.
    pub ack: WriteAck,
}

/// [`serve_rename`]'s body, reporting the delete half on its own.
pub(crate) async fn rename_halves<E: ProviderEngine>(
    engine: &E,
    from_rel: &str,
    to_rel: &str,
) -> Result<RenameHalves> {
    refuse_read_only(engine)?;
    let deleted = engine.delete(from_rel).await?;
    if !deleted.recorded {
        return Ok(RenameHalves {
            delete_recorded: false,
            ack: upload_ack(&deleted),
        });
    }
    // Renaming INTO an ignored name takes the file out of the set: the old
    // path's tombstone above is real (every other device drops it), the new
    // name is never ingested, and the OS is told `excluded` so it keeps the
    // local file without retrying. A rename FROM an ignored name needs no twin
    // arm — the from-side has no row, so the delete above resolves vacuously
    // and the new path ingests as an ordinary create.
    if engine.is_ignored(to_rel) {
        return Ok(RenameHalves {
            delete_recorded: true,
            ack: WriteAck::excluded(),
        });
    }
    let outcome = engine.ingest(to_rel).await?;
    Ok(RenameHalves {
        delete_recorded: true,
        ack: upload_ack(&outcome),
    })
}

/// observed eviction → demote the materialized file back to a placeholder row.
pub async fn serve_evict<E: ProviderEngine>(engine: &E, rel: &str) -> Result<()> {
    engine.mark_placeholder(rel)
}

/// eviction-observation tick → the rels the host believes are materialized.
/// The extension diffs these against the OS's materialized-item enumerator
/// (`NSFileProviderManager.enumeratorForMaterializedItems`) and calls
/// [`serve_evict`] for each rel the OS dropped. The diff itself is pure and
/// lives extension-side (`FileProviderEviction.evictionCandidates`) so it is
/// headlessly unit-tested; only the OS query is the live inch.
///
/// Direction note: the diff evicts only rels the HOST
/// thinks hydrated — a post-conflict-resolve `Placeholder` row whose loser
/// bytes are still on the OS's disk is absent from this list by construction,
/// so the tick can neither evict it nor mistake it for a fresh local edit
/// (nothing on this path ever ingests).
pub async fn serve_hydrated_rels<E: ProviderEngine>(engine: &E) -> Result<Vec<String>> {
    engine.hydrated_rels()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn row(rel: &str, size: u64, mtime: i64, version: &[u8]) -> ProviderRow {
        ProviderRow {
            rel: rel.into(),
            size,
            mtime,
            content_version: version.to_vec(),
        }
    }

    // ---- content_version ----

    #[test]
    fn content_version_prefers_recorded_then_manifest_then_empty() {
        let recorded = ContentHash::of_raw(b"recorded");
        let manifest = ContentHash::of_raw(b"manifest");
        assert_eq!(
            content_version(Some(recorded), Some(manifest)),
            recorded.digest().to_vec(),
            "hydration-proven recorded_content_hash wins"
        );
        assert_eq!(
            content_version(None, Some(manifest)),
            manifest.digest().to_vec(),
            "un-hydrated placeholder falls back to the manifest head — never versionless"
        );
        assert!(
            content_version(None, None).is_empty(),
            "no head at all → empty (defensive; forces a re-fetch)"
        );
    }

    // ---- enumerate_children ----

    #[test]
    fn enumerate_children_at_root_folds_files_and_dirs_with_versions() {
        let rows = vec![
            row("a.txt", 3, 10, b"va"),
            row("sub/b.txt", 5, 20, b"vb"),
            row("sub/deep/c.txt", 7, 30, b"vc"),
        ];
        let out = enumerate_children(&rows, "");
        // Directories sort before files (immediate_children contract).
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "sub");
        assert!(out[0].is_dir);
        assert_eq!(out[0].size, 0);
        assert_eq!(out[0].mtime, 30, "dir carries newest descendant mtime");
        assert!(
            out[0].content_version.is_empty(),
            "a synthesized directory has no contentVersion"
        );
        assert_eq!(out[1].name, "a.txt");
        assert!(!out[1].is_dir);
        assert_eq!(out[1].rel, "a.txt");
        assert_eq!(
            out[1].content_version,
            b"va".to_vec(),
            "file carries its version"
        );
    }

    #[test]
    fn enumerate_children_nested_joins_version_by_full_path() {
        let rows = vec![
            row("sub/b.txt", 5, 20, b"vb"),
            row("sub/deep/c.txt", 7, 30, b"vc"),
        ];
        let out = enumerate_children(&rows, "sub");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "deep");
        assert!(out[0].is_dir);
        assert_eq!(
            out[1].rel, "sub/b.txt",
            "file child carries its full rel identifier"
        );
        assert_eq!(out[1].content_version, b"vb".to_vec());
    }

    // ---- find_item ----

    #[test]
    fn find_item_resolves_file_dir_root_and_unknown() {
        let rows = vec![row("a.txt", 3, 10, b"va"), row("sub/b.txt", 5, 20, b"vb")];

        let file = find_item(&rows, "a.txt").expect("file");
        assert!(!file.is_dir);
        assert_eq!(file.size, 3);
        assert_eq!(file.content_version, b"va".to_vec());

        let dir = find_item(&rows, "sub").expect("dir");
        assert!(dir.is_dir);
        assert_eq!(dir.mtime, 20);
        assert!(dir.content_version.is_empty());

        let root = find_item(&rows, "").expect("root is a dir when rows exist");
        assert!(root.is_dir);
        assert_eq!(root.rel, "");

        assert!(find_item(&rows, "nope.txt").is_none());
        assert!(find_item(&[], "").is_none(), "empty set → no root item");
    }

    // ---- upload_ack ----

    #[test]
    fn upload_ack_is_true_only_when_recorded() {
        assert!(
            upload_ack(&UploadOutcome {
                recorded: true,
                ..Default::default()
            })
            .acked
        );
        assert!(!upload_ack(&UploadOutcome::default()).acked);
    }

    #[test]
    fn upload_ack_carries_content_changed_through() {
        let ack = upload_ack(&UploadOutcome {
            recorded: true,
            content_changed: true,
        });
        assert!(
            ack.content_changed,
            "a resolved merge/incoming winner must surface as shouldFetchContent"
        );
        assert!(
            !upload_ack(&UploadOutcome {
                recorded: true,
                content_changed: false,
            })
            .content_changed
        );
    }

    // ---- serving cores over a fake engine ----

    /// Records every mutating call so the tests can assert the callback→engine
    /// mapping (mark_hydrated with the served hash; delete-before-ingest order;
    /// ack keyed on `recorded`).
    #[derive(Default)]
    struct FakeEngine {
        rows: Vec<ProviderRow>,
        bytes: Vec<u8>,
        ingest_recorded: bool,
        conflicting_recorded: bool,
        conflicting_content_changed: bool,
        delete_recorded: bool,
        ignored: Vec<String>,
        hydrated_rels: Vec<String>,
        hydrated: RefCell<Vec<(String, ContentHash)>>,
        placeholdered: RefCell<Vec<String>>,
        deleted: RefCell<Vec<String>>,
        ingested: RefCell<Vec<String>>,
        conflicted: RefCell<Vec<String>>,
        read_only: bool,
    }

    #[async_trait::async_trait(?Send)]
    impl ProviderEngine for FakeEngine {
        async fn provider_rows(&self) -> Result<Vec<ProviderRow>> {
            Ok(self.rows.clone())
        }
        fn is_ignored(&self, rel: &str) -> bool {
            self.ignored.iter().any(|i| i == rel)
        }
        async fn fetch_bytes(&self, _rel: &str) -> Result<Vec<u8>> {
            Ok(self.bytes.clone())
        }
        async fn fetch_to_path(&self, _rel: &str, dest: &Path) -> Result<ContentHash> {
            std::fs::write(dest, &self.bytes)?;
            Ok(ContentHash::of_raw(&self.bytes))
        }
        fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> Result<()> {
            self.hydrated.borrow_mut().push((rel.into(), content_hash));
            Ok(())
        }
        fn mark_placeholder(&self, rel: &str) -> Result<()> {
            self.placeholdered.borrow_mut().push(rel.into());
            Ok(())
        }
        fn hydrated_rels(&self) -> Result<Vec<String>> {
            Ok(self.hydrated_rels.clone())
        }
        fn row_presence(&self, _rel: &str) -> Result<RowPresence> {
            Ok(RowPresence::Absent)
        }
        fn is_dehydration_safe(&self, _rel: &str) -> bool {
            false
        }
        async fn ingest(&self, rel: &str) -> Result<UploadOutcome> {
            self.ingested.borrow_mut().push(rel.into());
            Ok(UploadOutcome {
                recorded: self.ingest_recorded,
                ..Default::default()
            })
        }
        async fn ingest_conflicting(&self, rel: &str) -> Result<UploadOutcome> {
            self.conflicted.borrow_mut().push(rel.into());
            Ok(UploadOutcome {
                recorded: self.conflicting_recorded,
                content_changed: self.conflicting_content_changed,
            })
        }
        async fn delete(&self, rel: &str) -> Result<UploadOutcome> {
            self.deleted.borrow_mut().push(rel.into());
            Ok(UploadOutcome {
                recorded: self.delete_recorded,
                ..Default::default()
            })
        }
        fn read_only(&self) -> bool {
            self.read_only
        }
    }

    /// A reader's host has no write half: every write core refuses the typed
    /// `ReadOnlyHost` before the engine is reached — ingest (an ignored rel
    /// included: a reader's refusal is not an exclusion), the conflict arm,
    /// delete and rename — while every read core still serves.
    #[tokio::test]
    async fn a_read_only_engine_refuses_every_write_core_typed() {
        let engine = FakeEngine {
            rows: vec![row("a.txt", 1, 1, b"v")],
            bytes: b"body".to_vec(),
            ingest_recorded: true,
            conflicting_recorded: true,
            delete_recorded: true,
            ignored: vec![".hidden".into()],
            read_only: true,
            ..Default::default()
        };
        let refused = |r: Result<WriteAck>| r.unwrap_err().downcast_ref::<ReadOnlyHost>().is_some();
        assert!(refused(serve_ingest(&engine, "a.txt").await));
        assert!(refused(serve_ingest(&engine, ".hidden").await));
        assert!(refused(
            serve_ingest_with_base(&engine, "a.txt", b"stale").await
        ));
        assert!(refused(serve_ingest_with_base(&engine, "a.txt", b"").await));
        assert!(refused(serve_delete(&engine, "a.txt").await));
        assert!(refused(serve_rename(&engine, "a.txt", "b.txt").await));
        assert!(engine.ingested.borrow().is_empty());
        assert!(engine.conflicted.borrow().is_empty());
        assert!(engine.deleted.borrow().is_empty());

        assert_eq!(serve_enumerate(&engine, "").await.unwrap().len(), 1);
        assert!(serve_item(&engine, "a.txt").await.unwrap().is_some());
        assert_eq!(serve_fetch(&engine, "a.txt").await.unwrap().bytes, b"body");
        serve_evict(&engine, "a.txt").await.unwrap();
        assert_eq!(*engine.placeholdered.borrow(), vec!["a.txt".to_string()]);
    }

    #[tokio::test]
    async fn serve_fetch_marks_hydrated_with_the_served_hash_and_returns_it_as_version() {
        let engine = FakeEngine {
            bytes: b"hello world".to_vec(),
            ..Default::default()
        };
        let fetched = serve_fetch(&engine, "a.txt").await.expect("fetch");
        let expected = ContentHash::of_raw(b"hello world");
        assert_eq!(fetched.bytes, b"hello world".to_vec());
        assert_eq!(
            fetched.content_version,
            expected.digest().to_vec(),
            "the served content's hash is the new contentVersion"
        );
        assert_eq!(
            *engine.hydrated.borrow(),
            vec![("a.txt".to_string(), expected)],
            "fetch stamps recorded_content_hash so the dehydration gate stays honest"
        );
    }

    #[tokio::test]
    async fn serve_fetch_to_path_writes_dest_and_marks_hydrated() {
        let engine = FakeEngine {
            bytes: b"streamed to disk".to_vec(),
            ..Default::default()
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("a.txt");
        let version = serve_fetch_to_path(&engine, "a.txt", &dest)
            .await
            .expect("fetch_to_path");
        let expected = ContentHash::of_raw(b"streamed to disk");
        assert_eq!(
            std::fs::read(&dest).expect("dest exists"),
            b"streamed to disk".to_vec(),
            "the plaintext lands at dest, never crossing the seam as one buffer"
        );
        assert_eq!(
            version,
            expected.digest().to_vec(),
            "the verified whole-file hash is the new contentVersion"
        );
        assert_eq!(
            *engine.hydrated.borrow(),
            vec![("a.txt".to_string(), expected)],
            "same dehydration-honesty stamp as serve_fetch"
        );
    }

    #[tokio::test]
    async fn serve_ingest_acks_only_when_recorded() {
        let recorded = FakeEngine {
            ingest_recorded: true,
            ..Default::default()
        };
        assert!(
            serve_ingest(&recorded, "a.txt")
                .await
                .expect("ingest")
                .acked
        );

        let unrecorded = FakeEngine {
            ingest_recorded: false,
            ..Default::default()
        };
        assert!(
            !serve_ingest(&unrecorded, "a.txt")
                .await
                .expect("ingest")
                .acked,
            "an upload whose change record did not land must NOT be acked to the OS"
        );
        assert_eq!(*unrecorded.ingested.borrow(), vec!["a.txt".to_string()]);
    }

    #[tokio::test]
    async fn serve_ingest_with_base_fast_forwards_when_base_matches_current() {
        // The OS's base == the row's current head → the head has not moved, so this
        // is an ordinary single-writer ingest, never the conflict path.
        let engine = FakeEngine {
            rows: vec![row("a.txt", 3, 10, b"v1")],
            ingest_recorded: true,
            ..Default::default()
        };
        let ack = serve_ingest_with_base(&engine, "a.txt", b"v1")
            .await
            .expect("ingest_with_base");
        assert!(ack.acked, "a recorded fast-forward acks");
        assert_eq!(*engine.ingested.borrow(), vec!["a.txt".to_string()]);
        assert!(
            engine.conflicted.borrow().is_empty(),
            "a matching base never routes through auto-resolve"
        );
    }

    #[tokio::test]
    async fn serve_ingest_with_base_empty_base_fast_forwards() {
        // No base to check (a create, or an OS that supplied none) → plain ingest.
        let engine = FakeEngine {
            rows: vec![row("a.txt", 3, 10, b"v2")],
            ingest_recorded: true,
            ..Default::default()
        };
        let ack = serve_ingest_with_base(&engine, "a.txt", b"")
            .await
            .expect("ingest_with_base");
        assert!(ack.acked);
        assert_eq!(*engine.ingested.borrow(), vec!["a.txt".to_string()]);
        assert!(engine.conflicted.borrow().is_empty());
    }

    #[tokio::test]
    async fn serve_ingest_with_base_routes_to_conflict_on_mismatch() {
        // The OS edited base `v1` but the row's head is now `v2` (a concurrent
        // writer advanced it) → the staged write goes through auto-resolve, NOT a
        // last-writer-wins ingest that would clobber the newer head.
        let engine = FakeEngine {
            rows: vec![row("a.txt", 3, 10, b"v2")],
            conflicting_recorded: true,
            ..Default::default()
        };
        let ack = serve_ingest_with_base(&engine, "a.txt", b"v1")
            .await
            .expect("ingest_with_base");
        assert!(ack.acked, "a resolved conflict that reported acks");
        assert_eq!(
            *engine.conflicted.borrow(),
            vec!["a.txt".to_string()],
            "a base mismatch routes through ingest_conflicting"
        );
        assert!(
            engine.ingested.borrow().is_empty(),
            "the plain single-writer ingest is NOT taken on a mismatch"
        );
    }

    #[tokio::test]
    async fn serve_ingest_with_base_conflict_surfaces_content_changed() {
        // A merge / incoming winner re-points the row at content the OS does not
        // hold; the ack must carry content_changed so the extension returns
        // shouldFetchContent: true — else the OS associates its loser bytes with
        // the winning version and the next edit fast-forwards over the resolve.
        let engine = FakeEngine {
            rows: vec![row("a.txt", 3, 10, b"v2")],
            conflicting_recorded: true,
            conflicting_content_changed: true,
            ..Default::default()
        };
        let ack = serve_ingest_with_base(&engine, "a.txt", b"v1")
            .await
            .expect("ingest_with_base");
        assert!(ack.acked);
        assert!(ack.content_changed);

        // A plain fast-forward (base matches) never asks for a re-fetch.
        let ff = FakeEngine {
            rows: vec![row("a.txt", 3, 10, b"v1")],
            ingest_recorded: true,
            ..Default::default()
        };
        let ack = serve_ingest_with_base(&ff, "a.txt", b"v1")
            .await
            .expect("ingest_with_base");
        assert!(ack.acked);
        assert!(!ack.content_changed);
    }

    #[tokio::test]
    async fn serve_ingest_with_base_conflict_ack_keys_on_recorded() {
        // An unresolved conflict (report could not land) leaves the change pending:
        // the ack must be false so the OS keeps the local copy for retry.
        let engine = FakeEngine {
            rows: vec![row("a.txt", 3, 10, b"v2")],
            conflicting_recorded: false,
            ..Default::default()
        };
        let ack = serve_ingest_with_base(&engine, "a.txt", b"v1")
            .await
            .expect("ingest_with_base");
        assert!(
            !ack.acked,
            "an unresolved conflict must NOT be acked to the OS"
        );
        assert_eq!(*engine.conflicted.borrow(), vec!["a.txt".to_string()]);
    }

    #[tokio::test]
    async fn serve_rename_tombstones_old_then_ingests_new() {
        let engine = FakeEngine {
            ingest_recorded: true,
            delete_recorded: true,
            ..Default::default()
        };
        let ack = serve_rename(&engine, "old.txt", "new.txt")
            .await
            .expect("rename");
        assert!(ack.acked, "rename acks when both halves recorded");
        assert_eq!(*engine.deleted.borrow(), vec!["old.txt".to_string()]);
        assert_eq!(*engine.ingested.borrow(), vec!["new.txt".to_string()]);
    }

    #[tokio::test]
    async fn serve_delete_acks_only_when_recorded() {
        let recorded = FakeEngine {
            delete_recorded: true,
            ..Default::default()
        };
        assert!(
            serve_delete(&recorded, "a.txt")
                .await
                .expect("delete")
                .acked
        );

        let unrecorded = FakeEngine::default();
        assert!(
            !serve_delete(&unrecorded, "a.txt")
                .await
                .expect("delete")
                .acked,
            "a delete whose change record did not land must NOT be acked to the OS \
             — the un-acked change stays pending OS-side and retries"
        );
        assert_eq!(*unrecorded.deleted.borrow(), vec!["a.txt".to_string()]);
    }

    #[tokio::test]
    async fn serve_rename_requires_both_recorded() {
        // The delete half fails to record → the rename must not ack, and the new
        // path must NOT be ingested (recording the create while the old path
        // lives on would duplicate the file on every other device).
        let engine = FakeEngine {
            ingest_recorded: true,
            delete_recorded: false,
            ..Default::default()
        };
        let ack = serve_rename(&engine, "old.txt", "new.txt")
            .await
            .expect("rename");
        assert!(
            !ack.acked,
            "a rename whose delete half did not record must not ack"
        );
        assert_eq!(*engine.deleted.borrow(), vec!["old.txt".to_string()]);
        assert!(
            engine.ingested.borrow().is_empty(),
            "the new path is not ingested while the old path's delete is unrecorded"
        );
    }

    // ---- the ignore gate (file-sync.md § Built-in default ignores) ----

    #[tokio::test]
    async fn serve_ingest_never_reaches_engine_for_ignored_rel() {
        let engine = FakeEngine {
            ignored: vec!["~$report.docx".into()],
            ingest_recorded: true,
            ..Default::default()
        };
        let ack = serve_ingest(&engine, "~$report.docx")
            .await
            .expect("ingest");
        assert!(ack.excluded, "ignored litter answers excluded, never acked");
        assert!(!ack.acked);
        assert!(
            engine.ingested.borrow().is_empty(),
            "an ignored rel's createItem/modifyItem must never seal, upload, or record"
        );
    }

    #[tokio::test]
    async fn serve_ingest_with_base_never_reaches_engine_for_ignored_rel() {
        // Even a mismatched base must not route litter through the conflict
        // auto-resolve (a tracked row for an ignored rel: its modify excludes, not resolves).
        let engine = FakeEngine {
            ignored: vec!["draft.tmp".into()],
            rows: vec![row("draft.tmp", 1, 1, b"v9")],
            conflicting_recorded: true,
            ingest_recorded: true,
            ..Default::default()
        };
        let ack = serve_ingest_with_base(&engine, "draft.tmp", b"v1")
            .await
            .expect("ingest_with_base");
        assert!(ack.excluded);
        assert!(engine.ingested.borrow().is_empty());
        assert!(engine.conflicted.borrow().is_empty());
    }

    #[tokio::test]
    async fn serve_rename_to_ignored_tombstones_old_without_ingesting_new() {
        let engine = FakeEngine {
            ignored: vec!["report.tmp".into()],
            delete_recorded: true,
            ingest_recorded: true,
            ..Default::default()
        };
        let ack = serve_rename(&engine, "report.docx", "report.tmp")
            .await
            .expect("rename");
        assert!(ack.excluded);
        assert_eq!(
            *engine.deleted.borrow(),
            vec!["report.docx".to_string()],
            "the old path leaves the set for every other device"
        );
        assert!(engine.ingested.borrow().is_empty());
    }

    #[tokio::test]
    async fn serve_rename_to_ignored_unrecorded_delete_stays_pending() {
        let engine = FakeEngine {
            ignored: vec!["report.tmp".into()],
            delete_recorded: false,
            ..Default::default()
        };
        let ack = serve_rename(&engine, "report.docx", "report.tmp")
            .await
            .expect("rename");
        assert!(
            !ack.excluded && !ack.acked,
            "the tombstone has not landed — the OS must retry, not exclude"
        );
    }

    #[tokio::test]
    async fn serve_rename_from_ignored_ingests_new_normally() {
        let engine = FakeEngine {
            ignored: vec!["~$x.docx".into()],
            delete_recorded: true,
            ingest_recorded: true,
            ..Default::default()
        };
        let ack = serve_rename(&engine, "~$x.docx", "x.docx")
            .await
            .expect("rename");
        assert!(ack.acked && !ack.excluded);
        assert_eq!(*engine.ingested.borrow(), vec!["x.docx".to_string()]);
    }

    #[tokio::test]
    async fn serve_delete_still_reaches_engine_for_ignored_rel() {
        // A tracked row for an ignored rel (a WebDAV write bypassing the client
        // ignore gate, or a later ignore rule) must stay deletable.
        let engine = FakeEngine {
            ignored: vec!["~$old.docx".into()],
            delete_recorded: true,
            ..Default::default()
        };
        let ack = serve_delete(&engine, "~$old.docx").await.expect("delete");
        assert!(ack.acked && !ack.excluded);
        assert_eq!(*engine.deleted.borrow(), vec!["~$old.docx".to_string()]);
    }

    #[tokio::test]
    async fn serve_evict_and_delete_route_to_the_honest_row_primitives() {
        let engine = FakeEngine::default();
        serve_evict(&engine, "a.txt").await.expect("evict");
        serve_delete(&engine, "b.txt").await.expect("delete");
        assert_eq!(*engine.placeholdered.borrow(), vec!["a.txt".to_string()]);
        assert_eq!(*engine.deleted.borrow(), vec!["b.txt".to_string()]);
    }

    #[tokio::test]
    async fn serve_hydrated_rels_reads_the_synced_set() {
        let engine = FakeEngine {
            hydrated_rels: vec!["a.txt".into(), "sub/b.txt".into()],
            ..Default::default()
        };
        assert_eq!(
            serve_hydrated_rels(&engine).await.expect("hydrated_rels"),
            vec!["a.txt".to_string(), "sub/b.txt".to_string()]
        );
    }

    #[tokio::test]
    async fn serve_enumerate_and_item_read_through_provider_rows() {
        let engine = FakeEngine {
            rows: vec![row("a.txt", 3, 10, b"va"), row("sub/b.txt", 5, 20, b"vb")],
            ..Default::default()
        };
        let listed = serve_enumerate(&engine, "").await.expect("enumerate");
        assert_eq!(listed.len(), 2);
        let item = serve_item(&engine, "a.txt")
            .await
            .expect("item")
            .expect("some");
        assert_eq!(item.content_version, b"va".to_vec());
    }
}
