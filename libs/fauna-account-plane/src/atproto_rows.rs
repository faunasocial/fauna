//! The account's minted ATProto app credentials' production writer and reader
//! — the typed door for `fauna.state.atproto`
//! (`fauna_core::data::AtprotoAppCredential` owns the value;
//! `atproto-pds-full.md` owns the concept; `config-dissolution.md` § The
//! `__config` dissolution schedule owns the kind's place in the dissolution).
//!
//! **One row per credential, at its `credential_id`** (`config-dissolution.md`
//! § Phases and gates → *Bounded rows*: the list grows by one per mint and
//! every entry carries a secret, so each credential is its own bounded row).
//! [`AtprotoConfig`] is the READ fold over the account's live rows, oldest
//! first. Each row is latest-wins on the entry's `LwwStamp`: a mint is a put, a
//! revoke a stamped tombstone — the shipped blob rule (whole-record
//! `theirs_wins`) narrowed to one credential. No merge arm reads the value, so
//! the decode is tolerant: a field a newer writer added survives this build's
//! adoption, and this reader decodes past it.
//!
//! The kind is `GenerationTip`-sealed, so a write while no tip resolves is
//! refused at the fleet plane's REAL writer door and surfaces to the caller as
//! the write error. Each write is the **local write only**
//! ([`AccountStatePlane::put_local`] / [`AccountStatePlane::tombstone_local`]);
//! the account runtime's publish step ships it.
//!
//! Exposed through `fauna_sync_engine::account_runtime::AccountStoreHandle`'s
//! typed doors (`atproto`, `put_app_credential`, `revoke_app_credential`) so
//! app glue never touches a plane handle directly.

use anyhow::{Context, Result, bail, ensure};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::{AtprotoAppCredential, AtprotoConfig};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_ATPROTO;

use crate::account_state_plane::{AccountStatePlane, put_lww_row_local, tombstone_lww_row_local};

/// Decode one live row, refusing a value that does not name its own key: a
/// read keyed by `credential_id` would otherwise find a different credential's
/// secret. Fails loudly — silently dropping it would hide a writer bug.
fn credential_of(entry: &StateEntry) -> Result<AtprotoAppCredential> {
    let credential = fauna_core::encoding::canonical_decode::<AtprotoAppCredential>(&entry.value)
        .with_context(|| {
        format!("the stored app credential {:?} does not decode", entry.key)
    })?;
    if credential.credential_id != entry.key {
        bail!(
            "the app credential row at {:?} names {:?} — a row's value must name its own key",
            entry.key,
            credential.credential_id
        );
    }
    Ok(credential)
}

/// The app credentials among `entries` (a `states_of_kind(KIND_ATPROTO)`
/// read), folded into [`AtprotoConfig`]: every live row, oldest first
/// (`created_at`, then `credential_id` — the blob's "newest last" order made
/// total). Tombstones (revoked credentials) are skipped.
pub fn atproto_of(entries: &[StateEntry]) -> Result<AtprotoConfig> {
    let mut app_credentials = entries
        .iter()
        .filter(|e| e.kind == KIND_ATPROTO && !e.tombstone)
        .map(credential_of)
        .collect::<Result<Vec<_>>>()?;
    app_credentials
        .sort_by(|a, b| (a.created_at, &a.credential_id).cmp(&(b.created_at, &b.credential_id)));
    Ok(AtprotoConfig { app_credentials })
}

/// The app credentials this store holds.
pub async fn read_atproto<B: StoreBackend>(store: &AccountStore<B>) -> Result<AtprotoConfig> {
    atproto_of(&store.states_of_kind(KIND_ATPROTO).await?)
}

/// The one live credential at `credential_id`, if this store holds it.
pub async fn read_app_credential<B: StoreBackend>(
    store: &AccountStore<B>,
    credential_id: &str,
) -> Result<Option<AtprotoAppCredential>> {
    match store.state(KIND_ATPROTO, credential_id).await? {
        Some(entry) if !entry.tombstone => credential_of(&entry).map(Some),
        _ => Ok(None),
    }
}

/// Put `credential` at its own `credential_id`, stamped `(now, device_id)`.
/// Whether anything was written: `false` when the stored row already decodes
/// to `credential` (a re-put would only churn the stamp and the feed).
pub async fn put_app_credential<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    credential: &AtprotoAppCredential,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    ensure!(
        !credential.credential_id.is_empty(),
        "an app credential needs a credential_id to key its row"
    );
    if read_app_credential(store, &credential.credential_id)
        .await?
        .as_ref()
        == Some(credential)
    {
        return Ok(false);
    }
    let value =
        fauna_core::encoding::canonical_encode(credential).context("encode app credential")?;
    put_lww_row_local(
        fleet,
        KIND_ATPROTO,
        &credential.credential_id,
        value.to_vec(),
        device_id,
    )
    .await
    .context("app credential: plane put")?;
    Ok(true)
}

/// Revoke the credential at `credential_id`: a stamped tombstone, which a
/// concurrent put of the same row is ordered against by the stamp. Whether
/// anything was written: `false` when no live row is stored there.
pub async fn revoke_app_credential<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    credential_id: &str,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    match store.state(KIND_ATPROTO, credential_id).await? {
        Some(entry) if !entry.tombstone => {}
        _ => return Ok(false),
    }
    tombstone_lww_row_local(fleet, KIND_ATPROTO, credential_id, device_id)
        .await
        .context("app credential: plane tombstone")?;
    Ok(true)
}
