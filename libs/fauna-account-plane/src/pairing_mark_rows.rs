//! The pairing mark's writer and reader — the typed door for
//! `fauna.state.pairing-mark` (`account-runtime.md` § Multi-instance
//! concurrency → *A nest-side change a store-backed page shows leaves a mark
//! in the store: the pairing mark* owns the concept;
//! `fauna_client_core::linked_nests::pairing_mark_digest` owns the value).
//!
//! **One row per nest, at the hex64 of the nest's proven id**, its value the
//! 32-byte digest of the listing that nest answered. The row is a notice's
//! transport and nothing more: it carries no pairing and nothing reads it for
//! what a page lists. Latest-wins on the entry's `LwwStamp`; a put of the
//! value the row already holds writes nothing, so neither the gesture's write
//! nor the secondary leg's reconcile churns the feed (or the change
//! generation) when the listing has not moved.
//!
//! Two writers, two doors into the one function here:
//! - the shared `LinkedNestsMachine`'s link, both-ends link and unlink, through
//!   [`crate::account_driver::AccountStoreHandle::put_pairing_mark`] — a
//!   gesture's own write (`Cmd::is_background_write` is `false` for it), so it
//!   is no notice source on the acting runtime;
//! - the pump's secondary leg (`account_driver::pass::linked_pass`), which
//!   writes the bound nest's mark whenever its listing's digest is not the
//!   row's value — a pump run's write, which moves the runtime's generation.
//!
//! The kind is `GenerationTip`-sealed: a write while no tip resolves is
//! refused at the fleet plane's writer door and surfaces as the write error,
//! which both callers treat as best-effort (the next leg run writes it).

use anyhow::{Context, Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_client_core::linked_nests::pairing_mark_key;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_PAIRING_MARK;

use crate::account_state_plane::{AccountStatePlane, put_lww_row_local};

/// The digest a live row holds; a value that is not 32 bytes is refused
/// loudly (a writer bug), never read as "no mark".
fn digest_of(entry: &StateEntry) -> Result<[u8; 32]> {
    match <[u8; 32]>::try_from(entry.value.as_slice()) {
        Ok(d) => Ok(d),
        Err(_) => bail!(
            "the pairing mark at {:?} holds {} bytes, not a 32-byte digest",
            entry.key,
            entry.value.len()
        ),
    }
}

/// The mark this store holds for `nest_id`, if any.
pub async fn read_pairing_mark<B: StoreBackend>(
    store: &AccountStore<B>,
    nest_id: &[u8; 32],
) -> Result<Option<[u8; 32]>> {
    match store
        .state(KIND_PAIRING_MARK, &pairing_mark_key(nest_id))
        .await?
    {
        Some(entry) if !entry.tombstone => digest_of(&entry).map(Some),
        _ => Ok(None),
    }
}

/// Put `digest` as `nest_id`'s mark, stamped `(now, device_id)`. Whether
/// anything was written: `false` when the row already holds `digest`.
pub async fn put_pairing_mark<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    nest_id: &[u8; 32],
    digest: [u8; 32],
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if read_pairing_mark(store, nest_id).await? == Some(digest) {
        return Ok(false);
    }
    put_lww_row_local(
        fleet,
        KIND_PAIRING_MARK,
        &pairing_mark_key(nest_id),
        digest.to_vec(),
        device_id,
    )
    .await
    .context("pairing mark: plane put")?;
    Ok(true)
}
