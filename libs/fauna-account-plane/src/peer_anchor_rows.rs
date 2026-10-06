//! The succession witness's peer-anchor cache's production writer and reader
//! — the typed door for `fauna.state.peer-anchors` (`identity-succession.md`
//! § The succession statement → *the peer-profile harvest* owns the anchors;
//! `fauna_core::peer_anchor_rows` owns the rows, their key grammar, the strict
//! decode and the per-row join; `config-dissolution.md` § The `__config`
//! dissolution schedule owns the kind's birth, plane-only, and § Phases and
//! gates → *Bounded rows* the one-row-per-actor shape).
//!
//! **One row per anchored actor per vector**; the composite [`PeerAnchors`]
//! the witness works on is the READ fold over every row of the kind, through
//! the shipped rule ([`PeerAnchors::merge`]) — the one ceiling and the one
//! order included. A write is a **merge on the store thread**: the caller's
//! replica is joined with the stored fold, and each row of the join — which
//! the ceiling has already cut to `MAX_PEER_ANCHOR_ENTRIES` per vector, oldest
//! first — is read, joined with the stored row (the per-row half the plane
//! arm runs on a sibling's row), and put through the fleet plane's REAL writer
//! door only when the join moved it. **So the ceiling holds at the door too:**
//! a row the fold would cut is never put, and a replica cannot grow the store
//! past what it could read back. The kind is `GenerationTip`-sealed, so a put
//! while no tip resolves is refused there and surfaces to the caller. Nothing
//! between the reads and the puts yields to a walk (`Cmd::is_local`). Each put
//! is the **local write only** ([`AccountStatePlane::put_local`]); the account
//! runtime's publish step ships it.
//!
//! Because the write is a join, a caller never loses what the store gained
//! since it last read (another device's harvest, merged in by a walk): it gets
//! the joined fold back. Nothing is ever deleted — a replica that omits an
//! actor drops nothing, a behind replica's head never rewinds a stored one.
//!
//! A replica entry the plane would refuse — outside the writers' shape, or
//! carrying a field this build does not know — is refused HERE, before any
//! put: the door writes only rows every sibling's arm adopts.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::PeerAnchors;
use fauna_core::peer_anchor_rows::{PeerAnchorRow, decode_peer_anchor_row};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_PEER_ANCHORS;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's peer anchors, folded — empty when nothing is stored.
pub async fn read_peer_anchors<B: StoreBackend>(store: &AccountStore<B>) -> Result<PeerAnchors> {
    peer_anchors_of(&store.states_of_kind(KIND_PEER_ANCHORS).await?)
}

/// The fold of every live `fauna.state.peer-anchors` row among `entries` —
/// the handle's read, over one `states_of_kind` load. A row that does not
/// decode strictly, or names another actor, fails loudly: silently skipping
/// it would drop an anchor, and a dropped head is a rewind the rewrite guard
/// exists to refuse.
pub fn peer_anchors_of(entries: &[StateEntry]) -> Result<PeerAnchors> {
    let mut folded = PeerAnchors::default();
    for entry in entries
        .iter()
        .filter(|e| !e.tombstone && e.kind == KIND_PEER_ANCHORS)
    {
        folded
            .fold_row(&entry.key, &entry.value)
            .with_context(|| format!("the stored peer-anchor row {:?}", entry.key))?;
    }
    Ok(folded)
}

/// Join `replica` into the stored anchors and write every row of the join the
/// stored rows do not already hold. Returns the account's fold as it now
/// stands, and whether any put happened.
pub async fn merge_peer_anchors<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    replica: &PeerAnchors,
) -> Result<(PeerAnchors, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    // Refuse the whole write before any put when one entry would be refused.
    for (key, row) in replica.rows() {
        let bytes = row.encode_row().context("encode peer-anchor row")?;
        decode_peer_anchor_row(&key, &bytes)
            .with_context(|| format!("the peer-anchor entry for {key:?}"))?;
    }
    // The join's rows are the ones the read fold keeps — the ceiling, held at
    // the door.
    let joined = read_peer_anchors(store).await?.merge(replica);
    let mut moved = false;
    for (key, row) in joined.rows() {
        moved |= join_row(store, fleet, key, &row).await?;
    }
    Ok((read_peer_anchors(store).await?, moved))
}

/// Read `key`, join `row` into it, and put the result when the join moved the
/// stored bytes. Whether a put happened.
async fn join_row<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    key: String,
    row: &PeerAnchorRow,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let joined = match store.state(KIND_PEER_ANCHORS, &key).await? {
        Some(stored) if !stored.tombstone => {
            let current = decode_peer_anchor_row(&key, &stored.value)
                .with_context(|| format!("the stored peer-anchor row {key:?}"))?;
            let joined = current
                .merge(row)
                .context("peer-anchor row: join")?
                .encode_row()
                .context("encode peer-anchor row")?;
            if joined == stored.value {
                return Ok(false);
            }
            joined
        }
        _ => row.encode_row().context("encode peer-anchor row")?,
    };
    fleet
        .put_local(
            &ItemId {
                kind: KIND_PEER_ANCHORS.to_string(),
                key,
            },
            joined,
            // A row is its own record; no outer stamp.
            None,
        )
        .await
        .context("peer-anchor row: plane put")?;
    Ok(true)
}
