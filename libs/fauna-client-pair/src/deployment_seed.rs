//! The deployment-seed drives over the identity the connection is **bound**
//! to — the one composition every host's custody leg and rotation runs: the
//! native seam `fauna_client_account_runtime::deployment_seeds` (tui, linux,
//! and through it fauna-ffi for windows, apple and android) and fauna-wasm
//! (web) (`nest/box-recovery.md` § Deployment-seed rotation → *Custody after
//! rotation*; § The plane-era recovery floor, *(c) The writes*).
//!
//! The predecessor a rotation marks, and the box a custody leg captures, is
//! the identity [`LinkedNestsMachine::bound_nest_id`] proves — the login's
//! pin, else a possession proof over the connection, refused when the box's
//! own `fauna.nest.info` claim disagrees — never that claim on its own, so a
//! rotation can only ever mark the box it is talking to. The id is resolved
//! here and nowhere else: a host hands over its identity machine and its
//! transport, never an id, so no host can key a write on the claim. The
//! tests below drive both drives against a box whose claim names a sibling.
//!
//! What a host does with the outcome — the warning it renders, the verdict
//! it paints, its own log lines — stays the host's.

use fauna_client_config::{
    CustodyLegReport, DeploymentSeedStore, SeedRotation, rotate_deployment_seed_on_plane,
    run_deployment_seed_custody_leg,
};
use fauna_core::identity::ActorId;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::LinkedNestsMachine;

/// The identity `identity`'s connection is bound to, as an [`ActorId`].
///
/// # Errors
///
/// The bound id could not be resolved (no reachable connection, a box whose
/// own claim disagrees with what the connection proved) or is not 32 bytes.
pub async fn bound_actor_id(identity: &LinkedNestsMachine) -> Result<ActorId, String> {
    let bytes = identity.bound_nest_id().await.map_err(|e| e.to_string())?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map(ActorId)
        .map_err(|_| "the nest reported an identity that is not 32 bytes".to_string())
}

/// **The custody leg** (`fauna_client_config::run_deployment_seed_custody_leg`)
/// over `nest`, keyed on the id `identity` is bound to.
///
/// # Errors
///
/// The bound id did not resolve ([`bound_actor_id`]) — the leg was not
/// attempted: no custody round trip, nothing captured, nothing marked. The
/// host owes `fauna_client_config::custody_leg_unattempted_warning`.
pub async fn run_custody_leg_bound<R, S>(
    nest: &R,
    identity: &LinkedNestsMachine,
    store: &S,
) -> Result<CustodyLegReport, String>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    S: DeploymentSeedStore + ?Sized,
{
    let bound = bound_actor_id(identity).await?;
    Ok(run_deployment_seed_custody_leg(nest, bound, store).await)
}

/// **The plane rotation drive** (`fauna_client_config::
/// rotate_deployment_seed_on_plane`) over `nest`, its predecessor the id
/// `identity` is bound to — resolved first, while the box still answers as
/// the predecessor. `store` is the host's per-call resolver over its account
/// store (`fauna_client_config::ResolvingLedgerStore`).
///
/// Answers the [`SeedRotation`] itself, so a host can paint both the verdict
/// (`fauna_client_config::seed_rotation_verdict`) and whether the box's
/// identity flipped.
///
/// # Errors
///
/// The bound id did not resolve (nothing minted, merged or dispatched), or
/// the drive refused (the store never came, the custody merge refused, the
/// successor row not published, the dispatch failed) — nothing rotated. The
/// text is the cause the host's `rotate_seed_failed` verdict carries.
pub async fn rotate_deployment_seed_bound<R, S>(
    nest: &R,
    identity: &LinkedNestsMachine,
    store: &S,
) -> Result<SeedRotation, String>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
    S: DeploymentSeedStore + ?Sized,
{
    let bound = bound_actor_id(identity).await?;
    rotate_deployment_seed_on_plane(nest, bound, store)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    //! The drives' predecessor is the box the connection is bound to, never
    //! the box's own `fauna.nest.info` claim (`nest/box-recovery.md`
    //! § Deployment-seed rotation → *Custody after rotation*): a box claiming
    //! a sibling's identity is refused before anything is minted, merged,
    //! dispatched or marked — so a rotation can only ever mark the box it is
    //! talking to.
    use super::*;
    use std::sync::{Arc, Mutex};

    use fauna_client_config::test_helpers::FakeDeploymentSeedStore;
    use fauna_client_core::succession_delivery::OwedReason;
    use fauna_core::data::DeploymentSeedEntry;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::RpcErrorClass;

    use crate::{HolderInfo, LinkedNestsNest, PairNestError, SelfNest};
    use fauna_protocol::pair::{PairAddRequest, PairListReply};
    use fauna_protocol::{ByteBuf, RpcError};

    const ROTATE: &str = "fauna.admin.deployment_seed.rotate";

    fn keypair(byte: u8) -> ActorKeypair {
        ActorKeypair::from_secret([byte; 32])
    }

    fn entry(byte: u8, domain: &str) -> DeploymentSeedEntry {
        DeploymentSeedEntry {
            nest_actor_id: keypair(byte).actor_id().0,
            seed: [byte; 32].into(),
            domain: Some(domain.into()),
            ..Default::default()
        }
    }

    /// The identity half of the box: the connection proved `bound`, while the
    /// box's `fauna.nest.info` claim answers `claimed`. Only the two identity
    /// reads are reachable — the drives touch nothing else of the seam.
    struct BoxIdentity {
        bound: [u8; 32],
        claimed: [u8; 32],
    }

    #[async_trait::async_trait]
    impl LinkedNestsNest for BoxIdentity {
        async fn list(&self) -> Result<PairListReply, PairNestError> {
            unreachable!()
        }
        async fn add(&self, _: PairAddRequest) -> Result<(), PairNestError> {
            unreachable!()
        }
        async fn revoke(&self, _: Vec<u8>) -> Result<(), PairNestError> {
            unreachable!()
        }
        async fn forward_retry(&self) -> Result<u64, PairNestError> {
            unreachable!()
        }
        async fn forward_discard(&self) -> Result<u64, PairNestError> {
            unreachable!()
        }
        async fn this_nest(&self) -> Result<SelfNest, PairNestError> {
            Ok(SelfNest {
                id: self.claimed.to_vec(),
                url: "https://box.test".into(),
            })
        }
        async fn bound_nest_id(&self) -> Result<Vec<u8>, PairNestError> {
            Ok(self.bound.to_vec())
        }
        async fn connect_peer(&self, _: &str) -> Result<Arc<dyn LinkedNestsNest>, PairNestError> {
            unreachable!()
        }
        async fn escrow_generations(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            unreachable!()
        }
        async fn escrow_delete(&self, _: Vec<u8>) -> Result<(), PairNestError> {
            unreachable!()
        }
        async fn registration_chain(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            unreachable!()
        }
        async fn submit_registration(&self, _: Vec<u8>) -> Result<(), PairNestError> {
            unreachable!()
        }
        async fn predecessor_statements(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            unreachable!()
        }
        async fn submit_succession_at(
            &self,
            _: &str,
            _: Vec<Vec<u8>>,
        ) -> Result<usize, OwedReason> {
            unreachable!()
        }
        async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, PairNestError> {
            unreachable!()
        }
        async fn mint_grant(&self, _: Vec<u8>) -> Result<(), PairNestError> {
            unreachable!()
        }
        async fn renew_grant(
            &self,
            _: [u8; 16],
            _: u64,
            _: u64,
            _: Vec<Vec<u8>>,
        ) -> Result<(), PairNestError> {
            unreachable!()
        }
        async fn revoke_grant(&self, _: [u8; 16]) -> Result<(), PairNestError> {
            unreachable!()
        }
    }

    #[derive(Debug)]
    struct Refused(RpcError);
    impl core::fmt::Display for Refused {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "rpc {}", self.0.code)
        }
    }
    impl RpcErrorClass for Refused {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    /// The transport half: records every kind dispatched, answers
    /// `fauna.nest.info` with the same claim (so a drive reading the claim over
    /// the wire is caught too), commits a rotate by adopting the seed it was
    /// sent, and refuses everything else.
    struct BoxWire {
        claimed: [u8; 32],
        calls: Mutex<Vec<&'static str>>,
    }

    impl BoxWire {
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl RpcRequester for BoxWire {
        type Error = Refused;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            self.calls.lock().unwrap().push(kind);
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                "fauna.nest.info" => {
                    fauna_protocol::encode_canonical(&fauna_protocol::discovery::NestInfoReply {
                        nest_id: fauna_core::hex32::encode(&self.claimed),
                        domain: "box.test".into(),
                        ..Default::default()
                    })
                }
                ROTATE => {
                    let req: fauna_protocol::admin::AdminDeploymentSeedRotateRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode rotate request");
                    let sent = ActorKeypair::from_secret(
                        fauna_core::hex32::decode(req.new_seed.as_str()).expect("hex seed"),
                    )
                    .actor_id();
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::admin::AdminDeploymentSeedRotateReply {
                            nest_actor_id: ByteBuf::from(sent.0.to_vec()),
                            seq: 1,
                            already_rotated: false,
                            extra: Default::default(),
                        },
                    )
                }
                _ => {
                    return Err(Refused(RpcError::new(
                        "fauna.protocol.unknown_kind",
                        "error.protocol.unknown_kind",
                    )));
                }
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// The box bound as byte 1's identity, claiming byte `claimed`'s, over a
    /// store that custodies both it and its sibling (byte 2).
    fn rig(claimed: u8) -> (BoxWire, LinkedNestsMachine, FakeDeploymentSeedStore) {
        let claimed = keypair(claimed).actor_id().0;
        let identity = LinkedNestsMachine::new(Arc::new(BoxIdentity {
            bound: keypair(1).actor_id().0,
            claimed,
        }));
        let wire = BoxWire {
            claimed,
            calls: Mutex::new(Vec::new()),
        };
        let store =
            FakeDeploymentSeedStore::with(vec![entry(1, "this.test"), entry(2, "sibling.test")]);
        (wire, identity, store)
    }

    fn superseded(store: &FakeDeploymentSeedStore, byte: u8) -> Option<[u8; 32]> {
        let id = keypair(byte).actor_id().0;
        store
            .current()
            .into_iter()
            .find(|e| e.nest_actor_id == id)
            .expect("custodied")
            .superseded_by
    }

    /// The control: a box whose claim agrees with the connection rotates, the
    /// outcome says so, and the mark lands on it — so the refusal below is the
    /// identity check, not a rig that cannot rotate at all.
    #[tokio::test]
    async fn a_truthful_box_rotates_and_marks_itself() {
        let (wire, identity, store) = rig(1);
        let outcome = rotate_deployment_seed_bound(&wire, &identity, &store).await;
        assert!(
            matches!(outcome, Ok(SeedRotation::Rotated { .. })),
            "a truthful box rotates: {outcome:?}"
        );
        assert!(wire.calls().contains(&ROTATE));
        assert_eq!(store.current().len(), 3, "the successor is custodied");
        assert!(superseded(&store, 1).is_some(), "the bound box is marked");
        assert_eq!(superseded(&store, 2), None, "the sibling is untouched");
    }

    /// A box bound as one identity claiming its sibling's: the rotation is
    /// refused before the mint — no successor merged, no rotate dispatched,
    /// and the sibling (whose custody the store holds) never marked.
    #[tokio::test]
    async fn a_box_claiming_a_sibling_is_refused_and_marks_nothing() {
        let (wire, identity, store) = rig(2);
        let before = store.current();
        let outcome = rotate_deployment_seed_bound(&wire, &identity, &store).await;
        assert!(
            outcome.is_err(),
            "a lying box's rotation is a refusal: {outcome:?}"
        );
        assert!(
            !wire.calls().contains(&ROTATE),
            "nothing dispatched: {:?}",
            wire.calls()
        );
        assert_eq!(store.merges(), 0, "nothing minted or merged");
        assert_eq!(store.current(), before);
        assert_eq!(superseded(&store, 2), None, "the sibling is never marked");
    }

    /// The custody leg's control: over a truthful box the leg is attempted —
    /// it goes to the wire — so the silence below is the identity check.
    #[tokio::test]
    async fn the_custody_leg_over_a_truthful_box_is_attempted() {
        let (wire, identity, store) = rig(1);
        let report = run_custody_leg_bound(&wire, &identity, &store).await;
        assert!(report.is_ok(), "attempted: {report:?}");
        assert!(!wire.calls().is_empty(), "the leg went to the wire");
    }

    /// The custody leg over the same lying box: unattempted, with no custody
    /// round trip at all — nothing captured, nothing marked.
    #[tokio::test]
    async fn the_custody_leg_over_a_box_claiming_a_sibling_captures_and_marks_nothing() {
        let (wire, identity, store) = rig(2);
        let before = store.current();
        let report = run_custody_leg_bound(&wire, &identity, &store).await;
        assert!(report.is_err(), "unattempted: {report:?}");
        assert!(
            wire.calls().is_empty(),
            "no custody round trip: {:?}",
            wire.calls()
        );
        assert_eq!(store.merges(), 0);
        assert_eq!(store.current(), before);
    }
}
