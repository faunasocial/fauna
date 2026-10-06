//! The shared-folder content-key custody door over a seat's account runtime
//! (`fauna_client_folders::{FolderKeyReader, FolderKeyStore}`) — the one impl
//! every runtime-hosting app hands the custody writers (`FoldersAuthor`, the
//! custody-ingest sink, the key resolver, the set lifecycle, the engine
//! build), web's core chunk included (it serves the folders and media chunks
//! through the account port, `fauna_client_folders::port`).
//!
//! The custody is the account plane's `fauna.state.folder-keys` kind, one row
//! per entity (`config-dissolution.md` — the kinds table's row;
//! `fauna_account_plane::folder_key_rows` owns the door). The kind was cut
//! plane-only: there is no second rail behind this seam, so an absent
//! runtime is an error, never a fallback.
//!
//! **An absent runtime.** A read answers `Err` at once — "custody unreadable":
//! a bound set builds keyless and a key lookup answers unresolved, never
//! plaintext. A write waits for the handle as the blessing door's does
//! ([`crate::blessed_nests::WRITE_WAIT_POLLS`] × `WRITE_WAIT_POLL`) — a share
//! or a received key can land while the seat's runtime is still assembling —
//! then refuses; and so does the read a write starts from
//! (`FolderKeyStore::load_for_write`).

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_client_folders::{FolderKeyReader, FolderKeyStore};
use fauna_core::data::{FolderPendingRemoval, FoldersConfig};

/// What an absent runtime answers.
const RUNTIME_ABSENT: &str = "the account runtime is not running";

/// [`FolderKeyStore`] over a seat's account runtime. `source` is the seat's
/// own fresh read of its live handle — `AccountRuntimeHost::handle` on linux,
/// the `fauna-ffi` seat, the `App`-owned slot on tui, and on web the core
/// chunk's `account_runtime::handle_for` — exactly as
/// [`crate::blessed_nests::PlaneBlessedNests`] takes it.
pub struct PlaneFolderKeys<S> {
    source: S,
}

impl<S> PlaneFolderKeys<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    pub fn new(source: S) -> Self {
        Self { source }
    }

    /// The handle, waiting out an assembly still in flight.
    async fn handle_for_write(&self) -> anyhow::Result<AccountStoreHandle> {
        crate::blessed_nests::wait_for_handle(&self.source)
            .await
            .ok_or_else(|| anyhow::anyhow!(RUNTIME_ABSENT))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<S> FolderKeyReader for PlaneFolderKeys<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    async fn load(&self) -> anyhow::Result<FoldersConfig> {
        let handle = (self.source)().ok_or_else(|| anyhow::anyhow!(RUNTIME_ABSENT))?;
        handle.folder_keys().await
    }

    /// The store's replica-local meta row — device-local, never synced, and
    /// read by every process that mounts this device's store.
    async fn adoption_markers(&self) -> anyhow::Result<Vec<[u8; 32]>> {
        let handle = (self.source)().ok_or_else(|| anyhow::anyhow!(RUNTIME_ABSENT))?;
        let raw = handle
            .meta_get(fauna_client_folders::ADOPTION_MARKERS_META_KEY)
            .await?;
        fauna_client_folders::decode_adoption_markers(raw.as_deref())
    }

    /// One replica-local meta row per set, as the adoption markers' is.
    async fn refetch_requested_at(&self, channel_id: &[u8; 32]) -> anyhow::Result<Option<u64>> {
        let handle = (self.source)().ok_or_else(|| anyhow::anyhow!(RUNTIME_ABSENT))?;
        let raw = handle
            .meta_get(fauna_client_folders::refetch_request_meta_key(channel_id))
            .await?;
        Ok(fauna_client_folders::decode_refetch_request(raw.as_deref()))
    }

    /// The mail custody's MSEK, then its retained prior generations — read
    /// fresh each pass, so mail enabled or keys rotated after the engine was
    /// built reach the next adoption.
    async fn recipient_mseks(&self) -> anyhow::Result<Vec<fauna_core::secret::SecretArray32>> {
        let handle = (self.source)().ok_or_else(|| anyhow::anyhow!(RUNTIME_ABSENT))?;
        let mail = handle.mail().await?;
        Ok(mail.msek.into_iter().chain(mail.prior_mseks).collect())
    }

    /// A plain put — the set's engine is the key's only writer. No wait for a
    /// runtime still assembling: the request is best-effort, and the engine
    /// that makes it was built over a store already mounted.
    async fn request_refetch(&self, channel_id: &[u8; 32], now_micros: u64) -> anyhow::Result<()> {
        let handle = (self.source)().ok_or_else(|| anyhow::anyhow!(RUNTIME_ABSENT))?;
        handle
            .meta_put(
                fauna_client_folders::refetch_request_meta_key(channel_id),
                fauna_client_folders::encode_refetch_request(now_micros),
            )
            .await
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<S> FolderKeyStore for PlaneFolderKeys<S>
where
    S: Fn() -> Option<AccountStoreHandle> + Send + Sync,
{
    /// The read a write starts from waits for the handle as the write does —
    /// a set keyed or a received key ingested right after sign-in would
    /// otherwise fail on its own read before its write ever ran.
    async fn load_for_write(&self) -> anyhow::Result<FoldersConfig> {
        self.handle_for_write().await?.folder_keys().await
    }

    async fn merge(&self, replica: FoldersConfig) -> anyhow::Result<FoldersConfig> {
        self.handle_for_write()
            .await?
            .merge_folder_keys(replica)
            .await
    }

    async fn settle_removal(&self, removal: FolderPendingRemoval) -> anyhow::Result<FoldersConfig> {
        self.handle_for_write()
            .await?
            .settle_folder_removal(removal)
            .await
    }

    /// One replica-local meta row holds the list; the owner's reconcile is
    /// its only writer, so the read-modify-write races nothing.
    async fn record_adoption_marker(&self, replaced: [u8; 32]) -> anyhow::Result<()> {
        let handle = self.handle_for_write().await?;
        let key = fauna_client_folders::ADOPTION_MARKERS_META_KEY;
        let mut markers =
            fauna_client_folders::decode_adoption_markers(handle.meta_get(key).await?.as_deref())?;
        if markers.contains(&replaced) {
            return Ok(());
        }
        markers.push(replaced);
        handle
            .meta_put(
                key,
                fauna_client_folders::encode_adoption_markers(&markers)?,
            )
            .await
    }

    /// The identity the runtime serves — its succession-ledger seat.
    async fn minting_identity(&self) -> anyhow::Result<Option<fauna_core::identity::ActorId>> {
        use fauna_client_config::SuccessionLedgerStore as _;
        let handle = self.handle_for_write().await?;
        Ok(handle.self_actor().ok())
    }

    /// The runtime's store-change notice ([`crate::store_change`]): it wakes
    /// once a walk has landed — this runtime's own pump's or a co-located
    /// process's — so a custody nudge's re-read follows the rows it carried.
    /// `None` when the runtime never came up.
    async fn change_notices(&self) -> Option<Box<dyn fauna_client_folders::CustodyNotices>> {
        let handle = self.handle_for_write().await.ok()?;
        Some(Box::new(StoreChangeNotices(
            crate::store_change::StoreChangeWatch::new(handle).await,
        )))
    }
}

/// [`crate::store_change::StoreChangeWatch`] as the custody seam's notice
/// source.
struct StoreChangeNotices(crate::store_change::StoreChangeWatch);

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_folders::CustodyNotices for StoreChangeNotices {
    async fn changed(&mut self) -> bool {
        self.0.changed().await
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// With no runtime a read is refused at once — never an empty custody a
    /// build would read as "no key held".
    #[tokio::test]
    async fn an_absent_runtime_is_an_unreadable_custody_never_an_empty_one() {
        let seam = PlaneFolderKeys::new(|| None);
        let err = seam.load().await.expect_err("no runtime, no custody");
        assert!(err.to_string().contains(RUNTIME_ABSENT), "{err:#}");
    }
}
