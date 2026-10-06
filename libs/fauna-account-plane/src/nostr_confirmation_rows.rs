//! The npub confirmation stamp's production writer and reader — the typed
//! door for `fauna.state.nostr-confirmation`
//! (`fauna_core::nostr_confirmation` owns the value, its strict decode and its
//! join; `nostr.md` § Key succession and rotation owns the concept, leg 3 of
//! the aftermath; `config-dissolution.md` § The `__config` dissolution
//! schedule owns the kind's birth, plane-only).
//!
//! **One row per account, at [`NOSTR_CONFIRMATION_ROW_KEY`].** A confirmation
//! is a **merge on the store thread**: the stored stamp is read, joined with
//! the new one (`NostrConfirmation::merge` — the max the plane arm runs on a
//! sibling's row), and put through the fleet plane's REAL writer door only
//! when the join moved it, so a later confirmation another device already
//! wrote is never lowered. The kind is `GenerationTip`-sealed, so a put while
//! no tip resolves is refused there and surfaces to the caller as the write
//! error. Nothing between the read and the put yields to a walk
//! (`Cmd::is_local`). Each put is the **local write only**
//! ([`AccountStatePlane::put_local`]); the account runtime's publish step
//! ships it.
//!
//! The sole writer is the owner's own confirm gesture (and the best-effort
//! confirm a fresh link records), never a post-auth pass — so the kind needs
//! no store-ready edge of its own: any gesture happens with the runtime up.
//!
//! Exposed through `fauna_sync_engine::account_runtime::AccountStoreHandle`'s
//! typed doors (`npub_confirmed_at`, `confirm_nostr_npub`) so app glue
//! never touches a plane handle directly.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::nostr_confirmation::{NostrConfirmation, decode_nostr_confirmation_row};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_NOSTR_CONFIRMATION;
pub use fauna_protocol::merge_policy::NOSTR_CONFIRMATION_ROW_KEY;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// The confirmation among `entries` (a `states_of_kind` read) — unconfirmed
/// when there is none. A row that does not decode fails loudly: reading it as
/// unconfirmed would raise a banner the owner already answered.
pub fn nostr_confirmation_of(entries: &[StateEntry]) -> Result<NostrConfirmation> {
    match entries.iter().find(|e| {
        e.kind == KIND_NOSTR_CONFIRMATION && e.key == NOSTR_CONFIRMATION_ROW_KEY && !e.tombstone
    }) {
        Some(entry) => decode_nostr_confirmation_row(&entry.key, &entry.value)
            .context("the stored npub confirmation does not decode"),
        None => Ok(NostrConfirmation::default()),
    }
}

/// The confirmation this store holds (unconfirmed when there is none).
pub async fn read_nostr_confirmation<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<NostrConfirmation> {
    match store
        .state(KIND_NOSTR_CONFIRMATION, NOSTR_CONFIRMATION_ROW_KEY)
        .await?
    {
        Some(entry) if !entry.tombstone => decode_nostr_confirmation_row(&entry.key, &entry.value)
            .context("the stored npub confirmation does not decode"),
        _ => Ok(NostrConfirmation::default()),
    }
}

/// Join a confirmation at `now` (epoch seconds) into the stored stamp and
/// write it when the join moved it. Whether anything was written: `false`
/// when the store already holds a confirmation at or after `now`.
pub async fn confirm_nostr_npub<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    now: i64,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let stored = read_nostr_confirmation(store).await?;
    let joined = stored.merge(&NostrConfirmation {
        confirmed_at: Some(now),
    });
    if joined == stored {
        return Ok(false);
    }
    let value = joined.encode().context("encode npub confirmation")?;
    fleet
        .put_local(
            &ItemId {
                kind: KIND_NOSTR_CONFIRMATION.to_string(),
                key: NOSTR_CONFIRMATION_ROW_KEY.to_string(),
            },
            value,
            // The stamp is its own merge state; no outer one.
            None,
        )
        .await
        .context("npub confirmation: plane put")?;
    Ok(true)
}
