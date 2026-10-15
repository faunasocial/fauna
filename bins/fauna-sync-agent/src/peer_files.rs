//! The same-account peer data plane's two file halves on this host
//! (`docs/goal/behavior/p2p.md` § Goal; `file-sync.md` § Content residency —
//! seats fetch content seat↔seat over the account plane's peer leg).
//!
//! The agent hosts both the account runtime, whose peer leg dials and admits
//! this account's other devices, and the file-sync engines, which hold the
//! file bodies. This joins them, once per agent:
//!
//! - **Pull:** the sibling registry the peer leg's dial pass fills and every
//!   engine asks before the nest (`SyncEngine::with_sibling_chunks`).
//! - **Serve:** the peer leg's chunk door reaches the current engine host's
//!   relay seat, which hands the want to the folder's engine — the one serve
//!   core (`RelaySeat::serve_peer`). The seat is per host and a host is rebuilt
//!   on re-provision, so the door reads whichever seat is current, weakly: a
//!   host torn down serves nothing.
//! - **Folder rows (nest-down):** the peer leg's row and manifest doors read a
//!   folder's held rows and retained manifests from the folder's own state DB
//!   — a second WAL connection per request, the read the share pump makes
//!   across a process boundary, in-process here (`p2p.md` § Same-account
//!   nest-down folder transfer). Each running engine registers its folder's
//!   DB ([`FolderDbs`]) for as long as it runs; a folder no engine here runs
//!   answers an empty page or `missing`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use fauna_sync_engine::account_runtime::PeerFileSync;
use fauna_sync_engine::peer_share_store::SyncDbShareStore;
use fauna_sync_engine::relay_seat::RelaySeat;
use fauna_sync_engine::sibling_chunks::SiblingChannels;

/// See the module docs.
pub struct PeerFiles {
    siblings: Arc<SiblingChannels>,
    seat: Arc<RwLock<Weak<RelaySeat>>>,
    folder_dbs: Arc<FolderDbs>,
}

/// The running engines' state DBs by folder (`FolderRef` wire string) — what
/// the folder row and manifest doors read. An engine's [`FolderDbGuard`]
/// unregisters it when the engine stops; a replacement engine for the same
/// folder registers over it, and the stale guard's drop leaves the newer entry.
#[derive(Default)]
pub struct FolderDbs {
    folders: Mutex<HashMap<String, (u64, PathBuf)>>,
    next: AtomicU64,
}

impl FolderDbs {
    /// Register `folder`'s state DB until the returned guard drops.
    pub fn register(self: &Arc<Self>, folder: String, db_path: PathBuf) -> FolderDbGuard {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.folders
            .lock()
            .expect("folder dbs")
            .insert(folder.clone(), (id, db_path));
        FolderDbGuard {
            dbs: Arc::downgrade(self),
            folder,
            id,
        }
    }

    fn db_path(&self, folder: &str) -> Option<PathBuf> {
        self.folders
            .lock()
            .expect("folder dbs")
            .get(folder)
            .map(|(_, path)| path.clone())
    }

    /// The folder's held rows above `since`, pending rows on the tail — an
    /// empty page for a folder no engine here runs.
    async fn rows(
        &self,
        folder: &str,
        since: i64,
        max_rows: u32,
    ) -> anyhow::Result<Vec<fauna_protocol::peer_row::LocalShareChange>> {
        let Some(path) = self.db_path(folder) else {
            return Ok(Vec::new());
        };
        tokio::task::spawn_blocking(move || {
            let db = fauna_sync_engine::db::SyncDb::open(&path)?;
            SyncDbShareStore::new([0u8; 32], db).rows_since(since, max_rows)
        })
        .await?
    }

    /// One retained manifest's self-verified bytes, or `None`.
    async fn manifest(&self, folder: &str, hash: [u8; 32]) -> Option<Vec<u8>> {
        let path = self.db_path(folder)?;
        tokio::task::spawn_blocking(move || {
            let db = fauna_sync_engine::db::SyncDb::open(&path).ok()?;
            SyncDbShareStore::new([0u8; 32], db).retained_manifest_bytes(&hash)
        })
        .await
        .ok()?
    }
}

/// Holds one engine's [`FolderDbs`] entry; dropping it unregisters the entry
/// unless a newer engine for the folder replaced it.
pub struct FolderDbGuard {
    dbs: Weak<FolderDbs>,
    folder: String,
    id: u64,
}

impl Drop for FolderDbGuard {
    fn drop(&mut self) {
        if let Some(dbs) = self.dbs.upgrade() {
            let mut folders = dbs.folders.lock().expect("folder dbs");
            if folders
                .get(&self.folder)
                .is_some_and(|(id, _)| *id == self.id)
            {
                folders.remove(&self.folder);
            }
        }
    }
}

impl PeerFiles {
    pub fn new() -> Self {
        Self {
            siblings: SiblingChannels::new(),
            seat: Arc::new(RwLock::new(Weak::new())),
            folder_dbs: Arc::new(FolderDbs::default()),
        }
    }

    /// The registry every running engine of this agent puts its state DB in.
    pub fn folder_dbs(&self) -> Arc<FolderDbs> {
        Arc::clone(&self.folder_dbs)
    }

    /// The registry every engine of this agent asks before the nest.
    pub fn siblings(&self) -> Arc<SiblingChannels> {
        Arc::clone(&self.siblings)
    }

    /// The plane's reading for `GetServiceStatus` — an in-memory read.
    pub fn status(&self) -> fauna_ipc::sync::PeerFilesInfo {
        let tally = self.siblings.tally();
        fauna_ipc::sync::PeerFilesInfo {
            siblings: u32::try_from(self.siblings.len()).unwrap_or(u32::MAX),
            chunks: tally.chunks,
            bytes: tally.bytes,
            to_nest: tally.to_nest,
        }
    }

    /// Point the serve door at a newly started engine host's seat.
    pub fn set_seat(&self, seat: &Arc<RelaySeat>) {
        *self.seat.write().expect("peer files seat") = Arc::downgrade(seat);
    }

    /// The peer leg's half, for the account runtime's transport factory.
    pub fn binding(&self) -> PeerFileSync {
        let seat = Arc::clone(&self.seat);
        PeerFileSync {
            file_chunks: Arc::new(move |folder: String, store_key: [u8; 32]| {
                let seat = seat.read().expect("peer files seat").upgrade();
                Box::pin(async move {
                    match seat {
                        Some(seat) => seat.serve_peer(&folder, store_key).await,
                        None => None,
                    }
                })
            }),
            folder_changes: {
                let dbs = Arc::clone(&self.folder_dbs);
                Arc::new(move |folder: String, since: i64, max_rows: u32| {
                    let dbs = Arc::clone(&dbs);
                    Box::pin(async move { dbs.rows(&folder, since, max_rows).await })
                })
            },
            folder_manifests: {
                let dbs = Arc::clone(&self.folder_dbs);
                Arc::new(move |folder: String, hash: [u8; 32]| {
                    let dbs = Arc::clone(&dbs);
                    Box::pin(async move { dbs.manifest(&folder, hash).await })
                })
            },
            siblings: Arc::clone(&self.siblings),
        }
    }
}

impl Default for PeerFiles {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The folder doors read the registered folder's DB; a folder no engine
    /// runs, or one whose engine stopped, answers an empty page — and a stale
    /// guard never unregisters its replacement.
    #[tokio::test]
    async fn the_folder_doors_read_the_running_engines_db_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("sync.db");
        let db = fauna_sync_engine::db::SyncDb::open(&db_path).unwrap();
        db.mint_pending_own_change(&fauna_sync_engine::db::OwnChangeRow {
            seq: None,
            path: "a.txt".into(),
            path_hash: "ab".repeat(32),
            change_type: "create".into(),
            ..Default::default()
        })
        .unwrap();
        drop(db);
        let files = PeerFiles::new();
        let binding = files.binding();
        assert!(
            (binding.folder_changes)("7".into(), 0, 64)
                .await
                .unwrap()
                .is_empty(),
            "no engine runs folder 7 yet"
        );
        let first = files.folder_dbs().register("7".into(), db_path.clone());
        let rows = (binding.folder_changes)("7".into(), 0, 64).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].sequenced && rows[0].locally_authored);
        assert!(
            (binding.folder_changes)("8".into(), 0, 64)
                .await
                .unwrap()
                .is_empty()
        );

        let second = files.folder_dbs().register("7".into(), db_path);
        drop(first);
        assert_eq!(
            (binding.folder_changes)("7".into(), 0, 64)
                .await
                .unwrap()
                .len(),
            1,
            "the replacement engine stays registered"
        );
        drop(second);
        assert!(
            (binding.folder_changes)("7".into(), 0, 64)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            (binding.folder_manifests)("7".into(), [1; 32])
                .await
                .is_none()
        );
    }
}
