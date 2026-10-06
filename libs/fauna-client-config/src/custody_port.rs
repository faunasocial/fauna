//! The custody-ceremony seam's crossing of web's account port — both halves,
//! beside the [`CustodyCeremonyStore`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*, decisions (c) and (h): the custody-ceremony
//! seam's doors are `custody` and `merge_custody`).
//!
//! The custody facet lives in the folders chunk; the account runtime whose
//! `fauna.state.custody-ceremony` rows it folds lives in the core chunk. So
//! the folders chunk reads through [`PortCustodyStore`] — the **forwarder**,
//! which encodes a method's arguments, calls its door over the port and
//! decodes the answer, and does nothing else — and the core chunk's
//! `accountPortCall` hands each door to [`serve`] over the handle's own
//! implementation of the seam. Both chunks compile this module, so the two
//! ends of every crossing are one definition.
//!
//! **A port fault is never a success (decision (f)).** No runtime, another
//! account's runtime, an unknown door, undecodable bytes and broken glue all
//! answer the method's own refusal, [`StoreError`] — so an unreadable facet
//! is the same transient on web as a store not yet up is natively.

use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_core::custody_ceremony::CustodyConfig;

use crate::store_seam::{CustodyCeremonyStore, StoreError};

/// The seam's doors, one per [`CustodyCeremonyStore`] method, prefixed with
/// the seam (the core chunk's dispatch holds every seam's doors in one
/// namespace).
pub mod doors {
    pub const CUSTODY: &str = "custody_ceremony.custody";
    pub const MERGE_CUSTODY: &str = "custody_ceremony.merge_custody";

    /// Every door of the seam.
    pub const ALL: [&str; 2] = [CUSTODY, MERGE_CUSTODY];
}

/// What every door answers: the state as it now reads, or the refusal's
/// text (the forwarder restores the method's own [`StoreError`] arm).
type Reply = Result<CustodyConfig, String>;

/// The forwarder: [`CustodyCeremonyStore`] over an account-port transport.
pub struct PortCustodyStore<T> {
    transport: T,
}

impl<T: PortTransport> PortCustodyStore<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> CustodyCeremonyStore for PortCustodyStore<T> {
    async fn custody(&self) -> Result<CustodyConfig, StoreError> {
        let answered: Reply = forward(&self.transport, doors::CUSTODY, &())
            .await
            .map_err(|f| StoreError::Load(f.to_string()))?;
        answered.map_err(StoreError::Load)
    }

    async fn merge_custody(&self, replica: CustodyConfig) -> Result<CustodyConfig, StoreError> {
        let answered: Reply = forward(&self.transport, doors::MERGE_CUSTODY, &replica)
            .await
            .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::Save)
    }
}

/// Answer one door of the custody-ceremony seam from `seam` — the core
/// chunk's half. `None` when `door` is not this seam's, so the dispatch can
/// ask the next seam and refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn CustodyCeremonyStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::CUSTODY => {
            answer(payload, |(): ()| async move {
                let reply: Reply = seam.custody().await.map_err(|e| e.to_string());
                reply
            })
            .await
        }
        doors::MERGE_CUSTODY => {
            answer(payload, |replica: CustodyConfig| async move {
                let reply: Reply = seam.merge_custody(replica).await.map_err(|e| e.to_string());
                reply
            })
            .await
        }
        _ => return None,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::Arc;

    use fauna_account_port::loopback::Loopback;
    use fauna_core::custody_ceremony::{GrantedCustody, HeldCustody};
    use fauna_core::data::Timestamp;

    use super::*;
    use crate::test_helpers::FakeCustodyCeremonyStore;

    fn over(seam: Arc<FakeCustodyCeremonyStore>) -> PortCustodyStore<Loopback> {
        PortCustodyStore::new(Loopback::new(move |door, payload| {
            let seam = Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    fn one_of_each() -> CustodyConfig {
        CustodyConfig {
            granted: vec![GrantedCustody {
                grant_id: vec![1; 16],
                host: [2; 32],
                offer: vec![0xa1, 0x01],
                offer_posted: true,
                updated_at: Timestamp(5),
                ..Default::default()
            }],
            held: vec![HeldCustody {
                grant_id: vec![3; 16],
                owner: [4; 32],
                deliver: vec![0xa2],
                deliver_at: Timestamp(9),
                updated_at: Timestamp(6),
                ..Default::default()
            }],
        }
    }

    /// Both methods cross and come back with the seam's own answer, the
    /// replica intact through the join — and both refusal arms come back as
    /// the method's own `StoreError`.
    #[tokio::test]
    async fn every_method_round_trips_both_arms() {
        let seam = Arc::new(FakeCustodyCeremonyStore::empty());
        let port = over(Arc::clone(&seam));
        assert_eq!(port.custody().await.unwrap(), CustodyConfig::default());
        let merged = port.merge_custody(one_of_each()).await.unwrap();
        assert_eq!(merged, one_of_each());
        assert_eq!(seam.current(), one_of_each(), "the write reached the seam");
        assert_eq!(port.custody().await.unwrap(), one_of_each());

        seam.refuse_next_merges(1);
        assert!(matches!(
            port.merge_custody(one_of_each()).await,
            Err(StoreError::Save(why)) if why.contains("no generation tip")
        ));
    }

    /// A faulting transport answers both methods with their refusal — never
    /// a success, so an unreachable runtime is a transient, never "no
    /// custodians".
    #[tokio::test]
    async fn a_faulting_transport_is_refused_on_every_method() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortCustodyStore::new(Loopback::faulting(fault.clone()));
            let said = fault.to_string();
            assert!(matches!(port.custody().await, Err(StoreError::Load(w)) if w == said));
            assert!(matches!(
                port.merge_custody(one_of_each()).await,
                Err(StoreError::Save(w)) if w == said
            ));
        }
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name.
    #[tokio::test]
    async fn a_foreign_door_is_not_this_seams() {
        let seam = FakeCustodyCeremonyStore::empty();
        assert!(
            serve(&seam, "fleet_removal.fleet_members", &[])
                .await
                .is_none()
        );
        for door in doors::ALL {
            assert!(serve(&seam, door, &[0xff]).await.is_some(), "{door}");
        }
    }
}
