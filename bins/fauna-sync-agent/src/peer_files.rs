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
    bound: BoundLegRecorder,
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    serve_fault: Arc<ServeFault>,
}

/// **Test-only** (`testing.md` convention 15): the serve door's fault arm —
/// what `RequestMethod::ArmPeerServeFault` arms. Armed with `n`, the door
/// forges the next `n` distinct chunk bodies it answers by flipping one byte,
/// and keeps forging each of those keys the same way at every later want (a
/// ranged body is re-read per slice, so a forgery that changed between slices
/// would be a different fault). The witness of *"a tampered piece from a
/// sibling is refused"* (`p2p.md` § Wormability posture, rule 4) needs a
/// sibling that serves one, and no real seat ever does.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Default)]
pub struct ServeFault {
    state: Mutex<ServeFaultState>,
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Default)]
struct ServeFaultState {
    remaining: u32,
    forged: std::collections::HashSet<[u8; 32]>,
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl ServeFault {
    /// Forge the next `bodies` distinct bodies served (adds to what is armed).
    pub fn arm(&self, bodies: u32) {
        let mut state = self.state.lock().expect("serve fault");
        state.remaining = state.remaining.saturating_add(bodies);
    }

    /// `body` as the door answers it under `store_key`.
    fn apply(&self, store_key: [u8; 32], mut body: Vec<u8>) -> Vec<u8> {
        let mut state = self.state.lock().expect("serve fault");
        let forge = if state.forged.contains(&store_key) {
            true
        } else if state.remaining > 0 && !body.is_empty() {
            state.remaining -= 1;
            state.forged.insert(store_key);
            true
        } else {
            false
        };
        if forge && let Some(first) = body.first_mut() {
            *first ^= 0x01;
        }
        body
    }
}

/// What the account runtime's transport factory last bound on this agent —
/// written at each bind, read by the e2e probe arm (`crate::peer_probe`) to
/// dial as this device outside the runtime's passes.
///
/// The transport is held **weakly**: the runtime's node owns the endpoint and
/// dropping it closes the listener (wormability rule 5, no listener when off),
/// so a record must never keep a torn-down endpoint alive. A dead reference
/// reads as "not bound".
#[derive(Clone, Default)]
pub struct BoundLegRecorder(Arc<Mutex<Option<BoundLeg>>>);

#[derive(Clone)]
#[cfg_attr(
    not(any(test, debug_assertions, feature = "test-helpers")),
    allow(dead_code)
)]
struct BoundLeg {
    transport: Weak<dyn fauna_transport::PeerTransport>,
    bound_addrs: Vec<std::net::SocketAddr>,
    node_id: [u8; 32],
    device_authorization: fauna_core::encoding::EmbedAsBytes,
}

/// The bound leg as the probe arm uses it — the live transport, where it
/// listens, and the witness it admits under.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub struct LiveLeg {
    pub transport: Arc<dyn fauna_transport::PeerTransport>,
    pub bound_addrs: Vec<std::net::SocketAddr>,
    pub node_id: [u8; 32],
    pub device_authorization: fauna_core::encoding::EmbedAsBytes,
}

impl BoundLegRecorder {
    /// Record a bind (the factory's act, once per bind).
    pub fn record(
        &self,
        transport: &Arc<dyn fauna_transport::PeerTransport>,
        bound_addrs: &[std::net::SocketAddr],
        node_id: [u8; 32],
        device_authorization: fauna_core::encoding::EmbedAsBytes,
    ) {
        *self.0.lock().expect("bound leg") = Some(BoundLeg {
            transport: Arc::downgrade(transport),
            bound_addrs: bound_addrs.to_vec(),
            node_id,
            device_authorization,
        });
    }

    /// The leg bound now, or `None` when none was, or its endpoint is gone.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn live(&self) -> Option<LiveLeg> {
        let held = self.0.lock().expect("bound leg").clone()?;
        Some(LiveLeg {
            transport: held.transport.upgrade()?,
            bound_addrs: held.bound_addrs,
            node_id: held.node_id,
            device_authorization: held.device_authorization,
        })
    }
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

    /// The chunk store keys this agent's engine indexed for `relative_path` in
    /// `folder`, in file order — empty for a folder no engine here runs. The
    /// e2e probe arm's way to name a file's chunks (`crate::peer_probe`).
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub async fn held_store_keys(
        &self,
        folder: &str,
        relative_path: &str,
    ) -> anyhow::Result<Vec<[u8; 32]>> {
        let Some(path) = self.db_path(folder) else {
            return Ok(Vec::new());
        };
        let relative_path = relative_path.to_string();
        tokio::task::spawn_blocking(move || {
            fauna_sync_engine::db::SyncDb::open(&path)?.held_store_keys(&relative_path)
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
            bound: BoundLegRecorder::default(),
            #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
            serve_fault: Arc::default(),
        }
    }

    /// The serve door's fault arm ([`ServeFault`]).
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn serve_fault(&self) -> &ServeFault {
        &self.serve_fault
    }

    /// Where the transport factory records each bind ([`BoundLegRecorder`]).
    pub fn bound_leg(&self) -> BoundLegRecorder {
        self.bound.clone()
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
            refused: tally.refused,
        }
    }

    /// Point the serve door at a newly started engine host's seat.
    pub fn set_seat(&self, seat: &Arc<RelaySeat>) {
        *self.seat.write().expect("peer files seat") = Arc::downgrade(seat);
    }

    /// The peer leg's half, for the account runtime's transport factory.
    pub fn binding(&self) -> PeerFileSync {
        let seat = Arc::clone(&self.seat);
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        let serve_fault = Arc::clone(&self.serve_fault);
        PeerFileSync {
            file_chunks: Arc::new(move |folder: String, store_key: [u8; 32]| {
                let seat = seat.read().expect("peer files seat").upgrade();
                // What the door answers: the body as served, or — armed in a
                // test build only — its forgery ([`ServeFault`]).
                #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
                let answer = {
                    let serve_fault = Arc::clone(&serve_fault);
                    move |body: Option<Vec<u8>>| body.map(|b| serve_fault.apply(store_key, b))
                };
                #[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
                let answer = |body: Option<Vec<u8>>| body;
                Box::pin(async move {
                    answer(match seat {
                        Some(seat) => seat.serve_peer(&folder, store_key).await,
                        None => None,
                    })
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

    /// The fault arm forges exactly the next `n` distinct keys, the same way
    /// at every later want of them, and nothing else.
    #[test]
    fn the_serve_fault_forges_the_next_bodies_consistently_and_nothing_else() {
        let fault = ServeFault::default();
        let body = vec![7u8; 16];
        assert_eq!(fault.apply([1; 32], body.clone()), body, "unarmed");
        fault.arm(1);
        let forged = fault.apply([2; 32], body.clone());
        assert_ne!(forged, body);
        assert_eq!(forged[1..], body[1..], "one byte flipped");
        assert_eq!(fault.apply([3; 32], body.clone()), body, "only one armed");
        assert_eq!(
            fault.apply([2; 32], body.clone()),
            forged,
            "the same forgery at every later slice of the key"
        );
    }

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
