//! The account's followed public folders' production writer and reader — the
//! typed door for `fauna.state.follows` (`fauna_core::data::FollowedFolder`
//! owns the value; `folders.md` § Publicly-synced follow owns the concept;
//! `config-dissolution.md` § The `__config` dissolution schedule owns the
//! kind's place in the dissolution).
//!
//! **One row per followed folder, at `FollowedFolder::plane_key`**
//! (`config-dissolution.md` § Phases and gates → *Bounded rows*: the list grows
//! by one per follow with no cap, so each follow is its own bounded row).
//! [`FollowsConfig`] is the READ fold over the account's live rows, in the
//! canonical `(home_nest_url, folder_id)` order. Each row is latest-wins on the
//! entry's `LwwStamp`: a follow (or the refresh of one) is a put, an unfollow a
//! stamped tombstone — the shipped blob rule (whole-record `theirs_wins`)
//! narrowed to one folder, so an unfollow still propagates and two devices'
//! concurrent follows both survive. No merge arm reads the value, so the
//! decode is tolerant: a field a newer writer added survives this build's
//! adoption, and this reader decodes past it.
//!
//! The kind is `GenerationTip`-sealed, so a write while no tip resolves is
//! refused at the fleet plane's REAL writer door and surfaces to the caller as
//! the write error. Each write is the **local write only**
//! ([`AccountStatePlane::put_local`] / [`AccountStatePlane::tombstone_local`]);
//! the account runtime's publish step ships it.
//!
//! Exposed through `fauna_sync_engine::account_runtime::AccountStoreHandle`'s
//! typed doors (`follows`, `put_follow`, `unfollow`) so app glue never touches
//! a plane handle directly.

use anyhow::{Context, Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::{FollowedFolder, FollowsConfig};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_FOLLOWS;

use crate::account_state_plane::{AccountStatePlane, put_lww_row_local, tombstone_lww_row_local};

/// Decode one live row, refusing a value that does not name its own key: a
/// read keyed by a folder's identity would otherwise find a different folder.
/// Fails loudly — silently dropping it would hide a writer bug.
fn follow_of(entry: &StateEntry) -> Result<FollowedFolder> {
    let follow = fauna_core::encoding::canonical_decode::<FollowedFolder>(&entry.value)
        .with_context(|| format!("the stored follow {:?} does not decode", entry.key))?;
    if follow.plane_key() != entry.key {
        bail!(
            "the follow row at {:?} names {:?} folder {} — a row's value must name its own key",
            entry.key,
            follow.home_nest_url,
            follow.folder_id
        );
    }
    Ok(follow)
}

/// The followed folders among `entries` (a `states_of_kind(KIND_FOLLOWS)`
/// read), folded into [`FollowsConfig`]: every live row, in the canonical
/// order every writer of the blob field leaves it in. Tombstones (unfollowed
/// folders) are skipped.
pub fn follows_of(entries: &[StateEntry]) -> Result<FollowsConfig> {
    let followed = entries
        .iter()
        .filter(|e| e.kind == KIND_FOLLOWS && !e.tombstone)
        .map(follow_of)
        .collect::<Result<Vec<_>>>()?;
    let mut follows = FollowsConfig { followed };
    follows.sort_canonically();
    Ok(follows)
}

/// The followed folders this store holds.
pub async fn read_follows<B: StoreBackend>(store: &AccountStore<B>) -> Result<FollowsConfig> {
    follows_of(&store.states_of_kind(KIND_FOLLOWS).await?)
}

/// The one live follow at `(home_nest_url, folder_id)`, if this store holds it.
pub async fn read_follow<B: StoreBackend>(
    store: &AccountStore<B>,
    home_nest_url: &str,
    folder_id: i64,
) -> Result<Option<FollowedFolder>> {
    let key = FollowedFolder::plane_key_of(home_nest_url, folder_id);
    match store.state(KIND_FOLLOWS, &key).await? {
        Some(entry) if !entry.tombstone => follow_of(&entry).map(Some),
        _ => Ok(None),
    }
}

/// Put `follow` at its own identity, stamped `(now, device_id)` — a new follow,
/// or the refresh of one (the display name and the nest stamp come from the
/// latest fetch). Whether anything was written: `false` when the stored row
/// already decodes to `follow` (a re-put would only churn the stamp and the
/// feed).
pub async fn put_follow<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    follow: &FollowedFolder,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if read_follow(store, &follow.home_nest_url, follow.folder_id)
        .await?
        .as_ref()
        == Some(follow)
    {
        return Ok(false);
    }
    let value = fauna_core::encoding::canonical_encode(follow).context("encode follow")?;
    put_lww_row_local(
        fleet,
        KIND_FOLLOWS,
        &follow.plane_key(),
        value.to_vec(),
        device_id,
    )
    .await
    .context("follow: plane put")?;
    Ok(true)
}

/// Unfollow the folder at `(home_nest_url, folder_id)`: a stamped tombstone,
/// which a concurrent put of the same row is ordered against by the stamp.
/// Whether anything was written: `false` when no live row is stored there
/// (unfollowing what is already gone is success).
pub async fn unfollow<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    device_id: [u8; 32],
    home_nest_url: &str,
    folder_id: i64,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let key = FollowedFolder::plane_key_of(home_nest_url, folder_id);
    match store.state(KIND_FOLLOWS, &key).await? {
        Some(entry) if !entry.tombstone => {}
        _ => return Ok(false),
    }
    tombstone_lww_row_local(fleet, KIND_FOLLOWS, &key, device_id)
        .await
        .context("follow: plane tombstone")?;
    Ok(true)
}
