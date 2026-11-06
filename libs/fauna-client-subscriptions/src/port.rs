//! The period-key custody seam's crossing of web's account port — both halves,
//! beside the [`PeriodKeyStore`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*; the `fauna_client_folders::port` shape).
//!
//! The account's tier period-key custody (`fauna.state.subscriptions`) lives in
//! the core chunk's runtime; a page machine in another chunk that seals under a
//! tier — the archive import's friends-only and only-me posts, through
//! `SubscriptionsAuthor` — reads and writes it. So that chunk wires
//! [`PortPeriodKeys`] — the **forwarder**, which encodes a method's arguments,
//! calls its door over the port and decodes the answer, and does nothing else —
//! and the core chunk's `accountPortCall` hands each door to [`serve`] over
//! `fauna_account_seams::period_keys::PlanePeriodKeys`, the one adapter the six
//! native apps wire. Both chunks compile this module, so the two ends of every
//! crossing are one definition.
//!
//! What crosses is key material. The hop is between two wasm modules of one
//! tab, as canonical bytes, never off the device.
//!
//! **A port fault is never a success.** No runtime, another account's runtime,
//! an unknown door, undecodable bytes and broken glue all answer the method's
//! own `Err` — a read is `StoreError::Load`, a write `StoreError::Save` — so a
//! read that cannot reach the runtime is never an empty custody (the trait's
//! law: reading "no key" where one exists mints a fresh key over a live blob).
//! A seam refusal crosses as its bare message and comes back as the same
//! `StoreError`, so `StoreError::is_not_ready` still recognises the transient
//! one on the far side. A runtime still assembling is the far seam's to wait
//! out, for a write and for the read a write starts from
//! ([`doors::CUSTODY_FOR_WRITE`]) — the forwarder never waits.

use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_client_config::StoreError;
use fauna_core::data::{PendingRemoval, SubscriptionsConfig};

use crate::period_keys::PeriodKeyStore;

/// The seam's doors, one per [`PeriodKeyStore`] method. Prefixed with the
/// seam, since the core chunk's dispatch holds every seam's doors in one
/// namespace.
pub mod doors {
    pub const CUSTODY: &str = "period_keys.custody";
    pub const CUSTODY_FOR_WRITE: &str = "period_keys.custody_for_write";
    pub const MERGE_CUSTODY: &str = "period_keys.merge_custody";
    pub const SETTLE_REMOVAL: &str = "period_keys.settle_removal";

    /// Every door of the seam.
    pub const ALL: [&str; 4] = [CUSTODY, CUSTODY_FOR_WRITE, MERGE_CUSTODY, SETTLE_REMOVAL];
}

/// What a door answers: the seam method's own result, its failure as the
/// refusal's bare message (not `StoreError`'s prefixed `Display`).
type Reply = Result<SubscriptionsConfig, String>;

fn message(e: StoreError) -> String {
    match e {
        StoreError::Load(m) | StoreError::Save(m) => m,
    }
}

/// The forwarder: [`PeriodKeyStore`] over an account-port transport.
pub struct PortPeriodKeys<T> {
    transport: T,
}

impl<T: PortTransport> PortPeriodKeys<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

/// A crossing's answer as the seam's own `Result`, a port fault and the seam's
/// refusal both mapped by `as_error` (`Load` for a read, `Save` for a write).
fn answered(
    crossed: Result<Reply, PortFault>,
    as_error: fn(String) -> StoreError,
) -> Result<SubscriptionsConfig, StoreError> {
    crossed
        .map_err(|f| as_error(f.to_string()))?
        .map_err(as_error)
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> PeriodKeyStore for PortPeriodKeys<T> {
    async fn custody(&self) -> Result<SubscriptionsConfig, StoreError> {
        answered(
            forward(&self.transport, doors::CUSTODY, &()).await,
            StoreError::Load,
        )
    }

    // Its own door, never `CUSTODY`: the wait for a runtime still assembling is
    // the core chunk's seam's, and it only runs when the crossing says which
    // read this is.
    async fn custody_for_write(&self) -> Result<SubscriptionsConfig, StoreError> {
        answered(
            forward(&self.transport, doors::CUSTODY_FOR_WRITE, &()).await,
            StoreError::Load,
        )
    }

    async fn merge_custody(
        &self,
        replica: SubscriptionsConfig,
    ) -> Result<SubscriptionsConfig, StoreError> {
        answered(
            forward(&self.transport, doors::MERGE_CUSTODY, &replica).await,
            StoreError::Save,
        )
    }

    async fn settle_removal(
        &self,
        removal: PendingRemoval,
    ) -> Result<SubscriptionsConfig, StoreError> {
        answered(
            forward(&self.transport, doors::SETTLE_REMOVAL, &removal).await,
            StoreError::Save,
        )
    }
}

/// Answer one door of the seam from `seam` — the core chunk's half. `None`
/// when `door` is not this seam's, so the dispatch can ask the next seam and
/// refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn PeriodKeyStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::CUSTODY => {
            answer(payload, |(): ()| async move {
                let reply: Reply = seam.custody().await.map_err(message);
                reply
            })
            .await
        }
        doors::CUSTODY_FOR_WRITE => {
            answer(payload, |(): ()| async move {
                let reply: Reply = seam.custody_for_write().await.map_err(message);
                reply
            })
            .await
        }
        doors::MERGE_CUSTODY => {
            answer(payload, |replica: SubscriptionsConfig| async move {
                let reply: Reply = seam.merge_custody(replica).await.map_err(message);
                reply
            })
            .await
        }
        doors::SETTLE_REMOVAL => {
            answer(payload, |removal: PendingRemoval| async move {
                let reply: Reply = seam.settle_removal(removal).await.map_err(message);
                reply
            })
            .await
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fauna_account_port::loopback::Loopback;
    use fauna_client_config::LEDGER_NOT_READY;
    use fauna_client_testkit::block_on;
    use fauna_core::data::{TierPeriod, TierPeriodKeys};
    use fauna_core::identity::ActorId;

    use super::*;
    use crate::period_keys::MemoryPeriodKeyStore;

    fn over<S: PeriodKeyStore + 'static>(seam: Arc<S>) -> PortPeriodKeys<Loopback> {
        PortPeriodKeys::new(Loopback::new(move |door, payload| {
            let seam = Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    fn period(version: u64, key: u8) -> TierPeriod {
        TierPeriod {
            version,
            key: [key; 32].into(),
            rotated_at: 1_000 * version,
            minted_by: None,
        }
    }

    /// A write then a read round-trips the custody — the period key itself
    /// byte-for-byte — and a merge is the seam's join, a settle the seam's
    /// settle, each answered with the account's fold as it now stands.
    #[test]
    fn every_method_round_trips_the_custody() {
        let seam = Arc::new(MemoryPeriodKeyStore::new());
        let port = over(Arc::clone(&seam));
        assert_eq!(
            block_on(port.custody()).unwrap(),
            SubscriptionsConfig::default()
        );

        let replica = SubscriptionsConfig {
            tiers: vec![TierPeriodKeys {
                tier_name: "friends".into(),
                current: period(1, 0x42),
                prior: vec![],
            }],
            pending_removals: vec![],
        };
        let merged = block_on(port.merge_custody(replica)).unwrap();
        assert_eq!(merged, seam.folded());
        assert_eq!(merged.tiers[0].current.key, period(1, 0x42).key);
        assert_eq!(block_on(port.custody()).unwrap(), seam.folded());
        assert_eq!(block_on(port.custody_for_write()).unwrap(), seam.folded());

        let removal = PendingRemoval {
            tier_name: "friends".into(),
            subscriber_id: ActorId([3; 32]),
            new_period: period(2, 0x43),
        };
        let settled = block_on(port.settle_removal(removal)).unwrap();
        assert_eq!(settled, seam.folded());
        assert_eq!(settled.tiers[0].current.version, 2);
    }

    /// A faulting transport answers every method with `Err` — never a
    /// success, never an empty custody a mint would read as "no key held".
    #[test]
    fn a_faulting_transport_is_refused_on_every_method() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortPeriodKeys::new(Loopback::faulting(fault));
            assert!(matches!(block_on(port.custody()), Err(StoreError::Load(_))));
            assert!(matches!(
                block_on(port.custody_for_write()),
                Err(StoreError::Load(_))
            ));
            assert!(matches!(
                block_on(port.merge_custody(SubscriptionsConfig::default())),
                Err(StoreError::Save(_))
            ));
        }
    }

    /// A seam whose plain read refuses "not ready" while its write-intent read
    /// answers — `PlanePeriodKeys` over a runtime still assembling.
    struct AssemblingRuntime(MemoryPeriodKeyStore);

    #[async_trait::async_trait]
    impl PeriodKeyStore for AssemblingRuntime {
        async fn custody(&self) -> Result<SubscriptionsConfig, StoreError> {
            Err(StoreError::Load(LEDGER_NOT_READY.into()))
        }
        async fn custody_for_write(&self) -> Result<SubscriptionsConfig, StoreError> {
            self.0.custody().await
        }
        async fn merge_custody(
            &self,
            replica: SubscriptionsConfig,
        ) -> Result<SubscriptionsConfig, StoreError> {
            self.0.merge_custody(replica).await
        }
        async fn settle_removal(
            &self,
            removal: PendingRemoval,
        ) -> Result<SubscriptionsConfig, StoreError> {
            self.0.settle_removal(removal).await
        }
    }

    /// The read a write starts from crosses by its own door, so the far seam's
    /// wait runs for it alone; and the not-ready refusal of a plain read comes
    /// back as itself, so a reader on the far side still branches on it.
    #[test]
    fn a_write_intent_read_crosses_as_one_and_not_ready_survives() {
        let port = over(Arc::new(AssemblingRuntime(MemoryPeriodKeyStore::new())));
        let refused = block_on(port.custody()).unwrap_err();
        assert!(refused.is_not_ready(), "{refused:?}");
        assert_eq!(
            block_on(port.custody_for_write()).unwrap(),
            SubscriptionsConfig::default()
        );
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name.
    #[test]
    fn a_foreign_door_is_not_this_seams() {
        let seam = MemoryPeriodKeyStore::new();
        assert!(block_on(serve(&seam, "folder_keys.load", &[])).is_none());
        for door in doors::ALL {
            assert!(block_on(serve(&seam, door, &[0xff])).is_some(), "{door}");
        }
    }
}
