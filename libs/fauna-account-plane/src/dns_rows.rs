//! The account's DNS management record's production writer and reader — the
//! typed door for `fauna.state.dns` (`fauna_core::data::DnsConfig` owns the
//! value; `docs/goal/behavior/dns-management.md` owns the concept;
//! `config-dissolution.md` § The `__config` dissolution schedule owns the
//! kind's place in the dissolution).
//!
//! **One row per account, at [`DNS_ROW_KEY`]**, whole-record latest-wins: a
//! write replaces the record under a fresh `LwwStamp`, and a concurrent write
//! from another device is ordered against it by the stamp alone — the shipped
//! blob rule (`theirs_wins`) with the entry's stamp in place of the blob's
//! `updated_at`. No merge arm reads the value, so the decode is tolerant: a
//! field a newer writer added survives this build's adoption, and this reader
//! decodes past it.
//!
//! The kind is `GenerationTip`-sealed, so a put while no tip resolves is
//! refused at the fleet plane's REAL writer door and surfaces to the caller as
//! the write error. Each put is the **local write only**
//! ([`AccountStatePlane::put_local`] through
//! [`put_lww_row_local`](crate::account_state_plane::put_lww_row_local)); the
//! account runtime's publish step ships it.
//!
//! Exposed through `fauna_sync_engine::account_runtime::AccountStoreHandle`'s
//! typed doors (`dns`, `write_dns`) so app glue never touches a plane handle
//! directly.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::DnsConfig;
use fauna_protocol::RpcRequester;
pub use fauna_protocol::merge_policy::DNS_ROW_KEY;
use fauna_protocol::merge_policy::KIND_DNS;

use crate::account_state_plane::{AccountStatePlane, put_lww_row_local};

/// The DNS record among `entries` (a `states_of_kind(KIND_DNS)` read) — the
/// default (no credentials, nothing managed) one when there is none. An entry
/// that does not decode fails loudly: silently reading it as empty would let
/// the next write replace the admin's credentials with nothing.
pub fn dns_of(entries: &[StateEntry]) -> Result<DnsConfig> {
    match entries
        .iter()
        .find(|e| e.kind == KIND_DNS && e.key == DNS_ROW_KEY && !e.tombstone)
    {
        Some(entry) => fauna_core::encoding::canonical_decode::<DnsConfig>(&entry.value)
            .context("the stored DNS record does not decode"),
        None => Ok(DnsConfig::default()),
    }
}

/// The DNS record this store holds (the default one when there is none).
pub async fn read_dns<B: StoreBackend>(store: &AccountStore<B>) -> Result<DnsConfig> {
    match store.state(KIND_DNS, DNS_ROW_KEY).await? {
        Some(entry) if !entry.tombstone => {
            fauna_core::encoding::canonical_decode::<DnsConfig>(&entry.value)
                .context("the stored DNS record does not decode")
        }
        _ => Ok(DnsConfig::default()),
    }
}

/// Replace the account's DNS record with `next`, stamped `(now, device_id)`.
/// Whether anything was written: `false` when the stored record already
/// decodes to `next` (a re-put would only churn the stamp and the feed).
pub async fn write_dns<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    next: &DnsConfig,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if read_dns(store).await? == *next {
        return Ok(false);
    }
    let value = fauna_core::encoding::canonical_encode(next).context("encode DNS record")?;
    put_lww_row_local(fleet, KIND_DNS, DNS_ROW_KEY, value.to_vec(), device_id)
        .await
        .context("DNS record: plane put")?;
    Ok(true)
}
