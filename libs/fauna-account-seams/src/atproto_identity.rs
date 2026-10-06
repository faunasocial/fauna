//! The ATProto identity seam over a seat's account runtime
//! (`fauna_client_atproto::identity_store::AtprotoIdentityStore`) — the one
//! impl every runtime-hosting app wires into its ATProto settings machine and
//! its session-start alert sweep, web included (the core chunk directly, the
//! settings chunk through the account port: `fauna_client_atproto::port`).
//!
//! The custody is the account plane's `fauna.state.atproto-identity` kind,
//! one row per rotation key, consent, contest intent and nest-named DID
//! (`config-dissolution.md` — the kinds table's row;
//! `fauna_account_plane::atproto_identity_rows` owns the door). The kind was
//! born plane-only: there is no second rail behind this seam, so an
//! absent runtime is an error, never a fallback.
//!
//! **An absent runtime.** A read answers `Err` at once — every custody check
//! reads that as "cannot verify", quiet, retried on a later pass. A write —
//! the mint, which the enable gesture can reach before the seat's runtime has
//! assembled — waits for the handle for up to [`WRITE_WAIT_POLLS`] ×
//! [`WRITE_WAIT_POLL`], then refuses (the blessed-nests seam's rule).
//!
//! **The readiness edge (`until_readable`)** polls the same handle at the same
//! cadence, unbounded, and then awaits the runtime's prologue
//! ([`AccountStoreHandle::settled`]). A handle alone is not enough: the
//! runtime is up at assembly, but its first catch-up pass runs after that, so
//! a replica the sign-out dropped reads an empty ring until the pass has
//! pulled the custody back — and an empty ring is a custody check with
//! nothing to compare, not a refusal anyone retries. Its one caller, the alert
//! sweep, races the edge against its own re-sweep clock to re-run the custody
//! check a first pass could not (`critical-alerts.md` § Mechanism → *How often
//! the detector runs*).

use std::time::Duration;

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_core::data::AtprotoIdentityConfig;

/// The seam this module serves, re-exported so a host names the trait and its
/// one impl through one path.
pub use fauna_client_atproto::identity_store::AtprotoIdentityStore;

/// What an absent runtime answers.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// How often a write re-reads the seat's handle while the runtime assembles.
pub const WRITE_WAIT_POLL: Duration = Duration::from_millis(250);

/// How many polls a write waits for the runtime before it refuses (~30 s).
pub const WRITE_WAIT_POLLS: u32 = 120;

/// [`AtprotoIdentityStore`] over a seat's account runtime. `source` is the
/// seat's own fresh read of its live handle — `AccountRuntimeHost::handle` on
/// linux and the `fauna-ffi` seat, the `App`-owned slot on tui, and on web the
/// core chunk's `account_runtime::handle_for` the caller's account.
pub struct RuntimeAtprotoIdentity<S> {
    source: S,
}

impl<S> RuntimeAtprotoIdentity<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    pub fn new(source: S) -> Self {
        Self { source }
    }

    /// The handle, waiting out an assembly still in flight.
    async fn handle_for_write(&self) -> Result<AccountStoreHandle, String> {
        for _ in 0..WRITE_WAIT_POLLS {
            if let Some(handle) = (self.source)() {
                return Ok(handle);
            }
            fauna_sleep::sleep(WRITE_WAIT_POLL).await;
        }
        (self.source)().ok_or_else(|| RUNTIME_ABSENT.to_string())
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<S> AtprotoIdentityStore for RuntimeAtprotoIdentity<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    async fn atproto_identity(&self) -> Result<AtprotoIdentityConfig, String> {
        let handle = (self.source)().ok_or_else(|| RUNTIME_ABSENT.to_string())?;
        handle
            .atproto_identity()
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn merge_atproto_identity(
        &self,
        replica: AtprotoIdentityConfig,
    ) -> Result<AtprotoIdentityConfig, String> {
        self.handle_for_write()
            .await?
            .merge_atproto_identity(replica)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn until_readable(&self) {
        let handle = loop {
            if let Some(handle) = (self.source)() {
                break handle;
            }
            fauna_sleep::sleep(WRITE_WAIT_POLL).await;
        };
        handle.settled().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no runtime the custody read is refused at once — never answered
    /// "no key held", which a custody check would read as an empty ring and
    /// a mint as licence to generate a fresh senior key for every retry.
    #[tokio::test]
    async fn an_absent_runtime_refuses_the_read_rather_than_answering_an_empty_ring() {
        let door = RuntimeAtprotoIdentity::new(|| None);
        assert_eq!(
            door.atproto_identity().await,
            Err(RUNTIME_ABSENT.to_string())
        );
    }

    /// The readiness edge stays pending while the seat has no runtime, and
    /// keeps re-reading the seat's handle rather than reading it once — the
    /// edge the alert sweep re-runs its custody check at. (Resolving on a
    /// present handle needs a live runtime; the sweep's own loop tests pin
    /// what the edge's resolution drives.)
    #[tokio::test]
    async fn the_readable_edge_waits_while_the_runtime_is_absent() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let reads = std::sync::Arc::new(AtomicU32::new(0));
        let seen = std::sync::Arc::clone(&reads);
        let door = RuntimeAtprotoIdentity::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            None
        });
        // The crate's one cross-target sleep, never tokio's timer (the
        // `no_native_time` pin reads test modules too).
        let still_pending = tokio::select! {
            () = door.until_readable() => false,
            () = fauna_sleep::sleep(WRITE_WAIT_POLL * 3) => true,
        };
        assert!(
            still_pending,
            "an absent runtime must keep the edge pending"
        );
        assert!(
            reads.load(Ordering::SeqCst) >= 2,
            "the edge re-reads the seat's handle while it waits"
        );
    }
}
