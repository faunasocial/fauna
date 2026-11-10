//! The account's muted words, hidden report subjects and spam marks — the typed door for
//! `fauna.state.moderation` (`content-moderation-and-ranking.md`
//! § Implementation status today → *The moderation kind's rows — one per
//! keyword, one per hidden id* owns the ruling; `fauna_core::moderation_rows`
//! owns the key grammar, the fold and the diff).
//!
//! **One row per muted term, one per hidden id, one per spam mark**, each latest-wins on the
//! entry's `LwwStamp`: an add or a re-weight is a put, a removal a stamped
//! tombstone — the `follows_rows` shape. [`ModerationConfig`] is the READ fold
//! over the live rows; a surface runs its shared delta
//! (`fauna_client_config::preference_records`) on that fold and hands
//! [`write_changes`] the diff (`ModerationConfig::changes_to`).
//!
//! The kind is delegable (`Gen0`), so the rows go through the delegable
//! plane's local write, stamped with this replica's store writer — the same
//! plane and stamp writer the preference door gives its siblings; the writes
//! of one gesture are stamped in strictly increasing order. Each write is the
//! **local write only**; the account runtime's publish step ships it.
//!
//! Exposed through `AccountStoreHandle::{moderation, write_moderation}` so app
//! glue never touches a plane handle directly.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::{ModerationConfig, MutedKeyword};
use fauna_core::moderation_rows::{HiddenContentMark, ModerationChange, SpamMark};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::{KIND_MODERATION, LwwStamp};

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// The moderation record among `entries` (a `states_of_kind(KIND_MODERATION)`
/// read): every live row folded, each family oldest-stamp first. A row with
/// no readable stamp sorts first. Tombstones are skipped.
pub fn moderation_of(entries: &[StateEntry]) -> Result<ModerationConfig> {
    let rows = entries
        .iter()
        .filter(|e| e.kind == KIND_MODERATION && !e.tombstone)
        .map(|e| {
            let rank = e
                .merge_meta
                .as_deref()
                .and_then(|meta| LwwStamp::decode(meta).ok())
                .map(|stamp| stamp.rank());
            (rank, e.key.clone(), e.value.clone())
        });
    ModerationConfig::fold(rows).context("fauna.state.moderation: the rows do not fold")
}

/// The moderation record this store holds.
pub async fn read_moderation<B: StoreBackend>(store: &AccountStore<B>) -> Result<ModerationConfig> {
    moderation_of(&store.states_of_kind(KIND_MODERATION).await?)
}

/// The live row at `key`, if this store holds one.
async fn live_row<B: StoreBackend>(
    store: &AccountStore<B>,
    key: &str,
) -> Result<Option<StateEntry>> {
    Ok(store
        .state(KIND_MODERATION, key)
        .await?
        .filter(|entry| !entry.tombstone))
}

/// The stamps one gesture's writes carry: this replica's store writer, at the
/// wall clock — and each strictly after the one before, so the rows a single
/// save adds fold in the order it listed them rather than in key order (two
/// rows stamped in one millisecond by one writer would tie).
struct Stamps {
    writer: [u8; 32],
    last_ms: Option<i64>,
}

impl Stamps {
    fn of<B: StoreBackend>(store: &AccountStore<B>) -> Self {
        Self {
            writer: store.writer().0,
            last_ms: None,
        }
    }

    fn next(&mut self) -> Result<Vec<u8>> {
        let now = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
        let at_ms = self.last_ms.map_or(now, |last| now.max(last + 1));
        self.last_ms = Some(at_ms);
        Ok(LwwStamp {
            at_ms,
            writer: self.writer,
        }
        .encode()?)
    }
}

fn item(key: &str) -> ItemId {
    ItemId {
        kind: KIND_MODERATION.into(),
        key: key.into(),
    }
}

/// Put `value` at `key` unless the live row there already holds those bytes —
/// a re-put would mint a stamp that outranks a sibling's concurrent edit.
async fn put_row<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    stamps: &mut Stamps,
    key: &str,
    value: Vec<u8>,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if live_row(store, key)
        .await?
        .is_some_and(|entry| entry.value == value)
    {
        return Ok(false);
    }
    plane
        .put_local(&item(key), value, Some(stamps.next()?))
        .await
        .context("moderation: plane put")?;
    Ok(true)
}

/// Tombstone the row at `key` unless no live row is stored there (removing
/// what is already gone is success).
async fn tombstone_row<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    stamps: &mut Stamps,
    key: &str,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    if live_row(store, key).await?.is_none() {
        return Ok(false);
    }
    plane
        .tombstone_local(&item(key), Some(stamps.next()?))
        .await
        .context("moderation: plane tombstone")?;
    Ok(true)
}

async fn put_keyword_row<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    stamps: &mut Stamps,
    keyword: &MutedKeyword,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let value = fauna_core::encoding::canonical_encode(keyword).context("encode muted keyword")?;
    put_row(store, plane, stamps, &keyword.plane_key(), value.to_vec()).await
}

async fn put_hidden_row<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    stamps: &mut Stamps,
    mark: &HiddenContentMark,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let value = fauna_core::encoding::canonical_encode(mark).context("encode hidden mark")?;
    put_row(store, plane, stamps, &mark.plane_key(), value.to_vec()).await
}

async fn put_spam_row<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    stamps: &mut Stamps,
    mark: &SpamMark,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let value = fauna_core::encoding::canonical_encode(mark).context("encode spam mark")?;
    put_row(store, plane, stamps, &mark.plane_key(), value.to_vec()).await
}

/// Put `keyword` at its own key. Whether anything was written.
pub async fn put_muted_keyword<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    keyword: &MutedKeyword,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    put_keyword_row(store, plane, &mut Stamps::of(store), keyword).await
}

/// Take the keyword row at `key` off the list. Whether anything was written.
pub async fn tombstone_muted_keyword<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    key: &str,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    tombstone_row(store, plane, &mut Stamps::of(store), key).await
}

/// Put `mark` at its own key. Whether anything was written.
pub async fn put_hidden<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    mark: &HiddenContentMark,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    put_hidden_row(store, plane, &mut Stamps::of(store), mark).await
}

/// Show the subject whose hidden row is at `key` again. Whether anything was
/// written.
pub async fn tombstone_hidden<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    key: &str,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    tombstone_row(store, plane, &mut Stamps::of(store), key).await
}

/// Apply `changes` in order, each through its typed write, each stamped after
/// the one before. Whether anything was written.
pub async fn write_changes<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    changes: &[ModerationChange],
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let mut stamps = Stamps::of(store);
    let mut wrote = false;
    for change in changes {
        let stamps = &mut stamps;
        wrote |= match change {
            ModerationChange::PutKeyword(keyword) => {
                put_keyword_row(store, plane, stamps, keyword).await?
            }
            ModerationChange::DropKeyword(key) => tombstone_row(store, plane, stamps, key).await?,
            ModerationChange::PutHidden(mark) => put_hidden_row(store, plane, stamps, mark).await?,
            ModerationChange::DropHidden(key) => tombstone_row(store, plane, stamps, key).await?,
            ModerationChange::PutSpam(mark) => put_spam_row(store, plane, stamps, mark).await?,
            ModerationChange::DropSpam(key) => tombstone_row(store, plane, stamps, key).await?,
        };
    }
    Ok(wrote)
}
