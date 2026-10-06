//! The custody ceremony's record-then-act state's production writer and
//! reader — the typed door for `fauna.state.custody-ceremony`
//! (`fauna_core::custody_ceremony` owns the records and their join;
//! `fauna_core::custody_ceremony_rows` owns the rows' key grammar and strict
//! decode; `account-data-plane.md` § Replica posture → *The custody grant +
//! ceremony* owns the concept; `config-dissolution.md` § The `__config`
//! dissolution schedule owns the kind's birth, plane-only, and § Phases and
//! gates → *Bounded rows* the one-row-per-record shape).
//!
//! Not to be confused with [`crate::custody_rows`], the door for the
//! ceremony's two REGISTRY kinds (`fauna.state.custodian-endpoints`,
//! `fauna.state.custodies-held`) — the rows the ceremony's acts produce.
//!
//! **One row per ceremony side-record** (`granted/<grant id>`,
//! `held/<grant id>`); the composite `CustodyConfig` the ceremony code works
//! on is the READ fold over every row of the kind. A write is a **merge on
//! the store thread**, per row: the caller's replica is split into its
//! records, each is read, joined with the stored row (`CustodyRecord::merge`
//! — the half the plane arm runs on a sibling's row), and put through the
//! fleet plane's REAL writer door only when the join moved it. Nothing is
//! ever removed: the composite is a union, and a replica missing a record
//! leaves its row be. The kind is `GenerationTip`-sealed, so a put while no
//! tip resolves is refused there and surfaces to the caller; the same tip
//! seals the ceremony's registry rows, so the ceremony gains no new
//! precondition. Nothing between the reads and the puts yields to a walk
//! (`Cmd::is_local`). Each put is the **local write only**
//! ([`AccountStatePlane::put_local`]); the account runtime's publish step
//! ships it.
//!
//! Because the write is a join, a caller never loses what the store gained
//! since it last read (another device's captured accept, merged in by a
//! walk): it gets the joined fold back and folds it into its own replica.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::custody_ceremony::CustodyConfig;
use fauna_core::custody_ceremony_rows::decode_custody_row;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_CUSTODY_CEREMONY;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's custody-ceremony records, folded — empty when there are
/// none.
pub async fn read_custody<B: StoreBackend>(store: &AccountStore<B>) -> Result<CustodyConfig> {
    custody_of(&store.states_of_kind(KIND_CUSTODY_CEREMONY).await?)
}

/// The fold of every live `fauna.state.custody-ceremony` row among
/// `entries` — the handle's read, over one `states_of_kind` load. A row whose
/// key does not parse, whose value does not decode exactly, or whose value
/// names a different ceremony than its key fails loudly: silently skipping it
/// would let the next write put a bare replica over a ceremony it held.
pub fn custody_of(entries: &[StateEntry]) -> Result<CustodyConfig> {
    let mut folded = CustodyConfig::default();
    for entry in entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_CUSTODY_CEREMONY)
    {
        folded
            .fold_row(&entry.key, &entry.value)
            .with_context(|| format!("the stored custody ceremony row {:?}", entry.key))?;
    }
    Ok(folded)
}

/// Join each record of `replica` into its stored row and write every row the
/// join moved. Returns the account's fold as it now stands, and whether any
/// put happened.
pub async fn merge_custody<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    replica: &CustodyConfig,
) -> Result<(CustodyConfig, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut moved = false;
    for (key, record) in replica.rows().context("custody ceremony: split")? {
        let joined = match store.state(KIND_CUSTODY_CEREMONY, &key).await? {
            Some(entry) if !entry.tombstone => {
                let stored = decode_custody_row(&key, &entry.value)
                    .with_context(|| format!("the stored custody ceremony row {key:?}"))?;
                let joined = stored
                    .merge(&record)
                    .context("custody ceremony row: join")?
                    .encode()
                    .context("encode custody ceremony row")?;
                if joined == entry.value {
                    continue;
                }
                joined
            }
            _ => record.encode().context("encode custody ceremony row")?,
        };
        fleet
            .put_local(
                &ItemId {
                    kind: KIND_CUSTODY_CEREMONY.to_string(),
                    key,
                },
                joined,
                // Every ceremony record carries its own stamps; no outer one.
                None,
            )
            .await
            .context("custody ceremony row: plane put")?;
        moved = true;
    }
    Ok((read_custody(store).await?, moved))
}
