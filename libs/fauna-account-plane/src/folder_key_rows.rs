//! Shared-folder content-key custody's production writer and reader — the
//! typed door for `fauna.state.folder-keys` (`fauna_core::folder_key_rows`
//! owns the key grammar and the rows' decode; `FoldersConfig::merge` owns the
//! join; `config-dissolution.md` § Phases and gates → *Bounded rows* owns the
//! shape).
//!
//! **One row per entity** — a custody entry's metadata, each of its content-key
//! generations, each staged removal, each foreign set — and the composite
//! [`FoldersConfig`] the custody API works on is the READ fold over every row of
//! the kind ([`FoldersConfig::fold`]).
//!
//! A write is a **merge on the store thread**, per row: the caller hands its
//! replica (the custody it read, changed as the blob-era custody helpers change
//! it), each of the replica's rows is joined with the stored row (the same
//! per-row arm the plane runs on a sibling's row — `FolderKeyRecord::merge`)
//! and put through the fleet plane's REAL writer door only when the join moved
//! it. So a write can add and advance, never drop: a generation, a staging or a
//! foreign record the replica no longer holds stays a row (the module docs of
//! `fauna_core::folder_key_rows` say why). A staging the orchestration is done
//! with leaves the fold only by [`settle_pending_removal`], which first writes
//! its fresh generation, so no settle strands a key.
//! Before any put the door refuses, whole, a replica with a row the plane arm
//! would refuse (an entry with neither nonce nor channel, a value not naming
//! its key). The kind is `GenerationTip`-sealed, so a put while no tip resolves
//! is refused at the door and surfaces to the caller, whose leg retries. Each
//! put is the **local write only** ([`AccountStatePlane::put_local`]); the
//! account runtime's publish step ships it.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::{FolderKeyCustody, FolderPendingRemoval, FoldersConfig};
use fauna_core::folder_key_rows::{
    FolderGenerationRow, FolderKeyRecord, FolderRemovalRow, SetIdentity, decode_folder_key_row,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_FOLDER_KEYS;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's folder-key custody — empty when no row rests yet.
pub async fn read_folder_keys<B: StoreBackend>(store: &AccountStore<B>) -> Result<FoldersConfig> {
    folder_keys_of(&store.states_of_kind(KIND_FOLDER_KEYS).await?)
}

/// The READ fold of every live `fauna.state.folder-keys` row among `entries`
/// — the handle's read, over one `states_of_kind` load. A row whose key does
/// not parse or whose value does not decode, re-encode to its own bytes or
/// name its key fails loudly: silently skipping it would hide a content key
/// from every reader.
pub fn folder_keys_of(entries: &[StateEntry]) -> Result<FoldersConfig> {
    FoldersConfig::fold(
        entries
            .iter()
            .filter(|e| !e.tombstone && e.kind == KIND_FOLDER_KEYS)
            .map(|e| (e.key.as_str(), e.value.as_slice())),
    )
    .context("the stored folder-keys rows")
}

/// Join `record` into its stored row and put the join when it moved the row.
/// Returns whether a put happened.
async fn put_row<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    key: String,
    record: &FolderKeyRecord,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let joined = match store.state(KIND_FOLDER_KEYS, &key).await? {
        Some(entry) if !entry.tombstone => {
            let stored = decode_folder_key_row(&key, &entry.value)
                .with_context(|| format!("the stored folder-keys row {key:?}"))?;
            let joined = stored
                .merge(record)
                .with_context(|| format!("folder-keys row {key:?}: join"))?
                .encode()
                .context("encode folder-keys row")?;
            if joined == entry.value {
                return Ok(false);
            }
            joined
        }
        _ => record.encode().context("encode folder-keys row")?,
    };
    fleet
        .put_local(
            &ItemId {
                kind: KIND_FOLDER_KEYS.to_string(),
                key,
            },
            joined,
            // Every row is its own merge state; no outer stamp.
            None,
        )
        .await
        .context("folder-keys row: plane put")?;
    Ok(true)
}

/// Join each entity of `replica` into its stored row and write every row the
/// join moved. Returns the account's custody as it now reads, and whether any
/// put happened.
///
/// # Errors
/// The door's refusal (module docs), before any put — and the writer door's
/// own, a put while no generation tip resolves.
pub async fn merge_folder_keys<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    replica: &FoldersConfig,
) -> Result<(FoldersConfig, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    // Canonicalize first — a replica holding one entity twice joins it once —
    // and let every refusal run before the first put, so a refused merge
    // leaves nothing behind.
    let rows = FoldersConfig::default()
        .merge(replica)
        .rows()
        .context("folder-keys door: the replica's rows")?;
    for (key, record) in &rows {
        decode_folder_key_row(key, &record.encode()?)
            .with_context(|| format!("folder-keys door: the row at {key:?}"))?;
    }
    let mut moved = false;
    for (key, record) in rows {
        moved |= put_row(store, fleet, key, &record).await?;
    }
    Ok((read_folder_keys(store).await?, moved))
}

/// Settle one staged removal: write its fresh generation as a `gen/` row of
/// the set it rotates (the orchestration has committed it into the set's
/// `current`), then mark the removal row settled, so the staging leaves the
/// fold on every replica and a stale replica's unsettled copy cannot bring it
/// back — the plane twin of the blob's `clear_pending_removal`, in the
/// `fauna.state.subscriptions` removal row's shape. The set is the one the
/// custody API's commit resolves by channel: the first live entry holding
/// `removal.channel_id` in the fold's order, else any entry holding it, else a
/// bare channel-keyed entry. Idempotent; returns the account's custody and
/// whether any put happened.
///
/// # Errors
/// A stored row the fold refuses, or the writer door's refusal.
pub async fn settle_pending_removal<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    removal: &FolderPendingRemoval,
) -> Result<(FoldersConfig, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let custody = read_folder_keys(store).await?;
    let holds = |e: &&FolderKeyCustody| e.channel_id == Some(removal.channel_id);
    let set = custody
        .sets
        .iter()
        .filter(holds)
        .find(|e| e.is_live())
        .or_else(|| custody.sets.iter().find(holds))
        .and_then(SetIdentity::of)
        .unwrap_or(SetIdentity::Channel(removal.channel_id));
    let rows = [
        FolderKeyRecord::Generation(FolderGenerationRow {
            set,
            generation: removal.new_generation.clone(),
        }),
        FolderKeyRecord::Removal(FolderRemovalRow {
            removal: removal.clone(),
            settled: true,
        }),
    ];
    let mut moved = false;
    for record in &rows {
        moved |= put_row(store, fleet, record.plane_key()?, record).await?;
    }
    Ok((read_folder_keys(store).await?, moved))
}
