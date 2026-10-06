//! The offline group-share ceremony record's production writer and reader —
//! the typed door for `fauna.state.group-share-ceremony` (`p2p.md` § Offline
//! share initiation owns the ceremony; `fauna_core::group_ceremony` owns the
//! records, their key grammar and their join; `config-dissolution.md` § The
//! `__config` dissolution schedule owns the kind's birth, plane-only, and
//! § Phases and gates → *Bounded rows* the one-row-per-ceremony shape).
//!
//! **One row per ceremony side-record** (`initiated/<scope>/<recipient>`,
//! `invited/<scope>`); the composite `GroupShareConfig` the ceremony code
//! works on is the READ fold over every row of the kind. A write is a
//! **merge on the store thread**, per row: the caller's replica is split into
//! its records, each is read, joined with the stored row
//! (`InitiatedGroupShare::merge` / `InvitedGroupShare::merge` — the halves
//! the plane arm runs on a sibling's row), and put through the fleet plane's
//! REAL writer door only when the join moved it. The kind is
//! `GenerationTip`-sealed, so a put while no tip resolves is refused there
//! and surfaces to the caller; the same tip seals the ceremony's held-root
//! row, so the ceremony gains no new precondition. Nothing between the reads
//! and the puts yields to a walk (`Cmd::is_local`). Each put is the **local
//! write only** ([`AccountStatePlane::put_local`]); the account runtime's
//! publish step ships it.
//!
//! Because the write is a join, a caller never loses what the store gained
//! since it last read (another device's record, merged in by a walk): it gets
//! the joined fold back and folds it into its own replica.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::group_ceremony::{GroupShareConfig, decode_group_share_row};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_GROUP_SHARE_CEREMONY;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's ceremony records, folded — empty when there are none.
pub async fn read_group_shares<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<GroupShareConfig> {
    group_shares_of(&store.states_of_kind(KIND_GROUP_SHARE_CEREMONY).await?)
}

/// The fold of every live `fauna.state.group-share-ceremony` row among
/// `entries` — the handle's read, over one `states_of_kind` load. A row whose
/// key does not parse, whose value does not decode, or whose value names a
/// different ceremony than its key fails loudly: silently skipping it would
/// let the next write put a bare replica over a ceremony it held.
pub fn group_shares_of(entries: &[StateEntry]) -> Result<GroupShareConfig> {
    let mut folded = GroupShareConfig::default();
    for entry in entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_GROUP_SHARE_CEREMONY)
    {
        folded
            .fold_row(&entry.key, &entry.value)
            .with_context(|| format!("the stored group-share ceremony row {:?}", entry.key))?;
    }
    Ok(folded)
}

/// Join each record of `replica` into its stored row and write every row the
/// join moved. Returns the account's fold as it now stands, and whether any
/// put happened.
pub async fn merge_group_shares<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    replica: &GroupShareConfig,
) -> Result<(GroupShareConfig, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut moved = false;
    for (key, record) in replica.rows() {
        let joined = match store.state(KIND_GROUP_SHARE_CEREMONY, &key).await? {
            Some(entry) if !entry.tombstone => {
                let stored = decode_group_share_row(&key, &entry.value)
                    .with_context(|| format!("the stored group-share ceremony row {key:?}"))?;
                let joined = stored
                    .merge(&record)
                    .context("group-share row: join")?
                    .encode()
                    .context("encode group-share row")?;
                if joined == entry.value {
                    continue;
                }
                joined
            }
            _ => record.encode().context("encode group-share row")?,
        };
        fleet
            .put_local(
                &ItemId {
                    kind: KIND_GROUP_SHARE_CEREMONY.to_string(),
                    key,
                },
                joined,
                // Every ceremony record carries its own stamp; no outer one.
                None,
            )
            .await
            .context("group-share row: plane put")?;
        moved = true;
    }
    Ok((read_group_shares(store).await?, moved))
}
