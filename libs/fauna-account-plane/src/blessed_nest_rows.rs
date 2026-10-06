//! The user's per-nest blessing verdicts' production writer and reader — the
//! typed door for `fauna.state.blessed-nests` (`docs/goal/ui/nests.md`
//! § Expiry / renewal → *Duration and blessing* owns what a blessing is;
//! `fauna_core::blessed_nest_rows` owns the rows, their key grammar, the
//! strict decode and the per-row join; `config-dissolution.md` § The
//! `__config` dissolution schedule owns the kind's birth, plane-only, and
//! § Phases and gates → *Bounded rows* the one-row-per-nest shape).
//!
//! **One row per nest**, keyed on the nest id's lowercase hex64; the
//! composite list the renewal machinery works on is the READ fold over every
//! row of the kind, through the shipped rule (`merge_blessed_nests`), sorted
//! by `nest_id`. A write is a **verdict on the store thread**: the stored row
//! for the nest is read, the new verdict stamped over it
//! ([`BlessedNest::verdict`] — never at or behind the stored verdict, so a
//! toggle always supersedes the state it was made against), and put through
//! the fleet plane's REAL writer door only when the verdict changes it; a
//! re-assert of the current verdict writes nothing. The kind is
//! `GenerationTip`-sealed, so a put while no tip resolves is refused there
//! and surfaces to the caller. Nothing between the read and the put yields to
//! a walk (`Cmd::is_local`). Each put is the **local write only**
//! ([`AccountStatePlane::put_local`]); the account runtime's publish step
//! ships it.
//!
//! Nothing is ever deleted: an un-blessing is a row with `blessed: false`,
//! so the newer verdict can win a merge against a sibling's older blessing.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::blessed_nest_rows::{
    blessed_nest_key, decode_blessed_nest_row, fold_blessed_nest_row,
};
use fauna_core::data::BlessedNest;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_BLESSED_NESTS;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's blessing verdicts, folded and sorted by `nest_id` — empty
/// when nothing is stored.
pub async fn read_blessed_nests<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<Vec<BlessedNest>> {
    blessed_nests_of(&store.states_of_kind(KIND_BLESSED_NESTS).await?)
}

/// The fold of every live `fauna.state.blessed-nests` row among `entries` —
/// the handle's read, over one `states_of_kind` load. A row that does not
/// decode strictly, or names another nest, fails loudly: silently skipping
/// it would drop a verdict, and a dropped un-blessing lets a sibling's older
/// blessing renew a box the user withdrew.
pub fn blessed_nests_of(entries: &[StateEntry]) -> Result<Vec<BlessedNest>> {
    let mut folded = Vec::new();
    for entry in entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_BLESSED_NESTS)
    {
        fold_blessed_nest_row(&mut folded, &entry.key, &entry.value)
            .with_context(|| format!("the stored blessed-nest row {:?}", entry.key))?;
    }
    Ok(folded)
}

/// The stored verdict for `nest_id`, if any.
async fn stored_verdict<B: StoreBackend>(
    store: &AccountStore<B>,
    nest_id: &[u8; 32],
) -> Result<Option<BlessedNest>> {
    let key = blessed_nest_key(nest_id);
    match store.state(KIND_BLESSED_NESTS, &key).await? {
        Some(stored) if !stored.tombstone => Ok(Some(
            decode_blessed_nest_row(&key, &stored.value)
                .with_context(|| format!("the stored blessed-nest row {key:?}"))?,
        )),
        _ => Ok(None),
    }
}

/// Whether `nest_id` is blessed — one row read; an absent row is not.
pub async fn read_nest_blessed<B: StoreBackend>(
    store: &AccountStore<B>,
    nest_id: &[u8; 32],
) -> Result<bool> {
    Ok(stored_verdict(store, nest_id)
        .await?
        .is_some_and(|v| v.blessed))
}

/// Record the user's verdict for `nest_id` at `now` over the stored row
/// ([`BlessedNest::verdict`]), and put it when it changes the row. Whether a
/// put happened — a re-assert of the current verdict writes nothing.
pub async fn set_nest_blessed<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    nest_id: &[u8; 32],
    blessed: bool,
    now: u64,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let prior = stored_verdict(store, nest_id).await?;
    let Some(verdict) = BlessedNest::verdict(prior.as_ref(), nest_id, blessed, now) else {
        return Ok(false);
    };
    fleet
        .put_local(
            &ItemId {
                kind: KIND_BLESSED_NESTS.to_string(),
                key: verdict.plane_key(),
            },
            verdict.encode_row().context("encode blessed-nest row")?,
            // A row is its own record; no outer stamp.
            None,
        )
        .await
        .context("blessed-nest row: plane put")?;
    Ok(true)
}
