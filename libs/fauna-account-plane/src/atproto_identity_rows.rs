//! The account's ATProto identity custody's production writer and reader —
//! the typed door for `fauna.state.atproto-identity`
//! (`fauna_core::data::AtprotoIdentityConfig` owns the value and its merge;
//! `fauna_core::atproto_identity_rows` owns the rows' key grammar;
//! `atproto-pds-bridge.md` owns the concept; `config-dissolution.md` § The
//! `__config` dissolution schedule owns the kind's birth, plane-only, and
//! § Phases and gates → *Bounded rows* the one-row-per-element shape).
//!
//! **One row per list element** (`key/<pubkey_did_key>`, `consent/<did>`,
//! `intent/<did>/<contested_op_cid>`, `named/<did>`); the composite
//! `AtprotoIdentityConfig` the custody code works on is the READ fold over
//! every row of the kind. A write is a **merge on the store thread**, per
//! row: the caller's replica is split into its elements, each is read,
//! joined with the stored row (`AtprotoIdentityRecord::merge` — the half the
//! plane arm runs on a sibling's row), and put through the fleet plane's REAL
//! writer door only when the join moved it. Nothing is ever removed: the
//! composite is a union, and a replica missing an element leaves its row be.
//! The kind is `GenerationTip`-sealed, so a put while no tip resolves is
//! refused there and surfaces to the caller. Nothing between the reads and
//! the puts yields to a walk (`Cmd::is_local`). Each put is the **local write
//! only** ([`AccountStatePlane::put_local`]); the account runtime's publish
//! step ships it.
//!
//! Because the write is a join, a caller never loses what the store gained
//! since it last read (another device's rotation key, merged in by a walk):
//! it gets the joined fold back and folds it into its own replica. The
//! senior rotation key exists nowhere but these rows, which is why no path
//! here can shrink them.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::atproto_identity_rows::decode_atproto_identity_row;
use fauna_core::data::AtprotoIdentityConfig;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_ATPROTO_IDENTITY;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's ATProto identity custody, folded — empty when none rests.
pub async fn read_atproto_identity<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<AtprotoIdentityConfig> {
    atproto_identity_of(&store.states_of_kind(KIND_ATPROTO_IDENTITY).await?)
}

/// The fold of every live `fauna.state.atproto-identity` row among
/// `entries` — the handle's read, over one `states_of_kind` load. A row whose
/// key does not parse, whose value does not decode exactly, or whose value
/// names a different element than its key fails loudly: silently skipping it
/// would hide a rotation key the account holds.
pub fn atproto_identity_of(entries: &[StateEntry]) -> Result<AtprotoIdentityConfig> {
    let mut folded = AtprotoIdentityConfig::default();
    for entry in entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_ATPROTO_IDENTITY)
    {
        folded
            .fold_row(&entry.key, &entry.value)
            .with_context(|| format!("the stored atproto identity row {:?}", entry.key))?;
    }
    Ok(folded)
}

/// Join each element of `replica` into its stored row and write every row
/// the join moved. Returns the account's fold as it now stands, and whether
/// any put happened.
pub async fn merge_atproto_identity<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    replica: &AtprotoIdentityConfig,
) -> Result<(AtprotoIdentityConfig, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut moved = false;
    for (key, record) in replica.rows().context("atproto identity: split")? {
        let joined = match store.state(KIND_ATPROTO_IDENTITY, &key).await? {
            Some(entry) if !entry.tombstone => {
                let stored = decode_atproto_identity_row(&key, &entry.value)
                    .with_context(|| format!("the stored atproto identity row {key:?}"))?;
                let joined = stored
                    .merge(&record)
                    .context("atproto identity row: join")?
                    .encode()
                    .context("encode atproto identity row")?;
                if joined == entry.value {
                    continue;
                }
                joined
            }
            _ => record.encode().context("encode atproto identity row")?,
        };
        fleet
            .put_local(
                &ItemId {
                    kind: KIND_ATPROTO_IDENTITY.to_string(),
                    key,
                },
                joined,
                // A per-field CRDT row: the join needs no outer stamp.
                None,
            )
            .await
            .context("atproto identity row: plane put")?;
        moved = true;
    }
    Ok((read_atproto_identity(store).await?, moved))
}
