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

use std::sync::{Arc, RwLock, Weak};

use fauna_sync_engine::account_runtime::PeerFileSync;
use fauna_sync_engine::relay_seat::RelaySeat;
use fauna_sync_engine::sibling_chunks::SiblingChannels;

/// See the module docs.
pub struct PeerFiles {
    siblings: Arc<SiblingChannels>,
    seat: Arc<RwLock<Weak<RelaySeat>>>,
}

impl PeerFiles {
    pub fn new() -> Self {
        Self {
            siblings: SiblingChannels::new(),
            seat: Arc::new(RwLock::new(Weak::new())),
        }
    }

    /// The registry every engine of this agent asks before the nest.
    pub fn siblings(&self) -> Arc<SiblingChannels> {
        Arc::clone(&self.siblings)
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
            siblings: Arc::clone(&self.siblings),
        }
    }
}

impl Default for PeerFiles {
    fn default() -> Self {
        Self::new()
    }
}
