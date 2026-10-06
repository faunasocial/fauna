//! The bind leg's per-replica memory and the holder half's asks
//! (`account-sync-plane.md` § The bind leg, ruling 2; `account-data-taxonomy.md`
//! § The generation machinery → *A holder change re-receipts and never mints*).
//!
//! A device keeps memories about "the nest" that were written against one nest
//! replica and name none. The ruling that replaces them: **what can be asked of
//! the bound nest is asked; what must be remembered is keyed by the replica it
//! was learned from, and is void on any other.** This module holds the parts
//! the pump composes:
//!
//! - **The settled replica** ([`BoundReplica`]): the pinned identity and the
//!   replica id of the nest this store last completed a pass against. A pass
//!   asks the bound nest for its replica id first ([`probe_bound_replica`]);
//!   when the pair differs from the settled one, the pass runs the **bind
//!   verification** ahead of its other legs (the enrollment latch is void and
//!   re-registered, the watermarks are cleared, the rotation chain is read) and
//!   records the new pair only when the pass completes, so a verification cut
//!   short runs again. A nest that names no replica counts as changed at every
//!   assembly.
//! - **The holdings belief** is never remembered: in the first pass after
//!   every assembly — and after every bind verification — the device asks the
//!   holder for the account's wraps ([`holder_holdings`]) and re-deposits what
//!   the reply lacks (`generation_reescrow`). A box restored from a snapshot
//!   keeps its replica id and loses what was deposited since, so a replica
//!   change alone would miss it.
//! - **Recovery-only trust** ([`fetch_verified_ancestors`]): the superseded
//!   ancestors of the pinned identity, proven by the box's own rotation chain,
//!   which the escrow-recovery pass — and only it — admits receipts from.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_protocol::RpcRequester;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE, ItemClass};
use fauna_protocol::generation_escrow::{EscrowGetReply, EscrowGetRequest, KIND_ESCROW_GET};
use fauna_protocol::nest_rotation::{
    ROTATION_CHAIN_KIND, RotationChainReply, RotationChainRequest, verified_ancestors,
};
use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};
use serde::{Deserialize, Serialize};

/// The nest a pass is bound to, as the settled-replica record names it: the
/// identity the app pinned for it (the trusted holder set, sorted) and the
/// replica id its feed answered with (`None` — a nest that names none).
///
/// A rebuilt box is a new replica id under an unchanged identity; a rotated
/// box is the same replica id under a new identity; a second nest differs in
/// both. Each is a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundReplica {
    pub pinned: Vec<[u8; 32]>,
    pub replica: Option<Vec<u8>>,
}

/// The stored form — hex strings, so the record reads the same in every
/// backend's meta table.
#[derive(Serialize, Deserialize)]
struct SettledRecord {
    pinned: Vec<String>,
    replica: Option<String>,
}

impl BoundReplica {
    /// The pair for `pinned` (in any order) and `replica`.
    #[must_use]
    pub fn new(pinned: &[[u8; 32]], replica: Option<Vec<u8>>) -> Self {
        let mut pinned = pinned.to_vec();
        pinned.sort_unstable();
        pinned.dedup();
        Self { pinned, replica }
    }

    fn encode(&self) -> Result<Vec<u8>> {
        fauna_core::encoding::canonical_encode(&SettledRecord {
            pinned: self.pinned.iter().map(fauna_core::hex32::encode).collect(),
            replica: self.replica.as_deref().map(hex::encode),
        })
        .map(|b| b.to_vec())
        .context("encoding the settled replica")
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let record: SettledRecord = fauna_core::encoding::canonical_decode(bytes).ok()?;
        let pinned = record
            .pinned
            .iter()
            .map(|h| fauna_core::hex32::decode(h).ok())
            .collect::<Option<Vec<_>>>()?;
        let replica = match record.replica {
            Some(h) => Some(hex::decode(h).ok()?),
            None => None,
        };
        Some(Self::new(&pinned, replica))
    }
}

/// The settled replica this store recorded, if any. A record this build cannot
/// read is no record: the next pass verifies and writes a fresh one.
pub async fn settled_replica<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<Option<BoundReplica>> {
    Ok(store
        .settled_replica()
        .await?
        .as_deref()
        .and_then(BoundReplica::decode))
}

/// Record `bound` as the settled replica — only once a pass against it has
/// completed.
pub async fn record_settled_replica<B: StoreBackend>(
    store: &AccountStore<B>,
    bound: &BoundReplica,
) -> Result<()> {
    store.record_settled_replica(&bound.encode()?).await
}

/// Ask the bound nest which replica it is: one class-2 feed request on the
/// fleet scope that selects no row (`sealed_under` an all-zero generation id,
/// which no mint can have — a generation id is a hash), answered with the
/// nest's replica id and nothing else. Rides the owner session: the device
/// principal's own session is exactly what a rebuilt box refuses.
pub async fn probe_bound_replica<R>(rpc: &R) -> Result<Option<Vec<u8>>>
where
    R: RpcRequester,
{
    let reply: SyncChangesListReply = rpc
        .request(
            "fauna.sync.changes.list",
            SyncChangesListRequest {
                item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
                scope: Some(ACCOUNT_STATE_FLEET_SCOPE.to_string()),
                frontier: Some(Default::default()),
                sealed_under: Some(vec![0u8; 32].into()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("replica probe (fauna.sync.changes.list): {e}"))?;
    Ok(reply.replica_id.map(|id| id.to_vec()))
}

/// Whether a pass bound to `bound` owes the bind verification, given what the
/// store settled and what this assembly already verified.
///
/// Nothing settled yet is a **first bind**, not a change: nothing this store
/// remembers can be attributed to another replica — a fresh store remembers
/// nothing, and a stale latch is voided by the device handshake's own
/// `not_registered` answer — so the pass adopts the bound replica as settled
/// when it completes, and a sign-in's just-registered grant is not
/// re-registered.
#[must_use]
pub fn needs_verification(
    settled: Option<&BoundReplica>,
    bound: &BoundReplica,
    memo: &BindMemo,
) -> bool {
    let Some(settled) = settled else {
        return false;
    };
    if bound.replica.is_none() {
        // A nest that names no replica cannot be told apart from another one:
        // changed at every assembly.
        return !memo.unnamed_verified.load(Ordering::SeqCst);
    }
    settled != bound
}

/// Void both state scopes' serve-order watermarks and the feed coordinates
/// kept beside them (`AccountStore::void_nest_watermark`) — the
/// verification's first memory. The walk voids a bank from another replica by
/// itself (`page_walk::WatermarkCursor`); voiding here, ahead of the pass's
/// reconcile as the void's own law asks, is what makes a rotated box (same
/// replica id) start from a full walk too.
pub async fn clear_watermarks<B: StoreBackend>(store: &AccountStore<B>) -> Result<()> {
    for scope in [ACCOUNT_STATE_SCOPE, ACCOUNT_STATE_FLEET_SCOPE] {
        store
            .void_nest_watermark(scope)
            .await
            .with_context(|| format!("voiding the {scope} watermark"))?;
    }
    Ok(())
}

/// The superseded ancestors of every identity in `pinned`, proven by the bound
/// nest's rotation chain (`fauna.auth.rotation_chain`, verified hop by hop to
/// the pinned head — [`verified_ancestors`]). Empty for a box that never
/// rotated.
pub async fn fetch_verified_ancestors<R>(rpc: &R, pinned: &[[u8; 32]]) -> Result<Vec<[u8; 32]>>
where
    R: RpcRequester,
{
    let reply: RotationChainReply = rpc
        .request(ROTATION_CHAIN_KIND, RotationChainRequest::default())
        .await
        .map_err(|e| anyhow::anyhow!("rotation chain: {e}"))?;
    let mut ancestors = Vec::new();
    for head in pinned {
        for id in verified_ancestors(&reply.chain, head) {
            if !pinned.contains(&id) && !ancestors.contains(&id) {
                ancestors.push(id);
            }
        }
    }
    Ok(ancestors)
}

/// The generations whose wraps the holder holds for this account — one
/// unfiltered `fauna.generation.escrow.get` (`account-data-taxonomy.md`
/// § The generation machinery → *A holder change re-receipts and never mints*,
/// (3): a receipt proves a deposit, not a holding).
pub async fn holder_holdings<R>(rpc: &R) -> Result<BTreeSet<[u8; 32]>>
where
    R: RpcRequester,
{
    let reply: EscrowGetReply = rpc
        .request(KIND_ESCROW_GET, EscrowGetRequest::default())
        .await
        .map_err(|e| anyhow::anyhow!("holdings check (fauna.generation.escrow.get): {e}"))?;
    Ok(reply
        .wraps
        .iter()
        .filter_map(|w| <[u8; 32]>::try_from(w.generation_id.as_slice()).ok())
        .collect())
}

/// What one pass's replica check found (the pump's `bind` slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindPass {
    /// Bound to the settled replica: nothing to verify.
    Settled,
    /// Nothing was settled yet (a fresh store):
    /// `adopted` says whether the completed pass recorded the bound replica.
    FirstBind { adopted: bool },
    /// Bound to another replica (or to one that names none): the bind
    /// verification ran ahead of the other legs; `settled` says whether the
    /// pass completed it and recorded the new replica as settled.
    Verified { settled: bool },
}

/// What one assembly has done of the bind leg — fresh at every assembly, so
/// "the first pass after every assembly" is a pass that finds it unset.
#[derive(Debug, Default)]
pub struct BindMemo {
    /// The holdings check has run to completion against the bound holder
    /// since this assembly (or since the last bind verification re-armed it).
    holdings_checked: AtomicBool,
    /// A nest that names no replica has been verified once this assembly.
    unnamed_verified: AtomicBool,
    /// The replica an unsettled verification targets — its one-shot legs
    /// (the watermark clear, the holdings re-arm) ran for it; only what failed
    /// is retried while it stays unsettled.
    verifying: Mutex<Option<BoundReplica>>,
    /// The recovery-only ancestors, once read this assembly (`None` = not yet).
    ancestors: Mutex<Option<Vec<[u8; 32]>>>,
    /// The unsettled replica a **seed pass** of this assembly registered the
    /// machine at. A seed pass settles nothing, so the replica stays "other
    /// than the settled one" until the engine holder's pass settles it — and
    /// for a nest that names no replica, for the whole assembly; this is what
    /// makes the latch void once per target rather than at every seed pass.
    seed_registered: Mutex<Option<BoundReplica>>,
}

impl BindMemo {
    /// Whether this assembly still owes the holdings check.
    #[must_use]
    pub fn holdings_due(&self) -> bool {
        !self.holdings_checked.load(Ordering::SeqCst)
    }

    /// The holdings check ran to completion.
    pub fn holdings_done(&self) {
        self.holdings_checked.store(true, Ordering::SeqCst);
    }

    /// Whether an unsettled verification already targets `bound` — its
    /// one-shot legs ran.
    #[must_use]
    pub fn verifying(&self, bound: &BoundReplica) -> bool {
        self.verifying
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            == Some(bound)
    }

    /// A bind verification toward `bound` began (its watermark clear landed):
    /// every per-replica belief is asked again — the holdings, and the
    /// ancestry (the pin may have moved with it).
    pub fn begin(&self, bound: &BoundReplica) {
        *self.verifying.lock().unwrap_or_else(|p| p.into_inner()) = Some(bound.clone());
        self.holdings_checked.store(false, Ordering::SeqCst);
        self.set_ancestors(None);
    }

    /// The pass that verified `bound` completed: it is settled.
    pub fn settle(&self, bound: &BoundReplica) {
        *self.verifying.lock().unwrap_or_else(|p| p.into_inner()) = None;
        if bound.replica.is_none() {
            self.unnamed_verified.store(true, Ordering::SeqCst);
        }
    }

    /// Whether a seed pass of this assembly already registered at `bound`.
    #[must_use]
    pub fn seed_registered_at(&self, bound: &BoundReplica) -> bool {
        self.seed_registered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            == Some(bound)
    }

    /// A seed pass put the machine's grant on `bound`.
    pub fn note_seed_registered(&self, bound: &BoundReplica) {
        *self
            .seed_registered
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(bound.clone());
    }

    /// The recovery-only ancestors read this assembly, if read.
    #[must_use]
    pub fn ancestors(&self) -> Option<Vec<[u8; 32]>> {
        self.ancestors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Record the ancestors read (`None` forgets them — the pin moved).
    pub fn set_ancestors(&self, ancestors: Option<Vec<[u8; 32]>>) {
        *self.ancestors.lock().unwrap_or_else(|p| p.into_inner()) = ancestors;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settled_record_round_trips_and_ignores_pin_order() {
        let bound = BoundReplica::new(&[[2; 32], [1; 32]], Some(vec![9; 16]));
        assert_eq!(bound.pinned, vec![[1; 32], [2; 32]]);
        let bytes = bound.encode().unwrap();
        assert_eq!(BoundReplica::decode(&bytes), Some(bound.clone()));
        assert_eq!(
            BoundReplica::new(&[[1; 32], [2; 32]], Some(vec![9; 16])),
            bound
        );
        assert_eq!(BoundReplica::decode(b"not a record"), None);
    }

    #[test]
    fn a_rebuilt_a_rotated_and_a_second_nest_each_owe_the_verification() {
        let memo = BindMemo::default();
        let settled = BoundReplica::new(&[[1; 32]], Some(vec![1; 16]));
        assert!(!needs_verification(Some(&settled), &settled, &memo));
        let rebuilt = BoundReplica::new(&[[1; 32]], Some(vec![2; 16]));
        let rotated = BoundReplica::new(&[[3; 32]], Some(vec![1; 16]));
        let second = BoundReplica::new(&[[4; 32]], Some(vec![5; 16]));
        for other in [&rebuilt, &rotated, &second] {
            assert!(needs_verification(Some(&settled), other, &memo));
        }
        assert!(
            !needs_verification(None, &settled, &memo),
            "nothing settled yet is a first bind: adopted, never verified"
        );
    }

    #[test]
    fn a_nest_that_names_no_replica_is_verified_once_per_assembly() {
        let memo = BindMemo::default();
        let unnamed = BoundReplica::new(&[[1; 32]], None);
        assert!(needs_verification(Some(&unnamed), &unnamed, &memo));
        memo.begin(&unnamed);
        assert!(
            needs_verification(Some(&unnamed), &unnamed, &memo),
            "begun is not settled: a verification cut short runs again"
        );
        memo.settle(&unnamed);
        assert!(!needs_verification(Some(&unnamed), &unnamed, &memo));
        assert!(
            needs_verification(Some(&unnamed), &unnamed, &BindMemo::default()),
            "a fresh assembly verifies again"
        );
    }

    #[test]
    fn a_verification_re_arms_the_holdings_check_once_per_target() {
        let memo = BindMemo::default();
        assert!(memo.holdings_due());
        memo.holdings_done();
        assert!(!memo.holdings_due());
        let bound = BoundReplica::new(&[[1; 32]], Some(vec![1; 16]));
        assert!(!memo.verifying(&bound));
        memo.set_ancestors(Some(vec![[9; 32]]));
        memo.begin(&bound);
        assert!(memo.holdings_due());
        assert_eq!(memo.ancestors(), None, "the ancestry is read again");
        assert!(memo.verifying(&bound));
        memo.settle(&bound);
        assert!(!memo.verifying(&bound));
    }

    #[test]
    fn a_seed_pass_registers_once_per_replica_and_arms_no_verification() {
        let memo = BindMemo::default();
        memo.holdings_done();
        let rebuilt = BoundReplica::new(&[[1; 32]], Some(vec![2; 16]));
        assert!(!memo.seed_registered_at(&rebuilt));
        memo.note_seed_registered(&rebuilt);
        assert!(memo.seed_registered_at(&rebuilt));
        assert!(
            !memo.verifying(&rebuilt) && !memo.holdings_due(),
            "the verification's one-shot legs stay the engine holder's"
        );
        let again = BoundReplica::new(&[[1; 32]], Some(vec![3; 16]));
        assert!(
            !memo.seed_registered_at(&again),
            "a box rebuilt once more is another replica"
        );
    }
}
