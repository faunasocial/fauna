//! The subscription-tier period-key custody's production writer and reader —
//! the typed door for `fauna.state.subscriptions` (`key-material-hierarchy.md`
//! § Audience: an opaque set of subscriber pubkeys owns the keys;
//! `fauna_core::subscription_rows` owns the rows, their key grammar and their
//! per-row join; `config-dissolution.md` § The `__config` dissolution schedule
//! owns the kind's birth, plane-only, and § Phases and gates → *Bounded rows*
//! the one-row-per-record shape).
//!
//! **One row per period key and one per staged removal**; the composite
//! `SubscriptionsConfig` the custody code works on is the READ fold over every
//! row of the kind, through the shipped rule. A write is a **merge on the
//! store thread**, per row: the caller's replica is split into its records,
//! each is read, joined with the stored row (the per-row half the plane arm
//! runs on a sibling's row), and put through the fleet plane's REAL writer
//! door only when the join moved it. The kind is `GenerationTip`-sealed, so a
//! put while no tip resolves is refused there and surfaces to the caller.
//! Nothing between the reads and the puts yields to a walk (`Cmd::is_local`).
//! Each put is the **local write only** ([`AccountStatePlane::put_local`]);
//! the account runtime's publish step ships it.
//!
//! Because the write is a join, a caller never loses what the store gained
//! since it last read (another device's period, merged in by a walk): it gets
//! the joined fold back. And because nothing is ever deleted, a replica that
//! omits a period — a stale one, a fresh device's empty one — drops nothing.
//! A staged removal leaves the fold only by [`settle_pending_removal`], which
//! first writes its fresh period as a period row, so no settle strands a key.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::{PendingRemoval, SubscriptionsConfig};
use fauna_core::subscription_rows::{
    PendingRemovalRow, SubscriptionsRow, TierPeriodRow, decode_subscriptions_row,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_SUBSCRIPTIONS;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's period-key custody, folded — empty when nothing is stored.
pub async fn read_subscriptions<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<SubscriptionsConfig> {
    subscriptions_of(&store.states_of_kind(KIND_SUBSCRIPTIONS).await?)
}

/// The fold of every live `fauna.state.subscriptions` row among `entries` —
/// the handle's read, over one `states_of_kind` load. A row whose key does not
/// parse, whose value does not decode, or whose value is not the record its
/// key names fails loudly: silently skipping it would hide a period key from
/// the custody code, which would then mint to the roster without it.
pub fn subscriptions_of(entries: &[StateEntry]) -> Result<SubscriptionsConfig> {
    let mut folded = SubscriptionsConfig::default();
    for entry in entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_SUBSCRIPTIONS)
    {
        folded
            .fold_row(&entry.key, &entry.value)
            .with_context(|| format!("the stored subscriptions row {:?}", entry.key))?;
    }
    Ok(folded)
}

/// Join each record of `replica` — every period of every tier, every staged
/// removal — into its stored row and write every row the join moved. Returns
/// the account's fold as it now stands, and whether any put happened.
pub async fn merge_subscriptions<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    replica: &SubscriptionsConfig,
) -> Result<(SubscriptionsConfig, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut moved = false;
    for (key, row) in replica.rows() {
        moved |= join_row(store, fleet, key, &row).await?;
    }
    Ok((read_subscriptions(store).await?, moved))
}

/// Settle one staged removal: write its fresh period as a period row (the
/// orchestration has committed it into the tier), then mark the removal row
/// settled, so the sentinel leaves the fold on every replica and a stale
/// replica's unsettled copy cannot bring it back. Idempotent; returns the
/// account's fold and whether any put happened.
pub async fn settle_pending_removal<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    removal: &PendingRemoval,
) -> Result<(SubscriptionsConfig, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let period = SubscriptionsRow::Period(TierPeriodRow {
        tier_name: removal.tier_name.clone(),
        period: removal.new_period.clone(),
    });
    let settled = SubscriptionsRow::Removal(PendingRemovalRow {
        removal: removal.clone(),
        settled: true,
    });
    let mut moved = false;
    for row in [period, settled] {
        moved |= join_row(store, fleet, row.plane_key(), &row).await?;
    }
    Ok((read_subscriptions(store).await?, moved))
}

/// Read `key`, join `row` into it, and put the result when the join moved
/// the stored bytes. Whether a put happened.
async fn join_row<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    key: String,
    row: &SubscriptionsRow,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let joined = match store.state(KIND_SUBSCRIPTIONS, &key).await? {
        Some(entry) if !entry.tombstone => {
            let stored = decode_subscriptions_row(&key, &entry.value)
                .with_context(|| format!("the stored subscriptions row {key:?}"))?;
            let joined = stored
                .merge(row)
                .context("subscriptions row: join")?
                .encode()
                .context("encode subscriptions row")?;
            if joined == entry.value {
                return Ok(false);
            }
            joined
        }
        _ => row.encode().context("encode subscriptions row")?,
    };
    fleet
        .put_local(
            &ItemId {
                kind: KIND_SUBSCRIPTIONS.to_string(),
                key,
            },
            joined,
            // A row is its own record; no outer stamp.
            None,
        )
        .await
        .context("subscriptions row: plane put")?;
    Ok(true)
}
