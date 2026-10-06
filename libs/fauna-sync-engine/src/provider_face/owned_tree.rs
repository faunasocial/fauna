//! The owned-tree operations — what the OS does for apple's File Provider,
//! written once for a provider that owns its tree.
//!
//! `docs/goal/behavior/on-demand-files.md` § Android SAF DocumentsProvider
//! binding owns the rules; this module is their shared-Rust home. On apple the
//! OS keeps the replica, re-drives an un-acked change and evicts under
//! pressure. A SAF `DocumentsProvider` has no OS doing any of that, so the
//! provider keeps the replica itself, in two roots per set:
//!
//! - the **kept root** (the engine's `root_dir`) holds exactly the bodies this
//!   device must not lose — a local edit the nest has not recorded yet. A body
//!   found there IS the write intent: [`OwnedTree::start_sweep`] ingests every
//!   one, so a process death between a close and its upload loses nothing and
//!   needs no journal. One body there is not a write intent: a body the share
//!   plane landed from a peer for a row the nest has not confirmed
//!   ([`OwnedTree::share_ingest`]). It is kept for the same reason — the nest
//!   cannot serve it again — but it was never this replica's authorship, so it
//!   is never ingested; the nest's own row for the path confirms it (and it is
//!   demoted) or supersedes it (and it is dropped) —
//!   [`OwnedTree::settle_peer_bodies`];
//! - the **cache root** holds recorded bodies, which the nest can always serve
//!   again, so the OS's cache reclaim is this platform's *free up space*. A
//!   reclaimed body is **observed** ([`OwnedTree::observe_evictions`]) and the
//!   row goes back to a placeholder — it is never read as a deletion.
//!
//! A lookup reads the kept root first, then the cache root; a body in neither
//! is a placeholder, and a placeholder is *present-but-unreadable*: an open
//! fetches the real bytes or fails, never answering with a stand-in.
//!
//! Every operation composes the shared serving cores ([`super::serve_item`],
//! [`super::serve_ingest_with_base`], [`super::serve_delete`],
//! [`super::serve_evict`], …) over the [`ProviderEngine`] seam, so the fake
//! engine below proves the mapping and `SyncEngine` is the production impl.
//! Deletes and renames are **record-first**: one that cannot be recorded is an
//! error to the caller and changes nothing locally.
//!
//! A reader's tree has no write half ([`super::ReadOnlyHost`]): an open for
//! write, a close, a create, a delete and a rename are refused before anything
//! on disk moves, and the sweep ingests nothing — a reader authored no body, so
//! the only bodies its kept root can hold are peer-landed ones, which were
//! never write intents.
//!
//! Rels arrive from other apps (a SAF document id is caller-supplied), so every
//! entry point validates the rel before it touches the disk: no absolute path,
//! no empty, `.` or `..` component, no backslash or NUL.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};

use fauna_core::data::ContentHash;
use fauna_core::folder_keys::ActorScopedFolderRef;

use super::{
    ProviderEngine, RowPresence, WriteAck, refuse_read_only, rename_halves, serve_delete,
    serve_evict, serve_ingest, serve_ingest_with_base, serve_item, upload_ack,
};

/// The directory, inside each root, where bodies are written before they are
/// renamed into place. A dot-directory, so no rel under it is ever ingested
/// (the dotfile ignore); emptied by every [`OwnedTree::start_sweep`] — a
/// half-fetched body is discarded at the next start.
pub const TEMP_DIR: &str = ".fauna-tmp";

/// The directory under the platform's files and cache directories that holds
/// every account's on-demand sets ([`OwnedTree::for_set`]).
pub const ON_DEMAND_DIR: &str = "on-demand";

/// Which of the two roots a body lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyRoot {
    /// Bodies the nest may not hold yet — never freed by anyone but a recorded
    /// ack.
    Kept,
    /// Recorded bodies — the OS may reclaim them at any time.
    Cache,
}

/// What [`OwnedTree::open_for_write`] hands the caller: where to write, and the
/// `contentVersion` the open saw, which the close passes back so a head that
/// moved meanwhile routes through the shared conflict auto-resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOpen {
    pub path: PathBuf,
    pub base_version: Vec<u8>,
}

/// What one [`OwnedTree::start_sweep`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Kept-root bodies whose change the nest recorded.
    pub recorded: Vec<String>,
    /// Kept-root bodies still waiting (not recorded, or the ingest failed) —
    /// they stay in the kept root for the next sweep.
    pub pending: Vec<String>,
    /// Hydrated rows whose body was gone, now placeholders again.
    pub evicted: Vec<String>,
}

/// One set's owned tree: its two roots, and the writers open on it.
///
/// Single-threaded like the engine it drives (`ProviderEngine` is `?Send`);
/// the host's worker owns it.
#[derive(Debug)]
pub struct OwnedTree {
    kept: PathBuf,
    cache: PathBuf,
    /// Descriptors opened for write and not yet closed, per rel. A body is
    /// never moved out of the kept root while one is open — a second writer's
    /// bytes would land in a file nothing ingests.
    writers: RefCell<HashMap<String, u32>>,
}

impl OwnedTree {
    /// A tree over two explicit roots. The kept root must be the engine's
    /// `root_dir` — the engine reads a write's bytes from there.
    pub fn new(kept: PathBuf, cache: PathBuf) -> Self {
        Self {
            kept,
            cache,
            writers: RefCell::default(),
        }
    }

    /// The tree for one set of one account: the kept root
    /// `<files_dir>/on-demand/<actor-id-hex>/<ref-component>/` and the cache
    /// root `<cache_dir>/on-demand/<actor-id-hex>/<ref-component>/` — account
    /// outermost, the ref the shared `FolderRef::path_component` (`local%3A1`:
    /// the identifier's own ref half, apple's staging-root grammar, over the
    /// same [`ActorScopedFolderRef`]).
    pub fn for_set(files_dir: &Path, cache_dir: &Path, set: &ActorScopedFolderRef) -> Self {
        let tail = Path::new(ON_DEMAND_DIR)
            .join(hex::encode(set.actor_id))
            .join(set.folder.path_component());
        Self::new(files_dir.join(&tail), cache_dir.join(tail))
    }

    pub fn kept_root(&self) -> &Path {
        &self.kept
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache
    }

    /// Where `rel`'s body lives in `root` (whether or not it is there).
    fn path_in(&self, root: BodyRoot, rel: &str) -> PathBuf {
        match root {
            BodyRoot::Kept => self.kept.join(rel),
            BodyRoot::Cache => self.cache.join(rel),
        }
    }

    // ---- The three operations the peer-transfer plane consumes ----

    /// Look up `rel`'s body: the kept root, then the cache root, else `None` —
    /// a placeholder. A cache-root body counts only under a hydrated row: once
    /// a refresh re-points the row at a moved head, the cached bytes are the
    /// superseded version and are never served as the file.
    pub fn lookup_body<E: ProviderEngine>(&self, engine: &E, rel: &str) -> Result<Option<PathBuf>> {
        body_path(&self.kept, &self.cache, rel, || {
            Ok(engine.row_presence(rel)? == RowPresence::Hydrated)
        })
    }

    /// Land a body the caller holds at `src` into `root` as `rel`'s body
    /// (moved, temp-then-rename). The kept root takes a body the nest may not
    /// hold yet — the next sweep ingests it; the cache root takes one the nest
    /// does hold, and the row is marked hydrated with the body's hash unless a
    /// kept-root edit shadows it.
    pub fn land_body<E: ProviderEngine>(
        &self,
        engine: &E,
        rel: &str,
        src: &Path,
        root: BodyRoot,
    ) -> Result<PathBuf> {
        let rel = check_rel(rel)?;
        let dest = self.move_into(src, root, rel)?;
        if root == BodyRoot::Cache && !self.path_in(BodyRoot::Kept, rel).is_file() {
            let hash = fauna_core::chunker_stream::content_hash_streaming(&dest)?;
            engine.mark_hydrated(rel, hash)?;
        }
        Ok(dest)
    }

    /// Record `rel`'s row as a placeholder and drop its cache-root body. A
    /// kept-root body is never touched: it is an edit the nest has not recorded.
    pub fn record_placeholder<E: ProviderEngine>(&self, engine: &E, rel: &str) -> Result<()> {
        let rel = check_rel(rel)?;
        engine.mark_placeholder(rel)?;
        remove_if_present(&self.path_in(BodyRoot::Cache, rel))
    }

    // ---- Reads ----

    /// Open `rel` for reading: its body's path, hydrating it into the cache
    /// root first when it has none. A rel with no live row, or whose body
    /// cannot be fetched, is an error — never a stand-in body.
    pub async fn open_for_read<E: ProviderEngine>(&self, engine: &E, rel: &str) -> Result<PathBuf> {
        let rel = check_rel(rel)?;
        if let Some(body) = self.lookup_body(engine, rel)? {
            return Ok(body);
        }
        let presence = engine.row_presence(rel)?;
        if presence == RowPresence::Absent {
            bail!(
                "no such document: {}",
                fauna_core::log_redact::log_path(rel)
            );
        }
        if presence == RowPresence::Hydrated {
            // The body is gone behind our back: observed at open.
            serve_evict(engine, rel).await?;
        }
        let (path, hash) = self.fetch_into(engine, rel, BodyRoot::Cache).await?;
        engine.mark_hydrated(rel, hash)?;
        Ok(path)
    }

    // ---- Writes ----

    /// Open `rel` for writing: its body is promoted into the kept root
    /// (hydrated first when it has none), and the returned base is the
    /// `contentVersion` the caller is editing. Only an existing document opens
    /// for write — a new one is [`Self::create`]d first. Every open must be
    /// matched by one [`Self::closed_write`].
    pub async fn open_for_write<E: ProviderEngine>(
        &self,
        engine: &E,
        rel: &str,
    ) -> Result<WriteOpen> {
        let rel = check_rel(rel)?;
        refuse_read_only(engine)?;
        refuse_ignored(engine, rel)?;
        let kept = self.path_in(BodyRoot::Kept, rel);
        if !kept.is_file() {
            let presence = engine.row_presence(rel)?;
            let cache = self.path_in(BodyRoot::Cache, rel);
            match presence {
                RowPresence::Absent => {
                    bail!(
                        "no such document: {}",
                        fauna_core::log_redact::log_path(rel)
                    )
                }
                RowPresence::Hydrated if cache.is_file() => {
                    self.move_into(&cache, BodyRoot::Kept, rel)?;
                    // The served head's bytes, now at the engine's root: stamp
                    // their identity so an unchanged close records nothing new.
                    let hash = fauna_core::chunker_stream::content_hash_streaming(&kept)?;
                    engine.mark_hydrated(rel, hash)?;
                }
                _ => {
                    if presence == RowPresence::Hydrated {
                        serve_evict(engine, rel).await?;
                    }
                    let (_, hash) = self.fetch_into(engine, rel, BodyRoot::Kept).await?;
                    engine.mark_hydrated(rel, hash)?;
                }
            }
        }
        // Read AFTER the stamp above: the stamp itself moves the version from
        // the manifest's to the content's, and a base read before it would
        // send every first close down the conflict arm.
        let base_version = serve_item(engine, rel)
            .await?
            .map(|item| item.content_version)
            .unwrap_or_default();
        *self
            .writers
            .borrow_mut()
            .entry(rel.to_string())
            .or_default() += 1;
        Ok(WriteOpen {
            path: kept,
            base_version,
        })
    }

    /// Close one write opened by [`Self::open_for_write`]: ingest the kept-root
    /// body against the base the open saw. On a recorded ack — and once no
    /// other writer holds `rel` open — the body is demoted to the cache root
    /// (on the engine's dehydration proof), or dropped when a conflict moved
    /// the row to another head, so the next open fetches the winner. A change
    /// that did not record stays in the kept root for the next sweep.
    pub async fn closed_write<E: ProviderEngine>(
        &self,
        engine: &E,
        rel: &str,
        base_version: &[u8],
    ) -> Result<WriteAck> {
        let rel = check_rel(rel)?;
        self.release_writer(rel);
        refuse_read_only(engine)?;
        if !self.path_in(BodyRoot::Kept, rel).is_file() {
            bail!(
                "closed write of {}: no body in the kept root",
                fauna_core::log_redact::log_path(rel)
            );
        }
        let ack = self.ingest_kept(engine, rel, Some(base_version)).await?;
        if !self.writers.borrow().contains_key(rel) {
            self.settle(engine, rel, &ack)?;
        }
        Ok(ack)
    }

    /// End one writer of `rel` without ingesting — a close the host refused
    /// before it could serve it (a pre-seal hold: offline, no signer). The
    /// body stays in the kept root, and once no writer holds it the next
    /// [`Self::start_sweep`] re-drives it. [`Self::closed_write`] calls this
    /// itself; every open is ended by exactly one of the two.
    pub fn release_writer(&self, rel: &str) {
        let mut writers = self.writers.borrow_mut();
        if let Some(n) = writers.get_mut(rel) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                writers.remove(rel);
            }
        }
    }

    /// Create an empty document at `rel` in the kept root and ingest it. An
    /// ignored name, or one that already names a document or a directory, is
    /// refused and creates nothing.
    pub async fn create<E: ProviderEngine>(&self, engine: &E, rel: &str) -> Result<WriteAck> {
        let rel = check_rel(rel)?;
        refuse_read_only(engine)?;
        refuse_ignored(engine, rel)?;
        self.refuse_existing(engine, rel).await?;
        let kept = self.path_in(BodyRoot::Kept, rel);
        if let Some(parent) = kept.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&kept)
            .with_context(|| format!("creating {}", fauna_core::log_redact::log_path(rel)))?;
        let ack = self.ingest_kept(engine, rel, None).await?;
        self.settle(engine, rel, &ack)?;
        Ok(ack)
    }

    /// Delete `rel`, record-first: the tombstone reaches the nest, then both
    /// bodies go. A delete that cannot be recorded is an error and changes
    /// nothing.
    ///
    /// A directory (only its files' paths — it has no row of its own) is
    /// deleted file by file, each file record-first, in path order. The walk
    /// is not atomic: it stops at the first file whose delete cannot be
    /// recorded and is an error, and the files before it stay deleted — every
    /// file is either recorded gone or untouched, never lost, and repeating
    /// the delete finishes the rest.
    pub async fn delete<E: ProviderEngine>(&self, engine: &E, rel: &str) -> Result<()> {
        let rel = check_rel(rel)?;
        refuse_read_only(engine)?;
        if let Some(files) = self.directory_files(engine, rel).await? {
            for file in files {
                self.delete_document(engine, &file).await?;
            }
            return Ok(());
        }
        self.delete_document(engine, rel).await
    }

    async fn delete_document<E: ProviderEngine>(&self, engine: &E, rel: &str) -> Result<()> {
        let ack = serve_delete(engine, rel).await?;
        if !ack.acked {
            bail!(
                "delete of {} could not be recorded; nothing was changed",
                fauna_core::log_redact::log_path(rel)
            );
        }
        remove_if_present(&self.path_in(BodyRoot::Kept, rel))?;
        remove_if_present(&self.path_in(BodyRoot::Cache, rel))
    }

    /// Rename (or move) `from` to `to`, record-first: the body is placed at
    /// `to` in the kept root (fetched when `from` is a placeholder), then the
    /// old path's tombstone and the new path's create are recorded. A tombstone
    /// that cannot be recorded puts the body back and is an error — nothing
    /// changed. Once the tombstone landed the rename has happened: an
    /// un-recorded create leaves the body in the kept root for the next sweep,
    /// and the ack says so.
    ///
    /// A directory is renamed file by file (each file as above, in path
    /// order) under the same walk rule as [`Self::delete`]: the first file
    /// whose tombstone cannot be recorded stops it with an error, the files
    /// before it are already at their new paths, and every file is at exactly
    /// one of its two paths — repeating the rename finishes the rest. The
    /// ack is acked only when every file's create recorded. A directory is
    /// never moved into itself.
    pub async fn rename<E: ProviderEngine>(
        &self,
        engine: &E,
        from: &str,
        to: &str,
    ) -> Result<WriteAck> {
        let from = check_rel(from)?;
        let to = check_rel(to)?;
        refuse_read_only(engine)?;
        refuse_ignored(engine, to)?;
        if let Some(files) = self.directory_files(engine, from).await? {
            if to.starts_with(&format!("{from}/")) {
                bail!(
                    "{} cannot be moved into itself",
                    fauna_core::log_redact::log_path(from)
                );
            }
            self.refuse_existing(engine, to).await?;
            let mut all = WriteAck {
                acked: true,
                content_changed: false,
                excluded: false,
            };
            for file in files {
                let target = format!("{to}{}", &file[from.len()..]);
                let ack = self.rename_document(engine, &file, &target).await?;
                all.acked &= ack.acked;
            }
            return Ok(all);
        }
        self.rename_document(engine, from, to).await
    }

    async fn rename_document<E: ProviderEngine>(
        &self,
        engine: &E,
        from: &str,
        to: &str,
    ) -> Result<WriteAck> {
        refuse_ignored(engine, to)?;
        self.refuse_existing(engine, to).await?;

        let kept_from = self.path_in(BodyRoot::Kept, from);
        let cache_from = self.path_in(BodyRoot::Cache, from);
        let presence = engine.row_presence(from)?;
        // Where the body came from, so a refused rename can put it back.
        let origin = if kept_from.is_file() {
            self.move_into(&kept_from, BodyRoot::Kept, to)?;
            Some(BodyRoot::Kept)
        } else if presence == RowPresence::Hydrated && cache_from.is_file() {
            self.move_into(&cache_from, BodyRoot::Kept, to)?;
            Some(BodyRoot::Cache)
        } else if presence == RowPresence::Absent {
            bail!(
                "no such document: {}",
                fauna_core::log_redact::log_path(from)
            );
        } else {
            let tmp = self.temp_path(BodyRoot::Kept)?;
            engine.fetch_to_path(from, &tmp).await?;
            self.move_into(&tmp, BodyRoot::Kept, to)?;
            None
        };

        let halves = rename_halves(engine, from, to).await?;
        let kept_to = self.path_in(BodyRoot::Kept, to);
        if !halves.delete_recorded {
            match origin {
                Some(root) => {
                    self.move_into(&kept_to, root, from)?;
                }
                None => remove_if_present(&kept_to)?,
            }
            bail!(
                "rename of {} could not be recorded; nothing was changed",
                fauna_core::log_redact::log_path(from)
            );
        }
        remove_if_present(&cache_from)?;
        self.settle(engine, to, &halves.ack)?;
        Ok(halves.ack)
    }

    // ---- Start and refresh ----

    /// The host-start sweep: discard half-written temp bodies, ingest every
    /// kept-root body (the write intent a dead process left), demote each one
    /// the nest recorded, then observe evictions. Idempotent — a body already
    /// recorded skips on the engine's recorded-head proof. A peer-landed body
    /// the nest has not confirmed is not a write intent and stays pending,
    /// un-ingested. Per-body failures leave the body pending, never abort the
    /// sweep. A reader's tree ingests nothing — it authored no body — and only
    /// discards the temp bodies and observes evictions.
    pub async fn start_sweep<E: ProviderEngine>(&self, engine: &E) -> Result<SweepReport> {
        for root in [&self.kept, &self.cache] {
            let tmp = root.join(TEMP_DIR);
            if tmp.is_dir() {
                std::fs::remove_dir_all(&tmp)
                    .with_context(|| format!("discarding {}", tmp.display()))?;
            }
        }
        let mut report = SweepReport::default();
        if engine.read_only() {
            report.evicted = self.observe_evictions(engine).await?;
            return Ok(report);
        }
        for rel in kept_rels(&self.kept)? {
            if engine.is_ignored(&rel) || self.writers.borrow().contains_key(&rel) {
                continue;
            }
            match self.ingest_kept(engine, &rel, None).await {
                Ok(ack) if ack.acked => {
                    self.settle(engine, &rel, &ack)?;
                    report.recorded.push(rel);
                }
                Ok(_) => report.pending.push(rel),
                Err(e) => {
                    tracing::warn!(
                        path = %fauna_core::log_redact::log_path(&rel),
                        error = %e,
                        "owned tree: kept-root body not ingested — left for the next sweep"
                    );
                    report.pending.push(rel);
                }
            }
        }
        report.evicted = self.observe_evictions(engine).await?;
        Ok(report)
    }

    /// Every hydrated row whose body is in neither root goes back to a
    /// placeholder ([`super::serve_evict`]) — the OS reclaimed the cache.
    /// Never a delete. Returns the rels it flipped.
    pub async fn observe_evictions<E: ProviderEngine>(&self, engine: &E) -> Result<Vec<String>> {
        let mut evicted = Vec::new();
        for rel in engine.hydrated_rels()? {
            if check_rel(&rel).is_err() {
                continue;
            }
            if !self.path_in(BodyRoot::Kept, &rel).is_file()
                && !self.path_in(BodyRoot::Cache, &rel).is_file()
            {
                serve_evict(engine, &rel).await?;
                evicted.push(rel);
            }
        }
        Ok(evicted)
    }

    // ---- Internals ----

    /// Ingest `rel`'s kept-root body. A body over a placeholder row is an edit
    /// the nest's head moved under (a refresh re-pointed the row while the
    /// edit waited), so it takes the shared conflict auto-resolve whatever the
    /// base says; otherwise a known base goes through the base check and an
    /// unknown one (the sweep) is a plain ingest. A body the share plane
    /// landed from a peer is not ingested at all — recording it would publish
    /// another writer's file as this replica's own.
    async fn ingest_kept<E: ProviderEngine>(
        &self,
        engine: &E,
        rel: &str,
        base: Option<&[u8]>,
    ) -> Result<WriteAck> {
        if engine.is_ignored(rel) {
            return Ok(WriteAck::excluded());
        }
        if engine.holds_provisional_peer_body(rel)? {
            return Ok(WriteAck {
                acked: false,
                content_changed: false,
                excluded: false,
            });
        }
        if engine.row_presence(rel)? == RowPresence::Placeholder {
            return Ok(upload_ack(&engine.ingest_conflicting(rel).await?));
        }
        match base {
            Some(base) => serve_ingest_with_base(engine, rel, base).await,
            None => serve_ingest(engine, rel).await,
        }
    }

    /// After a write's ack: a recorded change whose head is these bytes leaves
    /// the kept root for the cache root (on the engine's own proof); one that
    /// moved the row to another head drops the local body; anything else stays
    /// kept.
    fn settle<E: ProviderEngine>(&self, engine: &E, rel: &str, ack: &WriteAck) -> Result<()> {
        if !ack.acked {
            return Ok(());
        }
        let kept = self.path_in(BodyRoot::Kept, rel);
        if ack.content_changed {
            return remove_if_present(&kept);
        }
        if engine.is_dehydration_safe(rel) {
            self.move_into(&kept, BodyRoot::Cache, rel)?;
        }
        Ok(())
    }

    /// Settle the peer-landed bodies one nest fold spoke for
    /// ([`crate::engine::OverlayReconcile`], the populate fold's reconcile):
    /// a **confirmed** body — the nest recorded exactly these bytes, and the
    /// fold stamped the proof — leaves the kept root for the cache root, on
    /// the engine's own dehydration proof like any recorded write; a
    /// **superseded** one — the nest's row for the path is some other head —
    /// is dropped, so the nest's row stands and the next open fetches it. A
    /// superseded body the user has since written over is no longer the body
    /// the peer landed: it stays, a write intent like any other. A body a
    /// writer still holds open is left for the next fold or sweep.
    pub fn settle_peer_bodies<E: ProviderEngine>(
        &self,
        engine: &E,
        reconcile: &crate::engine::OverlayReconcile,
    ) -> Result<()> {
        for rel in &reconcile.confirmed {
            if check_rel(rel).is_err() || self.writers.borrow().contains_key(rel) {
                continue;
            }
            let kept = self.path_in(BodyRoot::Kept, rel);
            if kept.is_file() && engine.is_dehydration_safe(rel) {
                self.move_into(&kept, BodyRoot::Cache, rel)?;
            }
        }
        for body in &reconcile.superseded {
            let rel = body.path.as_str();
            if check_rel(rel).is_err() || self.writers.borrow().contains_key(rel) {
                continue;
            }
            let kept = self.path_in(BodyRoot::Kept, rel);
            if !kept.is_file() {
                continue;
            }
            let disk = fauna_core::chunker_stream::content_hash_streaming(&kept)?;
            if hex::encode(disk.digest()) == body.content_hash {
                remove_if_present(&kept)?;
            }
        }
        Ok(())
    }

    /// The share plane's ingest door on an on-demand replica — the twin of
    /// the agent's `ShareIngest` command arm, run on the host's own worker
    /// (`p2p-shared-set-build.md` § *Phone peers — design*, decision 1). Every
    /// accepted row is recorded (the overlay row, and a new path as a
    /// placeholder); a body lands only when the landing policy wants it
    /// ([`crate::share_landing`]), in the kept root, above the storage floor.
    /// Provenance, relay retention and the pull cursor are the engine's own
    /// ([`crate::always_resident::ingest_share_page`]'s core) — one door, two
    /// landings.
    #[cfg(feature = "p2p-share")]
    pub async fn share_ingest(
        &self,
        engine: &crate::engine::SyncEngine,
        proven_actor_hex: &str,
        rows: &[Vec<u8>],
        spool_dir: PathBuf,
    ) -> Result<(crate::peer_share_store::PeerIngestReport, i64)> {
        let kept = self.kept.clone();
        let landing = crate::engine::OnDemandLanding {
            tree: self,
            free_space: &move || crate::share_landing::free_space(&kept),
        };
        crate::always_resident::ingest_share_page_landing(
            engine,
            proven_actor_hex,
            rows,
            spool_dir,
            Some(&landing),
        )
        .await
    }

    /// Refuse a rel that already names a document (a row or a body) or a
    /// directory.
    async fn refuse_existing<E: ProviderEngine>(&self, engine: &E, rel: &str) -> Result<()> {
        if engine.row_presence(rel)? != RowPresence::Absent
            || self.path_in(BodyRoot::Kept, rel).exists()
            || serve_item(engine, rel).await?.is_some()
        {
            bail!("{} already exists", fauna_core::log_redact::log_path(rel));
        }
        Ok(())
    }

    /// `rel`'s files when it names a directory (every live row under it, in
    /// path order), else `None`. A directory has no row: it is only its
    /// files' paths.
    async fn directory_files<E: ProviderEngine>(
        &self,
        engine: &E,
        rel: &str,
    ) -> Result<Option<Vec<String>>> {
        if !serve_item(engine, rel)
            .await?
            .is_some_and(|item| item.is_dir)
        {
            return Ok(None);
        }
        let prefix = format!("{rel}/");
        let mut files: Vec<String> = engine
            .provider_rows()
            .await?
            .into_iter()
            .map(|row| row.rel)
            .filter(|r| r.starts_with(&prefix))
            .collect();
        files.sort();
        files.dedup();
        Ok(Some(files))
    }

    /// Fetch `rel`'s recorded head into `root`, temp-then-rename.
    async fn fetch_into<E: ProviderEngine>(
        &self,
        engine: &E,
        rel: &str,
        root: BodyRoot,
    ) -> Result<(PathBuf, ContentHash)> {
        let tmp = self.temp_path(root)?;
        let hash = engine.fetch_to_path(rel, &tmp).await?;
        let dest = self.move_into(&tmp, root, rel)?;
        Ok((dest, hash))
    }

    /// A fresh temp path inside `root`'s [`TEMP_DIR`].
    pub(crate) fn temp_path(&self, root: BodyRoot) -> Result<PathBuf> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = self.path_in(root, TEMP_DIR);
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(format!(
            "{}-{}.part",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }

    /// Move the file at `src` to `rel` in `root`, replacing any body there. A
    /// rename where the filesystem allows it; otherwise a copy into the root's
    /// temp dir, renamed into place, then the source removed.
    fn move_into(&self, src: &Path, root: BodyRoot, rel: &str) -> Result<PathBuf> {
        let dest = self.path_in(root, rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::rename(src, &dest).is_err() {
            let tmp = self.temp_path(root)?;
            std::fs::copy(src, &tmp).with_context(|| format!("copying {}", src.display()))?;
            std::fs::rename(&tmp, &dest)?;
            std::fs::remove_file(src)?;
        }
        Ok(dest)
    }
}

/// The body lookup over two bare roots — [`OwnedTree::lookup_body`]'s rule,
/// for a reader that holds the roots and the set's state DB but not the
/// tree's engine (the share plane's serve half,
/// [`crate::share_body::OnDemandBodySource`]). `hydrated` answers whether
/// `rel`'s row is hydrated; it is asked only when the cache root holds a body.
pub fn body_path(
    kept: &Path,
    cache: &Path,
    rel: &str,
    hydrated: impl FnOnce() -> Result<bool>,
) -> Result<Option<PathBuf>> {
    let rel = check_rel(rel)?;
    let kept = kept.join(rel);
    if kept.is_file() {
        return Ok(Some(kept));
    }
    let cache = cache.join(rel);
    if cache.is_file() && hydrated()? {
        return Ok(Some(cache));
    }
    Ok(None)
}

/// A rel is a forward-slash path relative to the set's root, each component a
/// plain name. Anything else — reachable from another app through a document
/// id — is refused before it can name a path outside the roots.
fn check_rel(rel: &str) -> Result<&str> {
    let bad = rel.is_empty()
        || rel.contains(['\\', '\0'])
        || rel
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..");
    if bad {
        return Err(anyhow!("not a document path: {rel:?}"));
    }
    Ok(rel)
}

fn refuse_ignored<E: ProviderEngine>(engine: &E, rel: &str) -> Result<()> {
    if engine.is_ignored(rel) {
        bail!(
            "{} is excluded from sync",
            fauna_core::log_redact::log_path(rel)
        );
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Every regular file under the kept root as a rel, the temp dir excluded.
fn kept_rels(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
        };
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if prefix.is_empty() && name == TEMP_DIR {
                continue;
            }
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push((entry.path(), rel));
            } else if kind.is_file() {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::UploadOutcome;
    use crate::provider_face::{ProviderRow, ReadOnlyHost, content_version};
    use std::cell::Cell;
    use std::collections::BTreeMap;

    /// One engine row: its presence, the recorded head's content, and the
    /// recorded-head proof (set when the head's bytes sat at the engine root).
    #[derive(Clone)]
    struct Row {
        presence: RowPresence,
        head: Vec<u8>,
        recorded: Option<ContentHash>,
    }

    /// A fake engine that models rows, the nest's heads and the engine's root
    /// (the kept root) closely enough to prove the owned-tree mapping: ingest
    /// reads the kept-root body, `mark_hydrated` stamps the proof only when the
    /// body is at the root, the dehydration proof compares the disk.
    struct TreeFake {
        kept: PathBuf,
        rows: RefCell<BTreeMap<String, Row>>,
        record_ok: Cell<bool>,
        fetch_ok: Cell<bool>,
        /// A conflicted ingest re-points the row at the nest's head.
        conflict_moves_head: Cell<bool>,
        ignored: Vec<String>,
        /// Rels whose delete the nest does not record (a per-file
        /// `record_ok`, for a directory walk that stops part-way).
        unrecordable: RefCell<Vec<String>>,
        /// Rels whose kept-root body the share plane landed from a peer and
        /// the nest has not confirmed.
        provisional: RefCell<Vec<String>>,
        calls: RefCell<Vec<String>>,
        /// A reader's engine: no write half.
        read_only: Cell<bool>,
    }

    fn manifest_of(head: &[u8]) -> ContentHash {
        ContentHash::of_raw(&[b"manifest:".as_slice(), head].concat())
    }

    impl TreeFake {
        fn new(tree: &OwnedTree) -> Self {
            Self {
                kept: tree.kept_root().to_path_buf(),
                rows: RefCell::default(),
                record_ok: Cell::new(true),
                fetch_ok: Cell::new(true),
                conflict_moves_head: Cell::new(false),
                ignored: vec![],
                unrecordable: RefCell::default(),
                provisional: RefCell::default(),
                calls: RefCell::default(),
                read_only: Cell::new(false),
            }
        }
        fn with_row(self, rel: &str, presence: RowPresence, head: &[u8]) -> Self {
            self.rows.borrow_mut().insert(
                rel.into(),
                Row {
                    presence,
                    head: head.to_vec(),
                    recorded: None,
                },
            );
            self
        }
        fn presence(&self, rel: &str) -> RowPresence {
            self.rows
                .borrow()
                .get(rel)
                .map_or(RowPresence::Absent, |r| r.presence)
        }
        fn head(&self, rel: &str) -> Option<Vec<u8>> {
            self.rows.borrow().get(rel).map(|r| r.head.clone())
        }
        /// Another device records a new head (a refresh then re-points).
        fn remote_edit(&self, rel: &str, head: &[u8]) {
            let mut rows = self.rows.borrow_mut();
            let row = rows.get_mut(rel).unwrap();
            row.head = head.to_vec();
            row.recorded = None;
            row.presence = RowPresence::Placeholder;
        }
        fn calls(&self, prefix: &str) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .filter_map(|c| c.strip_prefix(prefix).map(str::to_owned))
                .collect()
        }
        fn record_local(&self, rel: &str) -> Result<UploadOutcome> {
            let bytes = std::fs::read(self.kept.join(rel))?;
            if !self.record_ok.get() {
                if let Some(row) = self.rows.borrow_mut().get_mut(rel) {
                    row.presence = RowPresence::Other;
                } else {
                    self.rows.borrow_mut().insert(
                        rel.into(),
                        Row {
                            presence: RowPresence::Other,
                            head: vec![],
                            recorded: None,
                        },
                    );
                }
                return Ok(UploadOutcome::default());
            }
            self.rows.borrow_mut().insert(
                rel.into(),
                Row {
                    presence: RowPresence::Hydrated,
                    recorded: Some(ContentHash::of_raw(&bytes)),
                    head: bytes,
                },
            );
            Ok(UploadOutcome {
                recorded: true,
                ..Default::default()
            })
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ProviderEngine for TreeFake {
        async fn provider_rows(&self) -> Result<Vec<ProviderRow>> {
            Ok(self
                .rows
                .borrow()
                .iter()
                .filter(|(_, r)| r.presence != RowPresence::Absent)
                .map(|(rel, r)| ProviderRow {
                    rel: rel.clone(),
                    size: r.head.len() as u64,
                    mtime: 0,
                    content_version: content_version(r.recorded, Some(manifest_of(&r.head))),
                })
                .collect())
        }
        async fn fetch_bytes(&self, rel: &str) -> Result<Vec<u8>> {
            self.head(rel).ok_or_else(|| anyhow!("no row"))
        }
        async fn fetch_to_path(&self, rel: &str, dest: &Path) -> Result<ContentHash> {
            self.calls.borrow_mut().push(format!("fetch:{rel}"));
            if !self.fetch_ok.get() {
                bail!("offline");
            }
            let head = self.head(rel).ok_or_else(|| anyhow!("no row"))?;
            std::fs::write(dest, &head)?;
            Ok(ContentHash::of_raw(&head))
        }
        fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> Result<()> {
            self.calls.borrow_mut().push(format!("hydrated:{rel}"));
            let at_root = self.kept.join(rel).is_file();
            if let Some(row) = self.rows.borrow_mut().get_mut(rel) {
                row.presence = RowPresence::Hydrated;
                if at_root {
                    row.recorded = Some(content_hash);
                }
            }
            Ok(())
        }
        fn mark_placeholder(&self, rel: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("placeholder:{rel}"));
            if let Some(row) = self.rows.borrow_mut().get_mut(rel) {
                row.presence = RowPresence::Placeholder;
            }
            Ok(())
        }
        fn hydrated_rels(&self) -> Result<Vec<String>> {
            Ok(self
                .rows
                .borrow()
                .iter()
                .filter(|(_, r)| r.presence == RowPresence::Hydrated)
                .map(|(rel, _)| rel.clone())
                .collect())
        }
        fn row_presence(&self, rel: &str) -> Result<RowPresence> {
            Ok(self.presence(rel))
        }
        fn is_dehydration_safe(&self, rel: &str) -> bool {
            let Ok(disk) = std::fs::read(self.kept.join(rel)) else {
                return false;
            };
            self.rows.borrow().get(rel).is_some_and(|r| {
                r.presence == RowPresence::Hydrated
                    && r.recorded == Some(ContentHash::of_raw(&disk))
            })
        }
        fn holds_provisional_peer_body(&self, rel: &str) -> Result<bool> {
            Ok(self.provisional.borrow().iter().any(|p| p == rel))
        }
        fn is_ignored(&self, rel: &str) -> bool {
            rel.split('/').any(|c| c.starts_with('.')) || self.ignored.iter().any(|i| i == rel)
        }
        async fn ingest(&self, rel: &str) -> Result<UploadOutcome> {
            self.calls.borrow_mut().push(format!("ingest:{rel}"));
            // The engine's recorded-head-proof skip.
            let disk = std::fs::read(self.kept.join(rel))?;
            if self.rows.borrow().get(rel).is_some_and(|r| {
                r.presence == RowPresence::Hydrated
                    && r.recorded == Some(ContentHash::of_raw(&disk))
            }) {
                return Ok(UploadOutcome {
                    recorded: true,
                    ..Default::default()
                });
            }
            self.calls.borrow_mut().push(format!("record:{rel}"));
            self.record_local(rel)
        }
        async fn ingest_conflicting(&self, rel: &str) -> Result<UploadOutcome> {
            self.calls.borrow_mut().push(format!("conflict:{rel}"));
            if !self.record_ok.get() {
                return Ok(UploadOutcome::default());
            }
            if self.conflict_moves_head.get() {
                // Incoming wins: the loser is retained on the nest, the row
                // re-points at the winner as a placeholder.
                self.rows.borrow_mut().get_mut(rel).unwrap().presence = RowPresence::Placeholder;
                return Ok(UploadOutcome {
                    recorded: true,
                    content_changed: true,
                });
            }
            self.record_local(rel)
        }
        async fn delete(&self, rel: &str) -> Result<UploadOutcome> {
            self.calls.borrow_mut().push(format!("delete:{rel}"));
            if !self.record_ok.get() || self.unrecordable.borrow().iter().any(|u| u == rel) {
                return Ok(UploadOutcome::default());
            }
            self.rows.borrow_mut().remove(rel);
            Ok(UploadOutcome {
                recorded: true,
                ..Default::default()
            })
        }
        fn read_only(&self) -> bool {
            self.read_only.get()
        }
    }

    fn tree(dir: &tempfile::TempDir) -> OwnedTree {
        OwnedTree::new(dir.path().join("kept"), dir.path().join("cache"))
    }

    /// A reader's tree: an open for read hydrates into the cache root, and
    /// every write — open for write, close, create, delete (a document and a
    /// directory), rename — is refused typed with nothing on disk moved and
    /// the engine never reached. The sweep ingests nothing, even a body that
    /// sits in the kept root, and still observes evictions.
    #[tokio::test]
    async fn a_readers_tree_reads_and_refuses_every_write() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        std::fs::create_dir_all(tree.kept_root()).unwrap();
        let engine = TreeFake::new(&tree)
            .with_row("a.txt", RowPresence::Placeholder, b"alpha")
            .with_row("dir/b.txt", RowPresence::Placeholder, b"beta")
            .with_row("gone.txt", RowPresence::Hydrated, b"x");
        engine.read_only.set(true);

        let body = tree.open_for_read(&engine, "a.txt").await.unwrap();
        assert_eq!(std::fs::read(&body).unwrap(), b"alpha");
        assert!(body.starts_with(tree.cache_root()));

        let refused = |e: anyhow::Error| e.downcast_ref::<ReadOnlyHost>().is_some();
        assert!(refused(
            tree.open_for_write(&engine, "a.txt").await.unwrap_err()
        ));
        assert!(refused(
            tree.closed_write(&engine, "a.txt", b"").await.unwrap_err()
        ));
        assert!(refused(tree.create(&engine, "new.txt").await.unwrap_err()));
        assert!(refused(tree.delete(&engine, "a.txt").await.unwrap_err()));
        assert!(refused(tree.delete(&engine, "dir").await.unwrap_err()));
        assert!(refused(
            tree.rename(&engine, "a.txt", "c.txt").await.unwrap_err()
        ));
        assert!(refused(
            tree.rename(&engine, "dir", "dir2").await.unwrap_err()
        ));

        assert!(body.is_file(), "the cached body is untouched");
        assert!(!tree.kept_root().join("a.txt").exists());
        assert!(!tree.kept_root().join("new.txt").exists());
        assert!(!tree.kept_root().join("c.txt").exists());
        for write in ["ingest:", "record:", "conflict:", "delete:"] {
            assert!(engine.calls(write).is_empty(), "{write} reached the engine");
        }

        std::fs::write(tree.kept_root().join("stray.txt"), b"s").unwrap();
        let report = tree.start_sweep(&engine).await.unwrap();
        assert!(report.recorded.is_empty() && report.pending.is_empty());
        assert_eq!(report.evicted, vec!["gone.txt".to_string()]);
        assert!(engine.calls("ingest:").is_empty());
        assert!(tree.kept_root().join("stray.txt").is_file());
    }

    fn hex_hash(bytes: &[u8]) -> String {
        hex::encode(ContentHash::of_raw(bytes).digest())
    }

    /// A body the share plane landed from a peer rests in the kept root (the
    /// nest cannot serve it again) but is not a write intent: the sweep never
    /// ingests it — recording it would publish another writer's file as this
    /// replica's own — and neither does a close that changed nothing.
    #[tokio::test]
    async fn a_peer_landed_body_is_kept_but_never_ingested() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        std::fs::create_dir_all(tree.kept_root()).unwrap();
        std::fs::write(tree.kept_root().join("cabin.txt"), b"from a peer").unwrap();
        let engine = TreeFake::new(&tree).with_row("cabin.txt", RowPresence::Hydrated, b"");
        engine.provisional.borrow_mut().push("cabin.txt".into());

        let report = tree.start_sweep(&engine).await.unwrap();
        assert_eq!(report.pending, ["cabin.txt"]);
        assert!(report.recorded.is_empty());
        assert!(
            engine.calls("ingest:").is_empty() && engine.calls("conflict:").is_empty(),
            "a peer's body is never uploaded as this replica's write"
        );
        assert_eq!(
            read(&tree.kept_root().join("cabin.txt")).as_deref(),
            Some(&b"from a peer"[..])
        );
    }

    /// The nest confirmed the peer-landed bytes: the body leaves the kept
    /// root for the cache root on the engine's own proof — and only on it.
    #[tokio::test]
    async fn a_confirmed_peer_body_is_demoted_on_the_engines_proof() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        std::fs::create_dir_all(tree.kept_root()).unwrap();
        std::fs::write(tree.kept_root().join("cabin.txt"), b"from a peer").unwrap();
        std::fs::write(tree.kept_root().join("unproven.txt"), b"no proof").unwrap();
        let engine = TreeFake::new(&tree)
            .with_row("cabin.txt", RowPresence::Hydrated, b"from a peer")
            .with_row("unproven.txt", RowPresence::Hydrated, b"no proof");
        engine
            .rows
            .borrow_mut()
            .get_mut("cabin.txt")
            .unwrap()
            .recorded = Some(ContentHash::of_raw(b"from a peer"));

        tree.settle_peer_bodies(
            &engine,
            &crate::engine::OverlayReconcile {
                confirmed: vec!["cabin.txt".into(), "unproven.txt".into(), "gone.txt".into()],
                superseded: vec![],
            },
        )
        .unwrap();

        assert_eq!(read(&tree.kept_root().join("cabin.txt")), None);
        assert_eq!(
            read(&tree.cache_root().join("cabin.txt")).as_deref(),
            Some(&b"from a peer"[..])
        );
        assert!(
            tree.kept_root().join("unproven.txt").is_file(),
            "no proof, no demotion"
        );
    }

    /// The nest recorded some other head for the path: the peer-landed body
    /// is dropped so the nest's row stands — unless the user has written over
    /// it since, which makes it a write intent like any other.
    #[tokio::test]
    async fn a_superseded_peer_body_is_dropped_unless_the_user_wrote_over_it() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        std::fs::create_dir_all(tree.kept_root()).unwrap();
        std::fs::write(tree.kept_root().join("lost.txt"), b"the peer's draft").unwrap();
        std::fs::write(tree.kept_root().join("edited.txt"), b"my own edit").unwrap();
        let engine = TreeFake::new(&tree);

        tree.settle_peer_bodies(
            &engine,
            &crate::engine::OverlayReconcile {
                confirmed: vec![],
                superseded: vec![
                    crate::engine::SupersededBody {
                        path: "lost.txt".into(),
                        content_hash: hex_hash(b"the peer's draft"),
                    },
                    crate::engine::SupersededBody {
                        path: "edited.txt".into(),
                        content_hash: hex_hash(b"what the peer landed"),
                    },
                    crate::engine::SupersededBody {
                        path: "../outside.txt".into(),
                        content_hash: hex_hash(b""),
                    },
                ],
            },
        )
        .unwrap();

        assert_eq!(read(&tree.kept_root().join("lost.txt")), None);
        assert_eq!(
            read(&tree.kept_root().join("edited.txt")).as_deref(),
            Some(&b"my own edit"[..]),
            "a body the user wrote over is a write intent — never dropped"
        );
    }

    fn read(path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    fn temp_is_empty(root: &Path) -> bool {
        std::fs::read_dir(root.join(TEMP_DIR)).map_or(true, |mut d| d.next().is_none())
    }

    // ---- paths ----

    /// The roots' ref component is the identifier's own ref half — one shared
    /// encoder, so a set's document id and its directories spell the ref alike.
    #[test]
    fn for_set_nests_account_then_the_identifiers_ref_component() {
        let set = fauna_core::folder_keys::FolderRef::Local(1).scoped_to([0xab; 32]);
        let tree = OwnedTree::for_set(Path::new("/files"), Path::new("/cache"), &set);
        let tail = format!("on-demand/{}/local%3A1", "ab".repeat(32));
        assert_eq!(tree.kept_root(), Path::new("/files").join(&tail));
        assert_eq!(tree.cache_root(), Path::new("/cache").join(&tail));
        let component = tree.kept_root().file_name().unwrap().to_str().unwrap();
        assert!(set.to_wire().starts_with(&format!("{component}@")));
    }

    #[tokio::test]
    async fn every_entry_point_refuses_a_rel_that_escapes_the_roots() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree);
        for bad in ["", "/abs", "../x", "a/../../x", "a//b", "./a", "a\\b", "a/"] {
            assert!(tree.open_for_read(&engine, bad).await.is_err(), "{bad:?}");
            assert!(tree.open_for_write(&engine, bad).await.is_err(), "{bad:?}");
            assert!(tree.create(&engine, bad).await.is_err(), "{bad:?}");
            assert!(tree.delete(&engine, bad).await.is_err(), "{bad:?}");
            assert!(tree.lookup_body(&engine, bad).is_err(), "{bad:?}");
            assert!(tree.record_placeholder(&engine, bad).is_err(), "{bad:?}");
        }
        assert!(
            engine.calls.borrow().is_empty(),
            "nothing reached the engine"
        );
        assert!(!dir.path().join("x").exists());
    }

    // ---- reads ----

    #[tokio::test]
    async fn open_for_read_hydrates_a_placeholder_into_the_cache_root() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("sub/a.txt", RowPresence::Placeholder, b"head");

        let path = tree.open_for_read(&engine, "sub/a.txt").await.unwrap();
        assert_eq!(path, tree.cache_root().join("sub/a.txt"));
        assert_eq!(read(&path).unwrap(), b"head");
        assert_eq!(engine.presence("sub/a.txt"), RowPresence::Hydrated);
        assert!(temp_is_empty(tree.cache_root()), "temp-then-rename");

        // A second open serves the cached body without fetching again.
        tree.open_for_read(&engine, "sub/a.txt").await.unwrap();
        assert_eq!(engine.calls("fetch:"), vec!["sub/a.txt"]);
    }

    #[tokio::test]
    async fn open_for_read_of_an_unknown_rel_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree);
        assert!(tree.open_for_read(&engine, "nope.txt").await.is_err());
        assert!(engine.calls("fetch:").is_empty());
    }

    #[tokio::test]
    async fn an_unfetchable_placeholder_fails_the_open_and_leaves_no_body() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        engine.fetch_ok.set(false);

        assert!(tree.open_for_read(&engine, "a.txt").await.is_err());
        assert!(
            !tree.cache_root().join("a.txt").exists(),
            "no stand-in body"
        );
        assert_eq!(engine.presence("a.txt"), RowPresence::Placeholder);
    }

    #[tokio::test]
    async fn a_kept_root_edit_is_read_before_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Hydrated, b"head");
        std::fs::create_dir_all(tree.cache_root()).unwrap();
        std::fs::create_dir_all(tree.kept_root()).unwrap();
        std::fs::write(tree.cache_root().join("a.txt"), b"head").unwrap();
        std::fs::write(tree.kept_root().join("a.txt"), b"edit").unwrap();

        let path = tree.open_for_read(&engine, "a.txt").await.unwrap();
        assert_eq!(read(&path).unwrap(), b"edit");
    }

    #[tokio::test]
    async fn a_cache_body_under_a_repointed_row_is_never_served() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"v1");
        tree.open_for_read(&engine, "a.txt").await.unwrap();

        engine.remote_edit("a.txt", b"v2");
        assert_eq!(tree.lookup_body(&engine, "a.txt").unwrap(), None);
        let path = tree.open_for_read(&engine, "a.txt").await.unwrap();
        assert_eq!(read(&path).unwrap(), b"v2", "the moved head is fetched");
    }

    // ---- writes ----

    #[tokio::test]
    async fn open_for_write_promotes_the_cached_body_and_hands_back_the_current_version() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        tree.open_for_read(&engine, "a.txt").await.unwrap();

        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        assert_eq!(open.path, tree.kept_root().join("a.txt"));
        assert_eq!(read(&open.path).unwrap(), b"head");
        assert!(
            !tree.cache_root().join("a.txt").exists(),
            "promoted, not copied"
        );
        let current = serve_item(&engine, "a.txt").await.unwrap().unwrap();
        assert_eq!(open.base_version, current.content_version);

        // An unchanged close records nothing new and demotes the body.
        let ack = tree
            .closed_write(&engine, "a.txt", &open.base_version)
            .await
            .unwrap();
        assert!(ack.acked);
        assert!(engine.calls("record:").is_empty(), "{:?}", engine.calls);
        assert!(engine.calls("conflict:").is_empty());
        assert_eq!(read(&tree.cache_root().join("a.txt")).unwrap(), b"head");
    }

    #[tokio::test]
    async fn open_for_write_of_a_placeholder_hydrates_straight_into_the_kept_root() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        assert_eq!(read(&open.path).unwrap(), b"head");
        assert!(temp_is_empty(tree.kept_root()));
        assert!(
            tree.open_for_write(&engine, "new.txt").await.is_err(),
            "create first"
        );
    }

    #[tokio::test]
    async fn a_recorded_close_uploads_and_demotes_the_body_to_the_cache_root() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        std::fs::write(&open.path, b"edited").unwrap();

        let ack = tree
            .closed_write(&engine, "a.txt", &open.base_version)
            .await
            .unwrap();
        assert!(ack.acked);
        assert_eq!(engine.head("a.txt").unwrap(), b"edited");
        assert!(
            !open.path.exists(),
            "the kept root holds only un-recorded edits"
        );
        assert_eq!(read(&tree.cache_root().join("a.txt")).unwrap(), b"edited");
        assert_eq!(
            tree.lookup_body(&engine, "a.txt").unwrap().unwrap(),
            tree.cache_root().join("a.txt")
        );
    }

    #[tokio::test]
    async fn an_unrecorded_close_keeps_the_body_in_the_kept_root() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        std::fs::write(&open.path, b"edited").unwrap();
        engine.record_ok.set(false);

        let ack = tree
            .closed_write(&engine, "a.txt", &open.base_version)
            .await
            .unwrap();
        assert!(!ack.acked);
        assert_eq!(read(&open.path).unwrap(), b"edited");
        assert!(!tree.cache_root().join("a.txt").exists());
    }

    #[tokio::test]
    async fn a_close_whose_conflict_moved_the_head_drops_the_local_body() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        std::fs::write(&open.path, b"mine").unwrap();
        // Another device records meanwhile, and a refresh re-points the row.
        engine.remote_edit("a.txt", b"theirs");
        engine.conflict_moves_head.set(true);

        let ack = tree
            .closed_write(&engine, "a.txt", &open.base_version)
            .await
            .unwrap();
        assert!(ack.acked && ack.content_changed);
        assert_eq!(engine.calls("conflict:"), vec!["a.txt"]);
        assert!(!open.path.exists());
        let path = tree.open_for_read(&engine, "a.txt").await.unwrap();
        assert_eq!(
            read(&path).unwrap(),
            b"theirs",
            "the next open fetches the winner"
        );
    }

    #[tokio::test]
    async fn a_moved_base_takes_the_conflict_arm_even_on_a_hydrated_row() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        std::fs::write(&open.path, b"mine").unwrap();
        let ack = tree
            .closed_write(&engine, "a.txt", b"some other version")
            .await
            .unwrap();
        assert!(ack.acked);
        assert_eq!(engine.calls("conflict:"), vec!["a.txt"]);
    }

    #[tokio::test]
    async fn the_body_stays_kept_until_the_last_writer_closes() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let first = tree.open_for_write(&engine, "a.txt").await.unwrap();
        let second = tree.open_for_write(&engine, "a.txt").await.unwrap();
        std::fs::write(&first.path, b"one").unwrap();

        tree.closed_write(&engine, "a.txt", &first.base_version)
            .await
            .unwrap();
        assert!(first.path.exists(), "a writer still holds it open");

        std::fs::write(&second.path, b"two").unwrap();
        let base = serve_item(&engine, "a.txt").await.unwrap().unwrap();
        tree.closed_write(&engine, "a.txt", &base.content_version)
            .await
            .unwrap();
        assert!(!second.path.exists());
        assert_eq!(engine.head("a.txt").unwrap(), b"two");
    }

    #[tokio::test]
    async fn create_ingests_an_empty_document_and_refuses_existing_or_ignored_names() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("docs/old.txt", RowPresence::Placeholder, b"x");

        let ack = tree.create(&engine, "docs/new.txt").await.unwrap();
        assert!(ack.acked);
        assert_eq!(engine.head("docs/new.txt").unwrap(), b"");
        assert!(tree.cache_root().join("docs/new.txt").is_file(), "demoted");

        assert!(tree.create(&engine, "docs/new.txt").await.is_err());
        assert!(tree.create(&engine, "docs/old.txt").await.is_err());
        assert!(tree.create(&engine, "docs").await.is_err(), "a directory");
        assert!(tree.create(&engine, ".hidden").await.is_err());
        assert!(!tree.kept_root().join(".hidden").exists());
    }

    #[tokio::test]
    async fn an_unrecorded_create_stays_in_the_kept_root_as_write_intent() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree);
        engine.record_ok.set(false);
        let ack = tree.create(&engine, "new.txt").await.unwrap();
        assert!(!ack.acked);
        assert!(tree.kept_root().join("new.txt").is_file());
    }

    #[tokio::test]
    async fn delete_is_record_first_and_an_unrecordable_one_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        tree.open_for_read(&engine, "a.txt").await.unwrap();

        engine.record_ok.set(false);
        assert!(tree.delete(&engine, "a.txt").await.is_err());
        assert!(tree.cache_root().join("a.txt").is_file(), "body untouched");
        assert_eq!(
            engine.presence("a.txt"),
            RowPresence::Hydrated,
            "row untouched"
        );

        engine.record_ok.set(true);
        tree.delete(&engine, "a.txt").await.unwrap();
        assert!(!tree.cache_root().join("a.txt").exists());
        assert_eq!(engine.presence("a.txt"), RowPresence::Absent);
    }

    #[tokio::test]
    async fn a_directory_is_deleted_file_by_file_each_record_first() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree)
            .with_row("d/a.txt", RowPresence::Placeholder, b"a")
            .with_row("d/sub/b.txt", RowPresence::Placeholder, b"b")
            .with_row("dx.txt", RowPresence::Placeholder, b"x");
        tree.open_for_read(&engine, "d/a.txt").await.unwrap();

        tree.delete(&engine, "d").await.unwrap();
        assert_eq!(engine.calls("delete:"), vec!["d/a.txt", "d/sub/b.txt"]);
        assert_eq!(engine.presence("d/a.txt"), RowPresence::Absent);
        assert_eq!(engine.presence("d/sub/b.txt"), RowPresence::Absent);
        assert!(!tree.cache_root().join("d/a.txt").exists());
        assert_eq!(
            engine.presence("dx.txt"),
            RowPresence::Placeholder,
            "a sibling sharing the prefix's letters is not under the directory"
        );
    }

    #[tokio::test]
    async fn a_directory_delete_stops_at_the_first_unrecordable_file_and_a_repeat_finishes_it() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree)
            .with_row("d/a.txt", RowPresence::Placeholder, b"a")
            .with_row("d/b.txt", RowPresence::Placeholder, b"b")
            .with_row("d/c.txt", RowPresence::Placeholder, b"c");
        engine.unrecordable.borrow_mut().push("d/b.txt".into());

        assert!(tree.delete(&engine, "d").await.is_err());
        assert_eq!(
            engine.presence("d/a.txt"),
            RowPresence::Absent,
            "recorded gone"
        );
        assert_eq!(
            engine.presence("d/b.txt"),
            RowPresence::Placeholder,
            "untouched"
        );
        assert_eq!(
            engine.presence("d/c.txt"),
            RowPresence::Placeholder,
            "never reached"
        );

        engine.unrecordable.borrow_mut().clear();
        tree.delete(&engine, "d").await.unwrap();
        assert_eq!(engine.presence("d/b.txt"), RowPresence::Absent);
        assert_eq!(engine.presence("d/c.txt"), RowPresence::Absent);
    }

    #[tokio::test]
    async fn a_directory_is_renamed_file_by_file_under_the_new_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree)
            .with_row("d/a.txt", RowPresence::Placeholder, b"a")
            .with_row("d/sub/b.txt", RowPresence::Placeholder, b"b");
        tree.open_for_read(&engine, "d/a.txt").await.unwrap();

        let ack = tree.rename(&engine, "d", "moved/e").await.unwrap();
        assert!(ack.acked);
        assert_eq!(engine.presence("d/a.txt"), RowPresence::Absent);
        assert_eq!(engine.presence("d/sub/b.txt"), RowPresence::Absent);
        assert_eq!(engine.head("moved/e/a.txt").unwrap(), b"a");
        assert_eq!(engine.head("moved/e/sub/b.txt").unwrap(), b"b");
        assert!(!tree.cache_root().join("d/a.txt").exists());
    }

    #[tokio::test]
    async fn a_directory_rename_stops_at_the_first_unrecordable_file_leaving_each_file_at_one_path()
    {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree)
            .with_row("d/a.txt", RowPresence::Placeholder, b"a")
            .with_row("d/b.txt", RowPresence::Placeholder, b"b");
        engine.unrecordable.borrow_mut().push("d/b.txt".into());

        assert!(tree.rename(&engine, "d", "e").await.is_err());
        assert_eq!(engine.head("e/a.txt").unwrap(), b"a");
        assert_eq!(engine.presence("d/a.txt"), RowPresence::Absent);
        assert_eq!(engine.presence("d/b.txt"), RowPresence::Placeholder);
        assert_eq!(engine.presence("e/b.txt"), RowPresence::Absent);
        assert!(
            !tree.kept_root().join("e/b.txt").exists(),
            "the body went back"
        );
    }

    #[tokio::test]
    async fn a_directory_is_never_moved_into_itself_or_onto_an_existing_name() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree)
            .with_row("d/a.txt", RowPresence::Placeholder, b"a")
            .with_row("e/z.txt", RowPresence::Placeholder, b"z")
            .with_row("f.txt", RowPresence::Placeholder, b"f");
        assert!(tree.rename(&engine, "d", "d/inner").await.is_err());
        assert!(tree.rename(&engine, "d", "e").await.is_err());
        assert!(tree.rename(&engine, "d", "f.txt").await.is_err());
        assert!(tree.rename(&engine, "d", ".hidden").await.is_err());
        assert!(engine.calls("delete:").is_empty());
    }

    #[tokio::test]
    async fn rename_moves_the_body_and_records_both_halves() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        tree.open_for_read(&engine, "a.txt").await.unwrap();

        let ack = tree.rename(&engine, "a.txt", "sub/b.txt").await.unwrap();
        assert!(ack.acked);
        assert_eq!(engine.presence("a.txt"), RowPresence::Absent);
        assert_eq!(engine.head("sub/b.txt").unwrap(), b"head");
        assert!(!tree.cache_root().join("a.txt").exists());
        assert_eq!(read(&tree.cache_root().join("sub/b.txt")).unwrap(), b"head");
    }

    #[tokio::test]
    async fn rename_of_a_placeholder_fetches_its_body_first() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let ack = tree.rename(&engine, "a.txt", "b.txt").await.unwrap();
        assert!(ack.acked);
        assert_eq!(engine.head("b.txt").unwrap(), b"head");
    }

    #[tokio::test]
    async fn an_unrecordable_rename_puts_the_body_back_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        tree.open_for_read(&engine, "a.txt").await.unwrap();
        engine.record_ok.set(false);

        assert!(tree.rename(&engine, "a.txt", "b.txt").await.is_err());
        assert_eq!(read(&tree.cache_root().join("a.txt")).unwrap(), b"head");
        assert!(!tree.kept_root().join("b.txt").exists());
        assert!(
            engine.calls("ingest:").is_empty(),
            "the new path is never ingested"
        );
        assert_eq!(engine.presence("a.txt"), RowPresence::Hydrated);

        // A placeholder's fetched body is discarded again.
        let engine2 = TreeFake::new(&tree).with_row("p.txt", RowPresence::Placeholder, b"x");
        engine2.record_ok.set(false);
        assert!(tree.rename(&engine2, "p.txt", "q.txt").await.is_err());
        assert!(!tree.kept_root().join("q.txt").exists());
        assert!(!tree.kept_root().join("p.txt").exists());
    }

    #[tokio::test]
    async fn rename_refuses_an_existing_or_ignored_target() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree)
            .with_row("a.txt", RowPresence::Placeholder, b"a")
            .with_row("b.txt", RowPresence::Placeholder, b"b");
        assert!(tree.rename(&engine, "a.txt", "b.txt").await.is_err());
        assert!(tree.rename(&engine, "a.txt", ".b.txt").await.is_err());
        assert!(engine.calls("delete:").is_empty());
    }

    #[tokio::test]
    async fn a_writer_released_without_a_close_is_swept_and_a_held_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"old");
        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        std::fs::write(&open.path, b"held at the close").unwrap();

        let report = tree.start_sweep(&engine).await.unwrap();
        assert!(
            report.recorded.is_empty() && report.pending.is_empty(),
            "a body still open for write is never swept"
        );

        // The host refused the close before serving it (offline, no signer).
        tree.release_writer("a.txt");
        let report = tree.start_sweep(&engine).await.unwrap();
        assert_eq!(report.recorded, vec!["a.txt".to_string()]);
        assert_eq!(engine.head("a.txt").unwrap(), b"held at the close");
    }

    // ---- start and refresh ----

    #[tokio::test]
    async fn the_start_sweep_ingests_kept_bodies_and_discards_half_written_temps() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree);
        std::fs::create_dir_all(tree.kept_root().join("sub")).unwrap();
        std::fs::create_dir_all(tree.kept_root().join(TEMP_DIR)).unwrap();
        std::fs::create_dir_all(tree.cache_root().join(TEMP_DIR)).unwrap();
        std::fs::write(tree.kept_root().join("sub/new.txt"), b"new").unwrap();
        std::fs::write(tree.kept_root().join(".DS_Store"), b"litter").unwrap();
        std::fs::write(tree.kept_root().join(TEMP_DIR).join("1-0.part"), b"half").unwrap();
        std::fs::write(tree.cache_root().join(TEMP_DIR).join("1-1.part"), b"half").unwrap();

        let report = tree.start_sweep(&engine).await.unwrap();
        assert_eq!(report.recorded, vec!["sub/new.txt"]);
        assert!(report.pending.is_empty());
        assert_eq!(engine.head("sub/new.txt").unwrap(), b"new");
        assert_eq!(
            read(&tree.cache_root().join("sub/new.txt")).unwrap(),
            b"new"
        );
        assert!(temp_is_empty(tree.kept_root()) && temp_is_empty(tree.cache_root()));
        assert!(
            tree.kept_root().join(".DS_Store").is_file(),
            "an ignored body is never ingested, never removed"
        );
        assert!(engine.calls("ingest:.").is_empty());

        // Idempotent.
        let again = tree.start_sweep(&engine).await.unwrap();
        assert!(again.recorded.is_empty() && again.pending.is_empty());
    }

    #[tokio::test]
    async fn an_offline_sweep_leaves_the_body_pending() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree);
        std::fs::create_dir_all(tree.kept_root()).unwrap();
        std::fs::write(tree.kept_root().join("a.txt"), b"a").unwrap();
        engine.record_ok.set(false);
        let report = tree.start_sweep(&engine).await.unwrap();
        assert_eq!(report.pending, vec!["a.txt"]);
        assert!(tree.kept_root().join("a.txt").is_file());
    }

    #[tokio::test]
    async fn a_kept_body_under_a_repointed_row_sweeps_through_the_conflict_arm() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        std::fs::write(&open.path, b"mine").unwrap();
        // The process dies before the close; a refresh then re-points the row.
        drop(tree);
        engine.remote_edit("a.txt", b"theirs");

        let tree = self::tree(&dir);
        tree.start_sweep(&engine).await.unwrap();
        assert_eq!(engine.calls("conflict:"), vec!["a.txt"]);
        assert!(
            engine.calls("record:").is_empty(),
            "never a last-writer-wins upload"
        );
    }

    #[tokio::test]
    async fn observe_evictions_flips_bodiless_hydrated_rows_and_never_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree)
            .with_row("a.txt", RowPresence::Placeholder, b"a")
            .with_row("b.txt", RowPresence::Placeholder, b"b");
        tree.open_for_read(&engine, "a.txt").await.unwrap();
        tree.open_for_read(&engine, "b.txt").await.unwrap();
        std::fs::remove_file(tree.cache_root().join("a.txt")).unwrap();

        assert_eq!(
            tree.observe_evictions(&engine).await.unwrap(),
            vec!["a.txt"]
        );
        assert_eq!(engine.presence("a.txt"), RowPresence::Placeholder);
        assert_eq!(engine.presence("b.txt"), RowPresence::Hydrated);
        assert!(engine.calls("delete:").is_empty());
    }

    // ---- the peer-transfer seam ----

    #[tokio::test]
    async fn the_seam_looks_up_lands_and_records_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree)
            .with_row("a.txt", RowPresence::Placeholder, b"a")
            .with_row("b.txt", RowPresence::Placeholder, b"b");
        assert_eq!(tree.lookup_body(&engine, "a.txt").unwrap(), None);

        // A body the nest holds lands in the cache root and hydrates the row.
        let src = dir.path().join("pulled-a");
        std::fs::write(&src, b"a").unwrap();
        let landed = tree
            .land_body(&engine, "a.txt", &src, BodyRoot::Cache)
            .unwrap();
        assert_eq!(landed, tree.cache_root().join("a.txt"));
        assert!(!src.exists(), "moved");
        assert_eq!(engine.presence("a.txt"), RowPresence::Hydrated);
        assert_eq!(tree.lookup_body(&engine, "a.txt").unwrap(), Some(landed));

        // One the nest may not hold lands kept, and the sweep uploads it.
        let src = dir.path().join("pulled-c");
        std::fs::write(&src, b"c").unwrap();
        tree.land_body(&engine, "c.txt", &src, BodyRoot::Kept)
            .unwrap();
        assert_eq!(
            tree.lookup_body(&engine, "c.txt").unwrap(),
            Some(tree.kept_root().join("c.txt"))
        );
        tree.start_sweep(&engine).await.unwrap();
        assert_eq!(engine.head("c.txt").unwrap(), b"c");

        // Recording a placeholder drops the cache body, never a kept one.
        tree.record_placeholder(&engine, "a.txt").unwrap();
        assert_eq!(engine.presence("a.txt"), RowPresence::Placeholder);
        assert!(!tree.cache_root().join("a.txt").exists());
        std::fs::create_dir_all(tree.kept_root()).unwrap();
        std::fs::write(tree.kept_root().join("b.txt"), b"edit").unwrap();
        tree.record_placeholder(&engine, "b.txt").unwrap();
        assert!(tree.kept_root().join("b.txt").is_file());
    }

    // ---- the row's end-to-end flow ----

    /// Open for write → the body is promoted to the kept root → the process
    /// dies → a new host's start sweep finds the body → it is ingested and
    /// recorded → the body is demoted to the cache root → the OS deletes the
    /// cache file → eviction observation flips the row to a placeholder → and
    /// no delete ever reaches the nest.
    #[tokio::test]
    async fn a_killed_writers_edit_survives_and_a_reclaimed_body_is_never_a_delete() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&dir);
        let engine = TreeFake::new(&tree).with_row("a.txt", RowPresence::Placeholder, b"head");
        let open = tree.open_for_write(&engine, "a.txt").await.unwrap();
        assert_eq!(open.path, tree.kept_root().join("a.txt"));
        std::fs::write(&open.path, b"edited before the kill").unwrap();
        drop(tree); // no close: the process died

        let tree = self::tree(&dir);
        let report = tree.start_sweep(&engine).await.unwrap();
        assert_eq!(report.recorded, vec!["a.txt"]);
        assert_eq!(engine.head("a.txt").unwrap(), b"edited before the kill");
        let cached = tree.cache_root().join("a.txt");
        assert!(!tree.kept_root().join("a.txt").exists());
        assert!(cached.is_file());

        std::fs::remove_file(&cached).unwrap(); // the OS trims the cache
        assert_eq!(
            tree.observe_evictions(&engine).await.unwrap(),
            vec!["a.txt"]
        );
        assert_eq!(engine.presence("a.txt"), RowPresence::Placeholder);
        assert!(engine.calls("delete:").is_empty());
    }
}
