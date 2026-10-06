//! The **body source** — where the engine's one serve core
//! ([`crate::serve_core`]) reads a file's plaintext, for the share leg and the
//! nest relay alike (`p2p-shared-set-build.md` § *Phone peers — design*,
//! decision 1: *the serve half reads a body through a source, never a bare
//! path*; `file-sync.md` § Relay serving).
//!
//! A serving replica re-derives each sealed chunk from a plaintext **range**
//! of the file it holds ([`crate::serve_core::serve_held_chunk`]).
//! Where that file lives differs per replica, and one replica has no path at
//! all, so the serve half asks a source rather than joining a directory:
//!
//! - [`TreeBodySource`] — a bound local tree (the desktops; what the serve
//!   half did before this seam existed, byte for byte);
//! - [`OnDemandBodySource`] — an on-demand replica that owns its tree: the
//!   kept root, then the cache root, else none
//!   ([`crate::provider_face::owned_tree`]);
//! - a descriptor the shell opens for a path (a phone's library originals,
//!   decision 4) — a third implementation, not built; the trait is cut so it
//!   needs no change to its callers: both questions are answerable from an
//!   open descriptor.
//!
//! **A source never fetches.** A body this device does not hold answers
//! `None` and the puller turns to another member: a peer's request spends
//! neither this device's data allowance nor its storage.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::db::{SyncDb, SyncState};

/// Where one set's bodies are read from. Every failure is the same honest
/// answer — *this replica cannot produce it* — so both reads return `None`
/// rather than an error.
#[async_trait::async_trait]
pub trait BodySource: Send + Sync + std::fmt::Debug {
    /// The length of `path`'s body, or `None` when this replica holds none.
    async fn body_len(&self, path: &str) -> Option<u64>;

    /// Exactly `len` bytes of `path`'s body at `offset`. `None` when this
    /// replica holds no body for the path, or the body is shorter than the
    /// range (it changed since the range was recorded).
    async fn read_range(&self, path: &str, offset: u64, len: u64) -> Option<Vec<u8>>;
}

async fn file_len(full_path: &Path) -> Option<u64> {
    match tokio::fs::metadata(full_path).await {
        Ok(m) if m.is_file() => Some(m.len()),
        _ => None,
    }
}

async fn file_range(full_path: &Path, offset: u64, len: u64) -> Option<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = tokio::fs::File::open(full_path).await.ok()?;
    file.seek(std::io::SeekFrom::Start(offset)).await.ok()?;
    let mut plain = vec![0u8; usize::try_from(len).ok()?];
    file.read_exact(&mut plain).await.ok()?;
    Some(plain)
}

/// A bound local tree: `path` is read at `root/path`. On an on-demand root
/// (cfapi today) a cloud-only placeholder stands at the path with its bytes
/// elsewhere; it answers `None` — the provider's own read of its own
/// placeholder would block for cfapi's full timeout, and serving must never
/// hydrate on an asker's behalf ([`crate::placeholder`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeBodySource {
    root: PathBuf,
}

impl TreeBodySource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The source as a spec carries it.
    pub fn shared(root: impl Into<PathBuf>) -> std::sync::Arc<dyn BodySource> {
        std::sync::Arc::new(Self::new(root.into()))
    }
}

#[async_trait::async_trait]
impl BodySource for TreeBodySource {
    async fn body_len(&self, path: &str) -> Option<u64> {
        file_len(&self.local_body(path)?).await
    }

    async fn read_range(&self, path: &str, offset: u64, len: u64) -> Option<Vec<u8>> {
        file_range(&self.local_body(path)?, offset, len).await
    }
}

impl TreeBodySource {
    /// `root/path`, or `None` when it is a cloud-only placeholder.
    fn local_body(&self, path: &str) -> Option<PathBuf> {
        let full = self.root.join(path);
        (!crate::placeholder::path_is_cloud_placeholder(&full)).then_some(full)
    }
}

/// An on-demand replica that owns its tree: the kept root, then the cache
/// root, else none — a placeholder.
///
/// The lookup is the owned tree's own rule
/// ([`crate::provider_face::owned_tree::body_path`]): a cache-root body counts
/// only while its row is hydrated. The row is read through a second
/// connection to the set's state DB — the cross-process WAL read the pump
/// already makes for its cursor — so the serve half never queues behind the
/// replica's one writer, and the same source serves a replica hosted in
/// another process.
pub struct OnDemandBodySource {
    kept: PathBuf,
    cache: PathBuf,
    // `SyncDb` wraps a `rusqlite::Connection`, which is `Send` but not `Sync`.
    db: Mutex<SyncDb>,
}

impl OnDemandBodySource {
    /// A source over the replica's two roots and `db`, a read connection to
    /// the set's state DB.
    pub fn new(kept: PathBuf, cache: PathBuf, db: SyncDb) -> Self {
        Self {
            kept,
            cache,
            db: Mutex::new(db),
        }
    }

    /// The body's path on this device, or `None` for a placeholder.
    fn locate(&self, path: &str) -> Option<PathBuf> {
        crate::provider_face::owned_tree::body_path(&self.kept, &self.cache, path, || {
            let db = self.db.lock().expect("body-source db mutex poisoned");
            Ok(db
                .get_entry(path)?
                .is_some_and(|e| e.state == SyncState::Synced))
        })
        .ok()
        .flatten()
    }
}

impl std::fmt::Debug for OnDemandBodySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnDemandBodySource")
            .field("kept", &self.kept)
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl BodySource for OnDemandBodySource {
    async fn body_len(&self, path: &str) -> Option<u64> {
        file_len(&self.locate(path)?).await
    }

    async fn read_range(&self, path: &str, offset: u64, len: u64) -> Option<Vec<u8>> {
        file_range(&self.locate(path)?, offset, len).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bound tree reads what the serve half read before the seam: the
    /// range at `root/path`, and none for a missing file or a range past its
    /// end.
    #[tokio::test]
    async fn the_tree_source_reads_a_range_and_answers_none_past_the_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.txt"), b"0123456789").unwrap();
        let source = TreeBodySource::new(dir.path().to_path_buf());

        assert_eq!(source.body_len("sub/a.txt").await, Some(10));
        assert_eq!(
            source.read_range("sub/a.txt", 3, 4).await.as_deref(),
            Some(&b"3456"[..])
        );
        assert_eq!(source.read_range("sub/a.txt", 8, 4).await, None, "shrunk");
        assert_eq!(source.body_len("missing.txt").await, None);
        assert_eq!(source.read_range("missing.txt", 0, 1).await, None);
        assert_eq!(source.body_len("sub").await, None, "a directory is no body");
    }

    fn row(db: &SyncDb, rel: &str, state: SyncState) {
        db.upsert_entry(
            rel,
            None,
            None,
            Some(fauna_core::data::ContentHash::of_raw(rel.as_bytes())),
            state,
            0,
            0,
            4,
            1,
            Some(1),
        )
        .unwrap();
    }

    /// The on-demand replica's lookup: the kept root wins, the cache root
    /// counts only under a hydrated row, and a placeholder answers none —
    /// the honest partial seed.
    #[tokio::test]
    async fn the_on_demand_source_reads_kept_then_cache_and_none_for_a_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, cache) = (dir.path().join("kept"), dir.path().join("cache"));
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let db_path = dir.path().join("fsid.db");
        let db = SyncDb::open(&db_path).unwrap();
        row(&db, "edited.txt", SyncState::Synced);
        row(&db, "cached.txt", SyncState::Synced);
        row(&db, "repointed.txt", SyncState::Placeholder);
        row(&db, "cloud.txt", SyncState::Placeholder);
        std::fs::write(kept.join("edited.txt"), b"kept").unwrap();
        std::fs::write(cache.join("edited.txt"), b"old!").unwrap();
        std::fs::write(cache.join("cached.txt"), b"cash").unwrap();
        // A cache body under a row the nest's head moved away from.
        std::fs::write(cache.join("repointed.txt"), b"gone").unwrap();

        let source =
            OnDemandBodySource::new(kept.clone(), cache.clone(), SyncDb::open(&db_path).unwrap());

        assert_eq!(
            source.read_range("edited.txt", 0, 4).await.as_deref(),
            Some(&b"kept"[..]),
            "the kept root is read first"
        );
        assert_eq!(
            source.read_range("cached.txt", 1, 2).await.as_deref(),
            Some(&b"as"[..])
        );
        assert_eq!(
            source.read_range("repointed.txt", 0, 4).await,
            None,
            "a cache body under a placeholder row is the superseded version"
        );
        assert_eq!(source.body_len("cloud.txt").await, None);
        assert_eq!(source.read_range("cloud.txt", 0, 1).await, None);
        assert_eq!(
            source.read_range("../fsid.db", 0, 1).await,
            None,
            "a path that leaves the roots names no body"
        );
    }
}
