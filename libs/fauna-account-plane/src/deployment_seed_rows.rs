//! The multi-nest deployment-seed custody's production writer and reader —
//! the typed door for `fauna.state.deployment-seeds` (`nest/box-recovery.md`
//! § Trust & audience owns the seeds; `fauna_core::deployment_seed_rows` owns
//! the rows, their key grammar, the strict decode and the per-row join;
//! `config-dissolution.md` § The `__config` dissolution schedule owns the
//! kind's birth, plane-only, and § Phases and gates → *Bounded rows* the
//! one-row-per-box shape).
//!
//! **One row per custodied box**; the composite map the custody code works
//! on is the READ fold over every row of the kind, through the shipped rule
//! (`DeploymentSeedEntry::merge_seed_map`). A write is a **merge on the store
//! thread**, per row: the caller's replica is split into its boxes, each is
//! read, joined with the stored row (the per-row half the plane arm runs on a
//! sibling's row), and put through the fleet plane's REAL writer door only
//! when the join moved it. The kind is `GenerationTip`-sealed, so a put while
//! no tip resolves is refused there and surfaces to the caller. Nothing
//! between the reads and the puts yields to a walk (`Cmd::is_local`). Each
//! put is the **local write only** ([`AccountStatePlane::put_local`]); the
//! account runtime's publish step ships it.
//!
//! Because the write is a join, a caller never loses what the store gained
//! since it last read (another device's captured box, merged in by a walk):
//! it gets the joined fold back. Nothing is ever deleted — a replica that
//! omits a box drops nothing, a stale unmarked copy never un-marks a rotated
//! one — and a rotation's supersession mark is written the same way, as a
//! merge of the marked entry.
//!
//! A replica entry the plane would refuse — its seed not its id's preimage,
//! or carrying a field this build does not know — is refused HERE, before
//! any put: the door writes only rows every sibling's arm adopts.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::DeploymentSeedEntry;
use fauna_core::deployment_seed_rows::{
    decode_deployment_seed_row, deployment_seed_rows, fold_deployment_seed_row,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_DEPLOYMENT_SEEDS;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's deployment-seed custody map, folded — empty when nothing is
/// stored.
pub async fn read_deployment_seeds<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<Vec<DeploymentSeedEntry>> {
    deployment_seeds_of(&store.states_of_kind(KIND_DEPLOYMENT_SEEDS).await?)
}

/// The fold of every live `fauna.state.deployment-seeds` row among `entries`
/// — the handle's read, over one `states_of_kind` load. A row that does not
/// decode strictly, names another box, or carries a seed that is not its id's
/// preimage fails loudly: silently skipping it would hide a box from the
/// recovery list, whose seed is irrecoverable.
pub fn deployment_seeds_of(entries: &[StateEntry]) -> Result<Vec<DeploymentSeedEntry>> {
    let mut folded = Vec::new();
    for entry in entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_DEPLOYMENT_SEEDS)
    {
        fold_deployment_seed_row(&mut folded, &entry.key, &entry.value)
            .with_context(|| format!("the stored deployment-seed row {:?}", entry.key))?;
    }
    Ok(folded)
}

/// Join each box of `replica` into its stored row and write every row the
/// join moved. Returns the account's fold as it now stands, and whether any
/// put happened.
pub async fn merge_deployment_seeds<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    replica: &[DeploymentSeedEntry],
) -> Result<(Vec<DeploymentSeedEntry>, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let rows = deployment_seed_rows(replica);
    // Refuse the whole write before any put when one box would be refused.
    for (key, entry) in &rows {
        let bytes = entry.encode_row().context("encode deployment-seed row")?;
        decode_deployment_seed_row(key, &bytes)
            .with_context(|| format!("the deployment-seed entry for {key:?}"))?;
    }
    let mut moved = false;
    for (key, entry) in rows {
        moved |= join_row(store, fleet, key, &entry).await?;
    }
    Ok((read_deployment_seeds(store).await?, moved))
}

/// Whether the row for `nest_actor_id` rests AND this device owes the feed no
/// write of it — no own journal row above this device's published high-water
/// (its frontier slot, the one `publish_pending` ships from) names the row.
/// The plane rotation drive's gate between its custody merge and its dispatch
/// (`box-recovery.md` § The ceremony): the same "at or below our own frontier
/// slot" test the reclamation pass holds a retire to
/// (`generation_reclaim::own_published_through`). A row this device never
/// wrote reached it from a replica that holds it, and reads as published. A
/// pull-only plane never publishes, so a row it wrote never reads as
/// published.
pub async fn deployment_seed_published<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    nest_actor_id: &[u8; 32],
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let key = fauna_core::hex32::encode(nest_actor_id);
    match store.state(KIND_DEPLOYMENT_SEEDS, &key).await? {
        Some(stored) if !stored.tombstone => {}
        _ => return Ok(false),
    }
    Ok(!fleet
        .owes_own_state(|kind, k| kind == KIND_DEPLOYMENT_SEEDS && k == key)
        .await?)
}

/// Read `key`, join `entry` into it, and put the result when the join moved
/// the stored bytes. Whether a put happened.
async fn join_row<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    key: String,
    entry: &DeploymentSeedEntry,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let joined = match store.state(KIND_DEPLOYMENT_SEEDS, &key).await? {
        Some(stored) if !stored.tombstone => {
            let current = decode_deployment_seed_row(&key, &stored.value)
                .with_context(|| format!("the stored deployment-seed row {key:?}"))?;
            let joined = current
                .merge(entry)
                .context("deployment-seed row: join")?
                .encode_row()
                .context("encode deployment-seed row")?;
            if joined == stored.value {
                return Ok(false);
            }
            joined
        }
        _ => entry.encode_row().context("encode deployment-seed row")?,
    };
    fleet
        .put_local(
            &ItemId {
                kind: KIND_DEPLOYMENT_SEEDS.to_string(),
                key,
            },
            joined,
            // A row is its own record; no outer stamp.
            None,
        )
        .await
        .context("deployment-seed row: plane put")?;
    Ok(true)
}
