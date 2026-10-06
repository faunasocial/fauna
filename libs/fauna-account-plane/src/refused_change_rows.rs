//! The refused inbound scheduling changes' production writer and reader — the
//! typed door for `fauna.state.refused-scheduling-changes`
//! (`inbound-scheduling-authority.md` § *Where the record rests* owns the
//! record; `fauna_core::refused_change_rows` owns the row, its key, the strict
//! decode and the owner's gestures; `config-dissolution.md` § The `__config`
//! dissolution schedule owns the kind's birth, plane-only).
//!
//! **One row per account**, at key `self`. Every write is a
//! **read-modify-put on the store thread**: the stored list is read (strictly
//! — a row the plane would refuse fails loudly rather than being overwritten),
//! the gesture is applied to it ([`RefusedChangeWrite`]), and the result is put
//! through the fleet plane's REAL writer door only when its bytes moved. The
//! kind is `GenerationTip`-sealed, so a put while no tip resolves is refused
//! there and surfaces to the caller. Nothing between the read and the put
//! yields to a walk (`Cmd::is_local`). The put is the **local write only**
//! ([`AccountStatePlane::put_local`]); the account runtime's publish step
//! ships it, and a sibling's copy joins through the plane arm
//! (`RefusedSchedulingChanges::merge`).

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::{RefusedSchedulingChange, RefusedSchedulingChanges};
use fauna_core::refused_change_rows::{REFUSED_CHANGES_ROW_KEY, decode_refused_changes_row};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_REFUSED_SCHEDULING_CHANGES;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// One write to the refused-change list — the three things anyone does to it.
#[derive(Debug, Clone)]
pub enum RefusedChangeWrite {
    /// The inbound drain refused a change
    /// ([`RefusedSchedulingChanges::record`]).
    Record(Box<RefusedSchedulingChange>),
    /// The owner dismissed the row this key names
    /// ([`RefusedSchedulingChanges::dismiss`]).
    Dismiss(String),
    /// Join a list held elsewhere into the stored one — the refusals a
    /// session recorded before its account store was up
    /// ([`RefusedSchedulingChanges::merge`]).
    Merge(Box<RefusedSchedulingChanges>),
}

/// This account's refused-change list — empty when nothing is stored.
pub async fn read_refused_scheduling_changes<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<RefusedSchedulingChanges> {
    let stored = store
        .state(KIND_REFUSED_SCHEDULING_CHANGES, REFUSED_CHANGES_ROW_KEY)
        .await?;
    match stored {
        Some(entry) if !entry.tombstone => decode(&entry.value),
        _ => Ok(RefusedSchedulingChanges::default()),
    }
}

/// The list among `entries` — the handle's read, over one `states_of_kind`
/// load. A row that does not decode strictly fails loudly.
pub fn refused_scheduling_changes_of(entries: &[StateEntry]) -> Result<RefusedSchedulingChanges> {
    entries
        .iter()
        .find(|e| {
            !e.tombstone
                && e.kind == KIND_REFUSED_SCHEDULING_CHANGES
                && e.key == REFUSED_CHANGES_ROW_KEY
        })
        .map_or_else(
            || Ok(RefusedSchedulingChanges::default()),
            |e| decode(&e.value),
        )
}

fn decode(value: &[u8]) -> Result<RefusedSchedulingChanges> {
    decode_refused_changes_row(REFUSED_CHANGES_ROW_KEY, value)
        .context("the stored refused-scheduling-changes row")
}

/// Apply `write` to the stored list and put the result when its bytes moved.
/// Answers whether the gesture changed anything (a repeat dismissal, a
/// refusal the ceilings cut at once, a covered merge: `false`, no put).
pub async fn write_refused_scheduling_changes<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    write: RefusedChangeWrite,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut list = read_refused_scheduling_changes(store).await?;
    let before = list.encode_row().context("encode refused-change row")?;
    let changed = match write {
        RefusedChangeWrite::Record(row) => list.record(*row),
        RefusedChangeWrite::Dismiss(key) => list.dismiss(&key),
        RefusedChangeWrite::Merge(replica) => {
            list = list.merge(&replica);
            true
        }
    };
    let after = list.encode_row().context("encode refused-change row")?;
    if !changed || after == before {
        return Ok(false);
    }
    fleet
        .put_local(
            &ItemId {
                kind: KIND_REFUSED_SCHEDULING_CHANGES.to_string(),
                key: REFUSED_CHANGES_ROW_KEY.to_string(),
            },
            after,
            // The row is its own record; no outer stamp.
            None,
        )
        .await
        .context("refused-change row: plane put")?;
    Ok(true)
}
