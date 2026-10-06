//! The **period-key custody** over a seat's account runtime
//! (`fauna_client_subscriptions::PeriodKeyStore`; `key-material-hierarchy.md`
//! § Audience: an opaque set of subscriber pubkeys → *Client-minted at-rest
//! custody*) — the one impl every runtime-hosting app hands its custody
//! consumers (the author pump and the Tiers tab, the feed's gated compose and
//! unlock, the trust facet's grant mint, the archive import), web included.
//!
//! The keys are the account plane's `fauna.state.subscriptions` rows
//! (`config-dissolution.md` — the kinds table's row;
//! `fauna_account_plane::subscription_rows` owns the door, and the handle
//! implements the seam itself). The kind was born plane-only: there is no
//! second rail behind this seam, so an absent runtime is an error,
//! never an empty custody — reading "no key" where a key exists is how a fresh
//! key gets minted over a live one.
//!
//! **An absent runtime** is the blessing door's rule
//! ([`crate::blessed_nests`]): a read answers `Err` at once (the author pump
//! retries on its next tick, a surface shows the error); a write — a tier
//! created, a removal staged, a sold post's key recorded, possibly before the
//! seat's runtime has assembled — waits for the handle, then refuses; and so
//! does the read a write starts from (`PeriodKeyStore::custody_for_write`).

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_client_config::StoreError;
use fauna_client_subscriptions::PeriodKeyStore;
use fauna_core::data::{PendingRemoval, SubscriptionsConfig};

/// What an absent runtime answers.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// [`PeriodKeyStore`] over a seat's account runtime. `source` is the seat's
/// own fresh read of its live handle — `account_runtime::handle` on linux, the
/// `fauna-ffi` seat and web, the settings-owned slot on tui — exactly as
/// [`crate::blessed_nests::PlaneBlessedNests`] takes it, so one store built at
/// sign-in follows the runtime through assembly and teardown.
pub struct PlanePeriodKeys<S> {
    source: S,
}

impl<S> PlanePeriodKeys<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    pub fn new(source: S) -> Self {
        Self { source }
    }

    fn handle_for_read(&self) -> Result<AccountStoreHandle, StoreError> {
        (self.source)().ok_or_else(|| StoreError::Load(RUNTIME_ABSENT.into()))
    }

    async fn handle_for_write(&self) -> Result<AccountStoreHandle, StoreError> {
        crate::blessed_nests::wait_for_handle(&self.source)
            .await
            .ok_or_else(|| StoreError::Save(RUNTIME_ABSENT.into()))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<S> PeriodKeyStore for PlanePeriodKeys<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    async fn custody(&self) -> Result<SubscriptionsConfig, StoreError> {
        let handle = self.handle_for_read()?;
        PeriodKeyStore::custody(&handle).await
    }

    /// The read a write starts from waits for the handle as the write does —
    /// a tier created right after sign-in would otherwise fail on its own
    /// read before its write ever ran.
    async fn custody_for_write(&self) -> Result<SubscriptionsConfig, StoreError> {
        let handle = crate::blessed_nests::wait_for_handle(&self.source)
            .await
            .ok_or_else(|| StoreError::Load(RUNTIME_ABSENT.into()))?;
        PeriodKeyStore::custody(&handle).await
    }

    async fn merge_custody(
        &self,
        replica: SubscriptionsConfig,
    ) -> Result<SubscriptionsConfig, StoreError> {
        let handle = self.handle_for_write().await?;
        PeriodKeyStore::merge_custody(&handle, replica).await
    }

    async fn settle_removal(
        &self,
        removal: PendingRemoval,
    ) -> Result<SubscriptionsConfig, StoreError> {
        let handle = self.handle_for_write().await?;
        PeriodKeyStore::settle_removal(&handle, removal).await
    }
}
