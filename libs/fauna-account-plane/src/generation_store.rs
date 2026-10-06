//! Store-row plumbing shared by the generation machinery's three passes
//! (`generation_tip`, `generation_topup`, `generation_unkeyable`) — pure
//! `AccountStore` reads with no generation-specific logic, factored out
//! 2026-08-19 after a scout found byte-identical copies of both functions
//! in all three modules.

use anyhow::Result;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;

/// Every non-tombstone merged row of `kind`, for the resolver's iterators.
pub async fn live_rows<B: StoreBackend>(
    store: &AccountStore<B>,
    kind: &str,
) -> Result<Vec<StateEntry>> {
    Ok(store
        .states_of_kind(kind)
        .await?
        .into_iter()
        .filter(|e| !e.tombstone)
        .collect())
}

pub fn row_ref(entry: &StateEntry) -> (&str, &[u8]) {
    (entry.key.as_str(), entry.value.as_slice())
}
