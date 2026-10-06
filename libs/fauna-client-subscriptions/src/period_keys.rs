//! The **period-key store seam** — where the author's tier period-key custody
//! lives: the account plane's `fauna.state.subscriptions` kind, one row per
//! period key and one per staged removal (`config-dissolution.md` § The
//! `__config` dissolution schedule, the kinds table's row; `key-material-
//! hierarchy.md` § Audience: an opaque set of subscriber pubkeys →
//! *Encrypted-mode at-rest custody* owns what the keys are).
//!
//! Born plane-only: no other copy sits behind this seam. Every consumer — the mint
//! orchestration, the feed's compose and unlock legs, the grant mint, the
//! post-succession rotation — reads and writes through one
//! `Arc<dyn PeriodKeyStore>`, implemented once, directly on the account
//! runtime's handle (`fauna_account_plane::account_driver`), so no app writes
//! glue beyond handing the handle in.
//!
//! The seam's three verbs are the door's, and they carry its two laws:
//!
//! - **A write is a join, never a replace.** [`PeriodKeyStore::merge_custody`]
//!   splits the caller's replica into its records and joins each into its row,
//!   so a replica that omits a key — a stale one, a fresh device's empty one —
//!   drops nothing, and the caller gets back the account's fold as it now
//!   stands, another device's periods included.
//! - **Nothing is ever deleted.** A staged removal leaves the fold only by
//!   [`PeriodKeyStore::settle_removal`], which writes its fresh period as a
//!   period row before marking the removal settled, so no settle strands a
//!   key. What the blob rewrote in place, the plane re-records and the fold
//!   reads as one period (`SubscriptionsConfig::fold_row` — *one mint reads as
//!   one period*).

use std::sync::Arc;

use fauna_client_config::StoreError;
use fauna_core::data::{PendingRemoval, SubscriptionsConfig};
use fauna_protocol::MaybeSendSync;

/// The author's period-key custody store — see the module docs. Native boxes
/// `Send` futures; wasm's `Rc`-based handle yields `!Send`, so the wasm arm is
/// `async_trait(?Send)` (the same split as the other `fauna_client_config` store seams).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait PeriodKeyStore: MaybeSendSync {
    /// The account's custody, folded — empty when nothing is stored. A store
    /// that cannot be read (the account runtime is not up yet, a row that
    /// will not decode) is an error, never an empty custody: reading "no key"
    /// where a key exists is how a fresh key gets minted over a live blob.
    async fn custody(&self) -> Result<SubscriptionsConfig, StoreError>;

    /// [`Self::custody`] read as the first half of a write — a read-join-put
    /// (a tier's first period recorded, a removal staged). A store whose writes
    /// wait out a runtime still assembling (`fauna_account_seams::period_keys`)
    /// waits here too, so a gesture made right after sign-in lands instead of
    /// failing on its own read; the answer is still the custody or an error,
    /// never an empty custody. Defaults to [`Self::custody`], for a store that
    /// is always up.
    async fn custody_for_write(&self) -> Result<SubscriptionsConfig, StoreError> {
        self.custody().await
    }

    /// Join every record of `replica` into the store and return the account's
    /// custody as it now stands.
    async fn merge_custody(
        &self,
        replica: SubscriptionsConfig,
    ) -> Result<SubscriptionsConfig, StoreError>;

    /// Settle one staged removal — its fresh period written as a period row,
    /// then the removal marked settled — and return the custody as it now
    /// stands. Idempotent.
    async fn settle_removal(
        &self,
        removal: PendingRemoval,
    ) -> Result<SubscriptionsConfig, StoreError>;
}

/// The shared form every consumer holds.
pub type SharedPeriodKeyStore = Arc<dyn PeriodKeyStore>;

/// An in-memory [`PeriodKeyStore`] with the door's exact semantics — one
/// canonical row per record, a per-row join on write
/// (`SubscriptionsRow::merge`), nothing ever deleted, the read the same fold —
/// so a consumer's tests exercise the rules the production door enforces.
/// `Clone` shares the rows: two clones are one account's store, the way two
/// seams over one handle are.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Default)]
pub struct MemoryPeriodKeyStore {
    rows: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>>,
}

/// A [`MemoryPeriodKeyStore`]'s rows at one moment ([`MemoryPeriodKeyStore::snapshot`]).
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeriodKeyRows(std::collections::BTreeMap<String, Vec<u8>>);

#[cfg(any(test, feature = "test-helpers"))]
impl MemoryPeriodKeyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The store as a [`SharedPeriodKeyStore`].
    pub fn shared(&self) -> SharedPeriodKeyStore {
        Arc::new(self.clone())
    }

    /// The number of stored rows — every record ever written, settled
    /// removals and re-stamped periods included.
    pub fn row_count(&self) -> usize {
        self.rows.lock().unwrap().len()
    }

    /// The rows this store holds now — to hand to [`Self::restore`] later.
    pub fn snapshot(&self) -> PeriodKeyRows {
        PeriodKeyRows(self.rows.lock().unwrap().clone())
    }

    /// Make this store hold exactly `rows` — a **lagging replica**: the
    /// device whose walk has not yet delivered what another device wrote since
    /// `rows` was taken (restoring an older snapshot), or whose walk just
    /// did (restoring a newer one). The one way a test models a read that
    /// raced a sibling's write; the production door has no such verb.
    pub fn restore(&self, rows: PeriodKeyRows) {
        *self.rows.lock().unwrap() = rows.0;
    }

    /// Make this store hold exactly `custody`'s rows — [`Self::restore`] for
    /// a lagging replica described by its fold.
    pub fn restore_to(&self, custody: &SubscriptionsConfig) {
        let rows = custody
            .rows()
            .into_iter()
            .map(|(key, row)| (key, row.encode().expect("encode subscriptions row")))
            .collect();
        *self.rows.lock().unwrap() = rows;
    }

    /// The fold, synchronously — for assertions.
    pub fn folded(&self) -> SubscriptionsConfig {
        let rows = self.rows.lock().unwrap();
        let mut folded = SubscriptionsConfig::default();
        for (key, value) in rows.iter() {
            folded
                .fold_row(key, value)
                .expect("the memory store holds only rows it encoded");
        }
        folded
    }

    fn join(&self, row: fauna_core::subscription_rows::SubscriptionsRow) -> Result<(), StoreError> {
        let key = row.plane_key();
        let mut rows = self.rows.lock().unwrap();
        let joined = match rows.get(&key) {
            Some(stored) => fauna_core::subscription_rows::decode_subscriptions_row(&key, stored)
                .and_then(|stored| stored.merge(&row))
                .map_err(|e| StoreError::Save(format!("subscriptions row {key:?}: {e}")))?,
            None => row,
        };
        let bytes = joined
            .encode()
            .map_err(|e| StoreError::Save(format!("encode subscriptions row: {e}")))?;
        rows.insert(key, bytes);
        Ok(())
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl PeriodKeyStore for MemoryPeriodKeyStore {
    async fn custody(&self) -> Result<SubscriptionsConfig, StoreError> {
        Ok(self.folded())
    }

    async fn merge_custody(
        &self,
        replica: SubscriptionsConfig,
    ) -> Result<SubscriptionsConfig, StoreError> {
        for (_, row) in replica.rows() {
            self.join(row)?;
        }
        Ok(self.folded())
    }

    async fn settle_removal(
        &self,
        removal: PendingRemoval,
    ) -> Result<SubscriptionsConfig, StoreError> {
        use fauna_core::subscription_rows::{PendingRemovalRow, SubscriptionsRow, TierPeriodRow};
        self.join(SubscriptionsRow::Period(TierPeriodRow {
            tier_name: removal.tier_name.clone(),
            period: removal.new_period.clone(),
        }))?;
        self.join(SubscriptionsRow::Removal(PendingRemovalRow {
            removal,
            settled: true,
        }))?;
        Ok(self.folded())
    }
}
