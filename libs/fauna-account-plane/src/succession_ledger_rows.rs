//! The succession ledger's production writer and reader — the typed door for
//! `fauna.state.succession-ledger` (`fauna_core::succession_ledger` owns the
//! values, their key grammar and their joins; `config-dissolution.md` §
//! Phases and gates → *Bounded rows* → *The ledger* owns the shape, the
//! read-time signer clause and the succession write).
//!
//! **One row per entity plus the ONE `chain` row**; the composite
//! `SuccessionLedger` the aftermath and the grant surfaces work on is the
//! READ fold over every row of the kind, as the runtime holding `self_actor`
//! sees it — the chain seeded with that identity, and only the events the
//! folded chain signed kept ([`SuccessionLedger::fold`]).
//!
//! A write is a **merge on the store thread**, per row: each record is read,
//! joined with the stored row (the same per-row arm the plane runs on a
//! sibling's row — `SuccessionLedgerRecord::merge`) and put through the fleet
//! plane's REAL writer door only when the join moved it. Before any put the
//! door refuses, whole: an event verifying against no id of the ATTESTED
//! set (this identity plus the predecessors whose keys the caller holds —
//! `account-data-taxonomy.md` § The generation machinery → *The source of
//! `prior`*; never a chain a row asserts), a row the plane arm would refuse
//! at first contact (a value not naming its key, a malformed chain), and a
//! chain the stored one forks from. The kind is `GenerationTip`-sealed, so a
//! put while no tip resolves is refused at the door and surfaces to the
//! caller, whose leg retries. Each put is the **local write only**
//! ([`AccountStatePlane::put_local`]); the account runtime's publish step
//! ships it.
//!
//! The succession write ([`repoint_succession_ledger`],
//! [`raise_grant_marks`]) re-seals nothing: pre-succession rows stay under
//! the generations the succession rider re-escrows, and the plane is one
//! logical namespace across generations, so the blob's per-predecessor
//! replica fold has no twin here. The destination marks are the
//! `fauna.state.backup` slice's, not this door's.

use anyhow::{Context, Result, bail};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::StateEntry;
use fauna_core::data::{GrantUnattestedMark, UnattestedVerdict};
use fauna_core::identity::ActorId;
use fauna_core::succession_ledger::{
    CHAIN_KEY, SuccessionLedger, SuccessionLedgerRecord, decode_succession_ledger_row,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER;

use crate::account_state_plane::{AccountStatePlane, ItemId};

/// This account's succession ledger as the runtime holding `self_actor`
/// reads it — its own identity alone when no row rests yet.
pub async fn read_succession_ledger<B: StoreBackend>(
    store: &AccountStore<B>,
    self_actor: ActorId,
) -> Result<SuccessionLedger> {
    succession_ledger_of(
        &store.states_of_kind(KIND_SUCCESSION_LEDGER).await?,
        self_actor,
    )
}

/// The READ fold of every live `fauna.state.succession-ledger` row among
/// `entries` — the handle's read, over one `states_of_kind` load. A row whose
/// key does not parse or whose value does not decode, re-encode to its own
/// bytes or name its key fails loudly: silently skipping it would let the
/// next write put a bare replica over what it held.
pub fn succession_ledger_of(
    entries: &[StateEntry],
    self_actor: ActorId,
) -> Result<SuccessionLedger> {
    SuccessionLedger::fold(
        self_actor,
        entries
            .iter()
            .filter(|e| !e.tombstone && e.kind == KIND_SUCCESSION_LEDGER)
            .map(|e| (e.key.as_str(), e.value.as_slice())),
    )
    .context("the stored succession-ledger rows")
}

/// Join `record` into its stored row and put the join when it moved the row.
/// Returns whether a put happened.
async fn put_row<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    record: &SuccessionLedgerRecord,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let key = record.key().context("succession-ledger row: key")?.render();
    let joined = match store.state(KIND_SUCCESSION_LEDGER, &key).await? {
        Some(entry) if !entry.tombstone => {
            let stored = decode_succession_ledger_row(&key, &entry.value)
                .with_context(|| format!("the stored succession-ledger row {key:?}"))?;
            let joined = stored
                .merge(record)
                .with_context(|| format!("succession-ledger row {key:?}: join"))?
                .encode()
                .context("encode succession-ledger row")?;
            if joined == entry.value {
                return Ok(false);
            }
            joined
        }
        _ => record.encode().context("encode succession-ledger row")?,
    };
    fleet
        .put_local(
            &ItemId {
                kind: KIND_SUCCESSION_LEDGER.to_string(),
                key,
            },
            joined,
            // Every row is its own merge state; no outer stamp.
            None,
        )
        .await
        .context("succession-ledger row: plane put")?;
    Ok(true)
}

/// Join each entity of `replica` into its stored row and write every row the
/// join moved. `attested` is the predecessors whose keys this runtime holds
/// (`AccountRegistry`'s attested predecessors); the attested signer set is
/// `self_actor` plus them. Returns the account's fold as `self_actor` now
/// reads it, and whether any put happened.
///
/// # Errors
/// The door's refusals (module docs), before any put: an unattested event,
/// a row the plane arm would refuse, a forked chain — and the writer door's
/// own, a put while no generation tip resolves.
pub async fn merge_succession_ledger<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    self_actor: ActorId,
    replica: &SuccessionLedger,
    attested: &[ActorId],
) -> Result<(SuccessionLedger, bool)>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let signers: Vec<ActorId> = std::iter::once(self_actor)
        .chain(attested.iter().copied())
        .collect();
    let rows = replica.rows().context("succession-ledger replica: rows")?;
    // Every refusal runs before the first put, so a refused merge leaves
    // nothing behind.
    for (key, record) in &rows {
        if let SuccessionLedgerRecord::Event(e) = record
            && !signers.iter().any(|id| e.verify(id).is_ok())
        {
            bail!(
                "succession-ledger door: the event at {key:?} verifies against no attested identity"
            );
        }
        decode_succession_ledger_row(key, &record.encode()?)
            .with_context(|| format!("succession-ledger door: the row at {key:?}"))?;
        if key == CHAIN_KEY
            && let (SuccessionLedgerRecord::Chain(ours), Some(entry)) = (
                record,
                store.state(KIND_SUCCESSION_LEDGER, CHAIN_KEY).await?,
            )
            && !entry.tombstone
            && let SuccessionLedgerRecord::Chain(stored) =
                decode_succession_ledger_row(CHAIN_KEY, &entry.value)
                    .context("the stored succession-ledger chain row")?
        {
            stored
                .join(ours)
                .context("succession-ledger door: the replica's chain forks from the stored one")?;
        }
    }
    let mut moved = false;
    for (_, record) in &rows {
        moved |= put_row(store, fleet, record).await?;
    }
    Ok((read_succession_ledger(store, self_actor).await?, moved))
}

/// **The succession write — the plane form of the aftermath's former
/// `__config` re-seal leg.** The successor's runtime folds the chain as `successor` reads it,
/// applies the one re-point rule (`ChainState::repoint_to_successor`:
/// `retired` recorded, `successor` current) and puts `chain` — which the
/// chain arm joins with the predecessor-written row through the gate's third
/// arm. `retired` is the ATTESTED predecessor id, never one a row asserts.
/// Returns whether a put happened (a second call writes nothing).
///
/// # Errors
/// The stored chain forks from the re-pointed one, or the writer door
/// refuses (no successor tip resolves yet — the leg stays owed and retries).
pub async fn repoint_succession_ledger<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    retired: ActorId,
    successor: ActorId,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut chain = read_succession_ledger(store, successor).await?.chain();
    chain.repoint_to_successor(retired, successor);
    put_row(store, fleet, &SuccessionLedgerRecord::Chain(chain)).await
}

/// **The grant-mark raise** — the grant half of the aftermath's
/// carried-across marking, on the plane: an `Open` mark keyed on
/// `(grant, predecessor)` for every grant live in the ledger as `self_actor`
/// reads it **whose latest live event `predecessor` signed** — the grants
/// that identity's window carried across. Idempotent by the key itself: a
/// mark the owner already decided joins to the stored verdict and puts
/// nothing, and a later succession is a different predecessor and raises its
/// own. Returns whether any put happened.
///
/// **Why the provenance filter.** The blob stamped every live grant because
/// it ran once, at the re-key, before the successor could have minted
/// anything — every live grant was predecessor-era by construction. This
/// raise re-runs at every store-ready (`succession-aftermath.md` § Re-key
/// scope → *Adjudicating what the aftermath carries across*, the 2026-09-30
/// paragraph), so the construction argument is gone: a grant the successor
/// minted, renewed or re-minted since is signed by the successor, and marking
/// it would ask the owner to adjudicate their own act. The signature is the
/// provenance the blob's timing stood in for.
///
/// # Errors
/// A stored row the fold refuses, or the writer door's refusal.
pub async fn raise_grant_marks<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    self_actor: ActorId,
    predecessor: ActorId,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let ledger = read_succession_ledger(store, self_actor).await?;
    let mut moved = false;
    for event in fauna_core::grant_event::latest_live_events_of(&ledger.grant_events)
        .into_iter()
        .filter(|e| e.verify(&predecessor).is_ok())
    {
        let mark = SuccessionLedgerRecord::GrantMark(GrantUnattestedMark {
            grant_id: event.grant_id.clone(),
            predecessor,
            verdict: UnattestedVerdict::Open,
        });
        moved |= put_row(store, fleet, &mark).await?;
    }
    Ok(moved)
}
