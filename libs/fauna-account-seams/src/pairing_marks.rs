//! The Nests page's pairing-mark door over a seat's account runtime
//! (`fauna_client_pair::PairingMarkStore`; `account-runtime.md`
//! § Multi-instance concurrency → *A nest-side change a store-backed page
//! shows leaves a mark in the store: the pairing mark*) — the one impl every
//! runtime-hosting app wires into its `LinkedNestsMachine`, beside the
//! blessing door ([`crate::blessed_nests`]), so a link or an unlink on one
//! seat reaches every other seat's open Nests page.
//!
//! The marks are the account plane's `fauna.state.pairing-mark` kind, one row
//! per nest (`fauna_account_plane::pairing_mark_rows` owns the door).
//!
//! **An absent runtime refuses at once.** Unlike the blessing toggle, the
//! mark is best-effort and the account runtime's secondary leg writes it
//! anyway; a gesture never waits out an assembly for it.

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_client_pair::PairingMarkStore;

/// What an absent runtime answers.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// [`PairingMarkStore`] over a seat's account runtime. `source` is the seat's
/// own fresh read of its live handle, exactly as
/// [`crate::blessed_nests::PlaneBlessedNests`] takes it.
pub struct PlanePairingMarks<S> {
    source: S,
}

impl<S> PlanePairingMarks<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    pub fn new(source: S) -> Self {
        Self { source }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<S> PairingMarkStore for PlanePairingMarks<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    async fn put_mark(&self, nest_id: &[u8; 32], digest: [u8; 32]) -> Result<bool, String> {
        let handle = (self.source)().ok_or_else(|| RUNTIME_ABSENT.to_string())?;
        handle
            .put_pairing_mark(*nest_id, digest)
            .await
            .map_err(|e| format!("{e:#}"))
    }
}
