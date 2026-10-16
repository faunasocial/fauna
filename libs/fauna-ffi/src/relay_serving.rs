//! UniFFI host for **relay serving on an on-demand replica** — a phone's
//! provider (`docs/goal/behavior/file-sync.md` § Relay serving → *Which seats
//! hold*, the on-demand rows): the app announces, on its own WS-RPC
//! connection, every set whose on-demand host this process holds, and answers
//! the nest's asks through each host's engine — the one serve core, reading
//! the owned tree's bodies (`p2p-shared-set-build.md` § *Phone peers —
//! design*, decision 1).
//!
//! A phone is a **foreground** seat (decision 2): its shell calls
//! [`FfiNestClient::start_relay_serving`] where it starts the share plane and
//! [`FfiNestClient::stop_relay_serving`] where it stops it, and the stop
//! withdraws the announce, so a backgrounded phone is asked for nothing.
//!
//! Its own pair of calls, not a part of the share plane: relay serving is not
//! an excisable feature (`file-sync.md` § Relay serving → *The seat serves
//! from the file*), and the plane is (`p2p-share`). Everything app-agnostic —
//! the announce, the routing, the host tracking, the withdrawal — is
//! `fauna_sync_engine::relay_seat`'s.

use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;

use fauna_client::NestClient;
use fauna_sync_engine::relay_seat::{HOST_RESCAN, RelaySeat};

use crate::FfiError;
use crate::crypto::bytes32;
use crate::nest_client::FfiNestClient;

/// The running seat: its task, and what its withdrawal needs.
struct RunningSeat {
    task: JoinHandle<()>,
    nest: Arc<NestClient>,
    device_id_hex: String,
}

/// This process's relay seat for on-demand hosts, once started.
static SEAT: Mutex<Option<RunningSeat>> = Mutex::new(None);

/// Stop `slot`'s seat: end its task, then withdraw the connection's announce.
/// A no-op with no seat running.
async fn stop_seat(slot: &Mutex<Option<RunningSeat>>) {
    let stopped = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(seat) = stopped else {
        return;
    };
    seat.task.abort();
    let _ = seat.task.await;
    if let Err(e) = fauna_sync_engine::relay_seat::withdraw(&seat.nest, &seat.device_id_hex).await {
        // The connection's close withdraws it as well; a stop that cannot
        // reach the nest leaves only that.
        tracing::info!(error = %e, "relay serving: the withdrawing announce did not reach the nest");
    }
}

/// Forget the seat without a withdrawal — the account runtime's teardown,
/// ahead of the connection's own close.
pub(crate) fn forget() {
    if let Some(seat) = SEAT.lock().unwrap_or_else(|e| e.into_inner()).take() {
        seat.task.abort();
    }
}

#[fauna_uniffi_async::export]
impl FfiNestClient {
    /// Start serving the nest's relay from this process's on-demand hosts for
    /// `actor_id` — every set whose host the provider holds, announced on
    /// this connection as `device_id` (the sync device the hosts run as, one
    /// of the account's registered devices). Hosts built or dropped later are
    /// announced or withdrawn within a rescan. Starting again replaces the
    /// running seat.
    pub async fn start_relay_serving(
        &self,
        actor_id: Vec<u8>,
        device_id: Vec<u8>,
    ) -> Result<(), FfiError> {
        let actor_id = bytes32(&actor_id, "actor_id")?;
        let device_id_hex = hex::encode(bytes32(&device_id, "device_id")?);
        stop_seat(&SEAT).await;
        let nest = self.nest_arc();
        let task = {
            let (nest, device_id_hex) = (Arc::clone(&nest), device_id_hex.clone());
            tokio::spawn(async move {
                RelaySeat::new()
                    .serve_hosts(
                        &nest,
                        &device_id_hex,
                        crate::file_provider_host::relay_hosts_for(actor_id),
                        HOST_RESCAN,
                    )
                    .await
            })
        };
        let raced = SEAT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(RunningSeat {
                task,
                nest,
                device_id_hex,
            });
        // A start that raced this one in between: the later seat stands alone,
        // and its first announce replaces the other's whole.
        if let Some(raced) = raced {
            raced.task.abort();
        }
        Ok(())
    }

    /// Stop serving the relay and withdraw this connection's announce — what
    /// a phone's shell calls when the app leaves the foreground
    /// (`p2p-shared-set-build.md` § *Phone peers — design*, decision 2). A
    /// no-op with no seat running.
    pub async fn stop_relay_serving(&self) {
        stop_seat(&SEAT).await;
    }
}
