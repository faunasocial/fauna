//! The succession-ledger seam's crossing of web's account port — both halves,
//! beside the [`SuccessionLedgerStore`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*, decisions (c), (e) and (h)).
//!
//! The grant log and its marks (`fauna.state.succession-ledger`) live in the
//! core chunk's runtime; the machines that record grant events through them —
//! the labeler catalog's grant mint and revoke, the folders page's custody
//! facet and custody revoke — live in their own chunks. So those chunks wire
//! [`PortLedgerStore`] — the **forwarder**, which encodes a method's
//! arguments, calls its door over the port and decodes the answer, and does
//! nothing else — and the core chunk's `accountPortCall` hands each door to
//! [`serve`] over the handle's own implementation of the seam. Both chunks
//! compile this module, so the two ends of every crossing are one definition.
//!
//! **The crossing set is a positive list (decision (h)).** Only the read, the
//! join and the publish to the nest's acknowledgement cross
//! (`succession_ledger.load`, `succession_ledger.merge`,
//! `succession_ledger.publish` — the last is what a chunk's grant mint
//! releases its blob behind, through the trait's provided `merge_published`). The
//! succession writes — the chain re-point and the grant-mark raise — are the
//! core chunk's post-store-ready pass's alone; on the forwarder they refuse
//! with `StoreError::Save` naming the door that does not cross. `self_actor`
//! is synchronous, so it never crosses either: the forwarder answers it from
//! the account the port was minted for (decision (e)).
//!
//! **A port fault is never a success (decision (f)).** No runtime, another
//! account's runtime, an unknown door, undecodable bytes and broken glue all
//! answer the method's own refusal, [`StoreError`] — an empty ledger is never
//! invented, and a grant event that cannot reach the runtime is not recorded.

use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_core::identity::ActorId;
use fauna_core::succession_ledger::SuccessionLedger;

use crate::store_seam::{StoreError, SuccessionLedgerStore};

/// The seam's doors — the crossing ones only (decision (h)). Prefixed with
/// the seam, since the core chunk's dispatch holds every seam's doors in one
/// namespace.
pub mod doors {
    pub const LOAD: &str = "succession_ledger.load";
    pub const MERGE: &str = "succession_ledger.merge";
    pub const PUBLISH: &str = "succession_ledger.publish";

    /// Every door of the seam that crosses.
    pub const ALL: [&str; 3] = [LOAD, MERGE, PUBLISH];
}

/// What every door answers: the ledger as it now reads, or the refusal's
/// message. The message crosses bare (not `StoreError`'s `Display`, which
/// prefixes it), so the forwarder restores the same refusal and
/// `StoreError::is_not_ready` still recognises the transient one on the far
/// side.
type Reply = Result<SuccessionLedger, String>;

/// A refusal's own message, without `StoreError`'s `Display` prefix.
fn message(e: StoreError) -> String {
    match e {
        StoreError::Load(m) | StoreError::Save(m) => m,
    }
}

/// The forwarder: [`SuccessionLedgerStore`] over an account-port transport,
/// for the account the port was minted for.
pub struct PortLedgerStore<T> {
    transport: T,
    self_actor: ActorId,
}

impl<T: PortTransport> PortLedgerStore<T> {
    /// `self_actor` is the account the port was minted for — what
    /// [`SuccessionLedgerStore::self_actor`] answers without crossing.
    pub fn new(transport: T, self_actor: ActorId) -> Self {
        Self {
            transport,
            self_actor,
        }
    }
}

fn not_crossing(door: &str) -> String {
    format!("the succession-ledger door `{door}` does not cross the account port")
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> SuccessionLedgerStore for PortLedgerStore<T> {
    fn self_actor(&self) -> Result<ActorId, StoreError> {
        Ok(self.self_actor)
    }

    async fn load(&self) -> Result<SuccessionLedger, StoreError> {
        let answered: Reply = forward(&self.transport, doors::LOAD, &())
            .await
            .map_err(|f| StoreError::Load(f.to_string()))?;
        answered.map_err(StoreError::Load)
    }

    async fn merge(&self, replica: SuccessionLedger) -> Result<SuccessionLedger, StoreError> {
        let answered: Reply = forward(&self.transport, doors::MERGE, &replica)
            .await
            .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::Save)
    }

    async fn publish_ledger(&self) -> Result<(), StoreError> {
        let answered: Result<(), String> = forward(&self.transport, doors::PUBLISH, &())
            .await
            .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::Save)
    }

    async fn repoint(&self, _retired: ActorId) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("repoint")))
    }

    async fn raise_grant_marks(&self, _predecessor: ActorId) -> Result<bool, StoreError> {
        Err(StoreError::Save(not_crossing("raise_grant_marks")))
    }
}

/// The forwarder over the SPA's `SharedAccountPort` object, minted for
/// `self_actor` — one constructor, so every consumer chunk wraps the port
/// identically. A value that is not a port is refused by name, where it was
/// wired.
#[cfg(target_arch = "wasm32")]
pub fn from_js_port(
    port: fauna_account_port::JsAccountPort,
    self_actor: ActorId,
) -> Result<std::sync::Arc<dyn SuccessionLedgerStore>, PortFault> {
    let transport = fauna_account_port::JsAccountTransport::new(port.into())?;
    Ok(std::sync::Arc::new(PortLedgerStore::new(
        transport, self_actor,
    )))
}

/// Answer one door of the succession-ledger seam from `seam` — the core
/// chunk's half. `None` when `door` is not this seam's, so the dispatch can
/// ask the next seam and refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn SuccessionLedgerStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::LOAD => {
            answer(payload, |(): ()| async move {
                let reply: Reply = seam.load().await.map_err(message);
                reply
            })
            .await
        }
        doors::MERGE => {
            answer(payload, |replica: SuccessionLedger| async move {
                let reply: Reply = seam.merge(replica).await.map_err(message);
                reply
            })
            .await
        }
        doors::PUBLISH => {
            answer(payload, |(): ()| async move {
                let reply: Result<(), String> = seam.publish_ledger().await.map_err(message);
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

    use super::*;
    use crate::test_helpers::FakeSuccessionLedgerStore;

    const ME: ActorId = ActorId([0x11; 32]);
    const PREDECESSOR: ActorId = ActorId([0x22; 32]);

    fn over(seam: Arc<dyn SuccessionLedgerStore>) -> PortLedgerStore<Loopback> {
        PortLedgerStore::new(
            Loopback::new(move |door, payload| {
                let seam = Arc::clone(&seam);
                async move {
                    serve(&*seam, door, &payload)
                        .await
                        .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
                }
            }),
            ME,
        )
    }

    /// A ledger the far side holds that differs from the empty one.
    fn moved() -> SuccessionLedger {
        let mut ledger = SuccessionLedger::empty(ME);
        ledger.prior_actor_ids = vec![PREDECESSOR];
        ledger
    }

    /// The read crosses and comes back as the seam's own ledger; the seam's
    /// not-ready refusal crosses as the same refusal, still recognisable.
    #[tokio::test]
    async fn load_round_trips_both_arms() {
        let seam = FakeSuccessionLedgerStore::serving(ME, moved());
        let port = over(Arc::new(seam.clone()));
        assert_eq!(
            port.load().await.expect("the ledger crosses"),
            seam.current()
        );

        seam.set_not_ready(true);
        let err = port.load().await.expect_err("the refusal crosses");
        assert!(
            matches!(&err, StoreError::Load(m) if m == crate::LEDGER_NOT_READY),
            "{err:?}"
        );
        assert!(
            err.is_not_ready(),
            "the not-ready refusal stays recognisable"
        );
    }

    /// The join crosses, lands in the far side's rows and answers the ledger
    /// as it now reads; the door's refusal crosses as `StoreError::Save` with
    /// its message intact, and nothing is written.
    #[tokio::test]
    async fn merge_round_trips_both_arms() {
        let seam = FakeSuccessionLedgerStore::serving(ME, SuccessionLedger::empty(ME));
        let port = over(Arc::new(seam.clone()));
        let read = port.merge(moved()).await.expect("the join crosses");
        assert_eq!(read, seam.current());
        assert_eq!(seam.merges(), 1);

        seam.refuse_next_merges(1);
        let err = port.merge(moved()).await.expect_err("the refusal crosses");
        assert!(
            matches!(&err, StoreError::Save(m) if m.contains("no generation tip")),
            "{err:?}"
        );
        assert_eq!(seam.merges(), 1, "a refused join writes nothing");
    }

    /// The publish crosses and reaches the far side's seam, its
    /// acknowledgement crossing back as `Ok`; the seam's refusal crosses as
    /// `StoreError::Save` with its message intact, so `merge_published`
    /// releases no blob behind a publish the nest never acknowledged.
    #[tokio::test]
    async fn publish_round_trips_both_arms() {
        let seam = FakeSuccessionLedgerStore::serving(ME, moved());
        let port = over(Arc::new(seam.clone()));
        port.publish_ledger().await.expect("the publish crosses");
        assert_eq!(seam.publishes(), 1, "the far side's seam published");

        seam.publish_refuses(true);
        let err = port
            .publish_ledger()
            .await
            .expect_err("the refusal crosses");
        assert!(
            matches!(&err, StoreError::Save(m) if m.contains("did not acknowledge the ledger rows")),
            "{err:?}"
        );
        assert_eq!(seam.publishes(), 1, "a refused publish is not counted");
    }

    /// `self_actor` never crosses: it answers the account the port was minted
    /// for, even over a transport that reaches nothing.
    #[test]
    fn self_actor_is_the_minted_account() {
        let port = PortLedgerStore::new(Loopback::faulting(PortFault::Glue("x".into())), ME);
        assert_eq!(port.self_actor().unwrap(), ME);
    }

    /// A faulting transport answers every method with a refusal — never an
    /// empty ledger, never a recorded event.
    #[tokio::test]
    async fn a_faulting_transport_is_refused_on_every_method() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortLedgerStore::new(Loopback::faulting(fault.clone()), ME);
            assert!(
                matches!(port.load().await, Err(StoreError::Load(m)) if m == fault.to_string()),
                "{fault:?}"
            );
            assert!(
                matches!(port.merge(moved()).await, Err(StoreError::Save(m)) if m == fault.to_string()),
                "{fault:?}"
            );
            assert!(
                matches!(port.publish_ledger().await, Err(StoreError::Save(m)) if m == fault.to_string()),
                "{fault:?}"
            );
            assert!(matches!(
                port.repoint(PREDECESSOR).await,
                Err(StoreError::Save(_))
            ));
            assert!(matches!(
                port.raise_grant_marks(PREDECESSOR).await,
                Err(StoreError::Save(_))
            ));
        }
    }

    /// The succession writes do not cross (decision (h)): each refuses on the
    /// forwarder without calling the port, and `serve` answers neither.
    #[tokio::test]
    async fn the_succession_writes_do_not_cross() {
        let seam = FakeSuccessionLedgerStore::serving(ME, moved());
        let port = over(Arc::new(seam.clone()));
        assert!(matches!(
            port.repoint(PREDECESSOR).await,
            Err(StoreError::Save(_))
        ));
        assert!(matches!(
            port.raise_grant_marks(PREDECESSOR).await,
            Err(StoreError::Save(_))
        ));
        assert_eq!(seam.merges(), 0, "nothing was written through the port");
        for door in [
            "succession_ledger.repoint",
            "succession_ledger.raise_grant_marks",
            "succession_ledger.self_actor",
        ] {
            assert!(serve(&seam, door, &[]).await.is_none(), "{door}");
        }
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name; every door of the seam is.
    #[tokio::test]
    async fn a_foreign_door_is_not_this_seams() {
        let seam = FakeSuccessionLedgerStore::serving(ME, moved());
        assert!(serve(&seam, "mail.load", &[]).await.is_none());
        let unit = fauna_account_port::encode(&()).unwrap();
        let replica = fauna_account_port::encode(&moved()).unwrap();
        assert!(serve(&seam, doors::LOAD, &unit).await.is_some());
        assert!(serve(&seam, doors::MERGE, &replica).await.is_some());
        assert!(serve(&seam, doors::PUBLISH, &unit).await.is_some());
    }
}
