//! **The capability reconcile sweep** — a step of the engine holder's full
//! pump pass, behind the fleet walk (`docs/goal/ui/nests.md` § Trust facet —
//! grants → *Reconcile*, the *Exactly one consumer* and *Automatic and silent,
//! and only where the walk is* bullets, which own every constraint below).
//!
//! The sweep revokes, on the bound nest, every grant row the account's signed
//! grant log does not hold live. It judges a nest-held id against THIS
//! replica's copy of the log, and that copy lags a sibling's fresh `Mint`
//! until the fleet walk pulls it — so the step is split around the walk:
//!
//! 1. [`enumerate`] — `fauna.capabilities.reconcile`, **before** the fleet
//!    walk. The rows it names were deposited after their `Mint` reached the
//!    nest, so the walk that follows serves every one of those events.
//! 2. the pass's fleet walk ([`super::pass::pump`]).
//! 3. [`judge_and_revoke`] — only when the walk completed and left no row
//!    unopened for want of a key: fold the ledger fresh, judge with
//!    [`grant_log::unrecognized_grant_ids`] (the per-owner cap bound lives in
//!    it), revoke each id independently.
//!
//! The answer feeds nothing but revokes aimed back at the answering nest: no
//! `GrantEvent` is appended and nothing from it is written to the store. The
//! seed pass, the nudge walk and the publish step never run it, and neither
//! does a non-holder (its pass is skipped whole — its replica converges
//! through a co-located holder it cannot observe).

use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_client_capabilities::grant_log;
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_core::identity::ActorId;
use fauna_protocol::RpcRequester;

use crate::account_state_plane::WalkReport;

/// What the pass's capability reconcile sweep did
/// ([`super::PumpReport::capability_sweep`]). Local diagnostics only: the log,
/// the store and both trust-facet lenses are untouched by it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CapabilitySweep {
    /// Grant ids the bound nest answered the enumerate with (ill-formed ids
    /// already dropped by [`CapabilitiesClient::reconcile`]).
    pub enumerated: usize,
    /// Revokes the nest accepted this pass.
    pub revoked: usize,
    /// Why nothing was judged this pass — `None` when the judge ran. Each
    /// reason is a ratified "never": the walk did not complete, it left rows
    /// unopened, or the ledger did not fold.
    pub skipped: Option<&'static str>,
}

/// The enumerate's outcome, carried across the fleet walk.
pub(crate) enum Enumerated {
    /// The nest's grant ids for this owner.
    Ids(Vec<[u8; 16]>),
    /// The enumerate refused or failed: no revoke this pass. The message is
    /// the step error the pass reports.
    Failed(String),
}

/// Step 1 — enumerate before the fleet walk.
pub(crate) async fn enumerate<R: RpcRequester + Clone>(rpc: &R) -> Enumerated {
    match CapabilitiesClient::new(rpc.clone()).reconcile().await {
        Ok(ids) => Enumerated::Ids(ids),
        Err(e) => Enumerated::Failed(format!("capability sweep: enumerate: {e}")),
    }
}

/// Step 3 — judge against the ledger read after the walk, and revoke.
///
/// `walk` is the fleet listing this pass completed last (the re-walk behind an
/// escrow recovery when one ran), `None` when it erred. `ledger_actor` is the
/// account's identity, `None` when it did not decode. Every failure degrades
/// to revoking less; errors land in `errors`, never in the store.
pub(crate) async fn judge_and_revoke<B, R>(
    store: &AccountStore<B>,
    rpc: &R,
    ledger_actor: Option<ActorId>,
    enumerated: Enumerated,
    walk: Option<&WalkReport>,
    errors: &mut Vec<String>,
) -> CapabilitySweep
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let nest_ids = match enumerated {
        Enumerated::Ids(ids) => ids,
        Enumerated::Failed(msg) => {
            errors.push(msg);
            return CapabilitySweep {
                skipped: Some("enumerate failed"),
                ..CapabilitySweep::default()
            };
        }
    };
    let mut sweep = CapabilitySweep {
        enumerated: nest_ids.len(),
        ..CapabilitySweep::default()
    };
    if nest_ids.is_empty() {
        return sweep;
    }
    let Some(walk) = walk else {
        sweep.skipped = Some("fleet walk did not complete");
        return sweep;
    };
    // A row left unopened for want of its generation's key may be a ledger
    // event row: the fold would be missing it and judge its live grant
    // unrecognized. Such a replica cannot fold the log, so it does not judge.
    if !walk.unkeyed.is_empty() {
        sweep.skipped = Some("fleet walk left rows unkeyed");
        return sweep;
    }
    let Some(actor) = ledger_actor else {
        sweep.skipped = Some("account id did not decode");
        return sweep;
    };
    let ledger = match crate::succession_ledger_rows::read_succession_ledger(store, actor).await {
        Ok(ledger) => ledger,
        Err(e) => {
            errors.push(format!("capability sweep: ledger read: {e:#}"));
            sweep.skipped = Some("ledger did not fold");
            return sweep;
        }
    };
    let caps = CapabilitiesClient::new(rpc.clone());
    for grant_id in grant_log::unrecognized_grant_ids(&ledger, nest_ids) {
        match caps.revoke(grant_id).await {
            Ok(_) => sweep.revoked += 1,
            Err(e) => errors.push(format!(
                "capability sweep: revoke {}: {e}",
                hex::encode(grant_id)
            )),
        }
    }
    sweep
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fauna_account_store::memory::MemoryBackend;
    use fauna_account_store::types::WriterId;

    use super::*;

    const ACCOUNT: ActorId = ActorId([0x11; 32]);
    const DEPOSITED: [u8; 16] = [0x42; 16];

    /// Records every kind sent and refuses each, so a revoke the judge fires
    /// is visible however the nest would have answered it.
    #[derive(Clone, Default)]
    struct Nest {
        sent: Arc<Mutex<Vec<&'static str>>>,
    }

    impl RpcRequester for Nest {
        type Error = String;
        async fn request<Req, Reply>(&self, kind: &'static str, _: Req) -> Result<Reply, String>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.sent.lock().unwrap().push(kind);
            Err(format!("{kind}: refused by the test nest"))
        }
    }

    async fn store() -> AccountStore<MemoryBackend> {
        AccountStore::open(
            MemoryBackend::new(),
            &fauna_core::hex32::encode(&ACCOUNT.0),
            WriterId([0x33; 32]),
        )
        .await
        .expect("open the in-memory store")
    }

    async fn judge(walk: &WalkReport) -> (CapabilitySweep, Vec<&'static str>, Vec<String>) {
        let store = store().await;
        let nest = Nest::default();
        let mut errors = Vec::new();
        let sweep = judge_and_revoke(
            &store,
            &nest,
            Some(ACCOUNT),
            Enumerated::Ids(vec![DEPOSITED]),
            Some(walk),
            &mut errors,
        )
        .await;
        let sent = nest.sent.lock().unwrap().clone();
        (sweep, sent, errors)
    }

    /// The *only where the walk is* "never" for an unkeyed replica
    /// (`ui/nests.md` § Trust facet — grants → *Reconcile*): a walk that left
    /// a row unopened for want of its generation key may have left a
    /// sibling's ledger row — and the `Mint` it carries — out of the fold, so
    /// the judge does not run. This replica's ledger holds no `Mint` at all,
    /// so a judge that ran would revoke the deposited grant.
    #[tokio::test]
    async fn a_walk_that_left_rows_unkeyed_revokes_nothing() {
        let walk = WalkReport {
            unkeyed: [[0x77; 32]].into_iter().collect(),
            ..WalkReport::default()
        };
        let (sweep, sent, errors) = judge(&walk).await;
        assert_eq!(
            sweep,
            CapabilitySweep {
                enumerated: 1,
                revoked: 0,
                skipped: Some("fleet walk left rows unkeyed"),
            }
        );
        assert!(sent.is_empty(), "no revoke was sent: {sent:?}");
        assert!(errors.is_empty(), "{errors:?}");
    }

    /// The control: the same replica after a walk that opened every row
    /// judges and revokes the grant its ledger does not hold — so the pin
    /// above is held by the unkeyed skip, not by some other refusal.
    #[tokio::test]
    async fn a_fully_keyed_walk_judges_and_revokes_the_unrecognized() {
        let (sweep, sent, errors) = judge(&WalkReport::default()).await;
        assert_eq!(sweep.skipped, None);
        assert_eq!(sent, ["fauna.capabilities.revoke"]);
        assert_eq!(
            errors.len(),
            1,
            "the refused revoke is reported: {errors:?}"
        );
    }
}
