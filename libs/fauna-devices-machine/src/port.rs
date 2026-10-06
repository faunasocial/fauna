//! The fleet seam's crossing of web's account port — both halves, beside the
//! [`FleetRemoval`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*, decision (c)).
//!
//! The Devices page's machine lives in the folders chunk; the account runtime
//! it removes through lives in the core chunk. So the folders chunk wires
//! [`PortFleetRemoval`] — the **forwarder**, which encodes a method's
//! arguments, calls its door over the port and decodes the answer, and does
//! nothing else — and the core chunk's `accountPortCall` hands each door to
//! [`serve`] over `RuntimeFleetRemoval`, the one adapter the six native apps
//! wire. Both chunks compile this module, so the two ends of every crossing
//! are one definition, and the machine's removal order is the native one:
//! the same machine, the same trait, the same adapter.
//!
//! **A port fault is never a success (decision (f)).** No runtime, another
//! account's runtime, an unknown door, undecodable bytes and broken glue all
//! answer the method's own refusal — [`FleetRemovalRefusal::Unavailable`] on
//! `resolve_removal` and `remove_member`, `Err` on the other three — so a
//! removal that cannot reach the runtime deletes nothing, exactly as an
//! absent runtime refuses natively.

use fauna_account_port::{PortFault, PortTransport, answer, forward};
use serde::{Deserialize, Serialize};

use crate::machine::{FleetMembersView, FleetRemoval, FleetRemovalRefusal, NestDeletion};

/// The fleet seam's doors, one per [`FleetRemoval`] method. Prefixed with the
/// seam, since the core chunk's dispatch holds every seam's doors in one
/// namespace.
pub mod doors {
    pub const RESOLVE_REMOVAL: &str = "fleet_removal.resolve_removal";
    pub const STAGE_REMOVAL: &str = "fleet_removal.stage_removal";
    pub const SETTLE_REMOVAL: &str = "fleet_removal.settle_removal";
    pub const FLEET_MEMBERS: &str = "fleet_removal.fleet_members";
    pub const REMOVE_MEMBER: &str = "fleet_removal.remove_member";

    /// Every door of the seam.
    pub const ALL: [&str; 5] = [
        RESOLVE_REMOVAL,
        STAGE_REMOVAL,
        SETTLE_REMOVAL,
        FLEET_MEMBERS,
        REMOVE_MEMBER,
    ];
}

// What each door carries. Named fields rather than tuples, because a
// fixed-width id crosses as a byte string (`serialization.md` § Canonical IPLD
// dag-cbor, "Fixed-size byte arrays"), which only a field attribute can say.

/// One roster row: its id and the principal the nest put on it.
#[derive(Serialize, Deserialize)]
struct RowClaim {
    row: String,
    #[serde(with = "serde_bytes")]
    claimed: Option<[u8; 32]>,
}

/// A removal's row and the fleet ids it resolved to.
#[derive(Serialize, Deserialize)]
struct RowTargets {
    row: String,
    #[serde(with = "fauna_core::byte_array::vec")]
    targets: Vec<[u8; 32]>,
}

#[derive(Serialize, Deserialize)]
struct Settle {
    row: String,
    #[serde(with = "fauna_core::byte_array::vec")]
    targets: Vec<[u8; 32]>,
    outcome: NestDeletion,
}

#[derive(Serialize, Deserialize)]
struct Roster {
    rows: Vec<RowClaim>,
}

#[derive(Serialize, Deserialize)]
struct Member {
    #[serde(with = "serde_bytes")]
    member: [u8; 32],
}

/// `resolve_removal`'s success arm.
#[derive(Serialize, Deserialize)]
struct Targets(#[serde(with = "fauna_core::byte_array::vec")] Vec<[u8; 32]>);

/// The forwarder: [`FleetRemoval`] over an account-port transport.
pub struct PortFleetRemoval<T> {
    transport: T,
}

impl<T: PortTransport> PortFleetRemoval<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

fn unavailable(fault: PortFault) -> FleetRemovalRefusal {
    FleetRemovalRefusal::Unavailable(fault.to_string())
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> FleetRemoval for PortFleetRemoval<T> {
    async fn resolve_removal(
        &self,
        row_device_id: &str,
        claimed_principal: Option<[u8; 32]>,
    ) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal> {
        let args = RowClaim {
            row: row_device_id.to_string(),
            claimed: claimed_principal,
        };
        let answered: Result<Targets, FleetRemovalRefusal> =
            forward(&self.transport, doors::RESOLVE_REMOVAL, &args)
                .await
                .map_err(unavailable)?;
        answered.map(|t| t.0)
    }

    async fn stage_removal(
        &self,
        row_device_id: &str,
        targets: Vec<[u8; 32]>,
    ) -> Result<(), String> {
        let args = RowTargets {
            row: row_device_id.to_string(),
            targets,
        };
        forward(&self.transport, doors::STAGE_REMOVAL, &args)
            .await
            .map_err(|f| f.to_string())?
    }

    async fn settle_removal(
        &self,
        row_device_id: &str,
        targets: Vec<[u8; 32]>,
        outcome: NestDeletion,
    ) -> Result<(), String> {
        let args = Settle {
            row: row_device_id.to_string(),
            targets,
            outcome,
        };
        forward(&self.transport, doors::SETTLE_REMOVAL, &args)
            .await
            .map_err(|f| f.to_string())?
    }

    async fn fleet_members(
        &self,
        roster: Vec<(String, Option<[u8; 32]>)>,
    ) -> Result<FleetMembersView, String> {
        let args = Roster {
            rows: roster
                .into_iter()
                .map(|(row, claimed)| RowClaim { row, claimed })
                .collect(),
        };
        forward(&self.transport, doors::FLEET_MEMBERS, &args)
            .await
            .map_err(|f| f.to_string())?
    }

    async fn remove_member(&self, member: [u8; 32]) -> Result<(), FleetRemovalRefusal> {
        forward(&self.transport, doors::REMOVE_MEMBER, &Member { member })
            .await
            .map_err(unavailable)?
    }
}

/// Answer one door of the fleet seam from `seam` — the core chunk's half.
/// `None` when `door` is not this seam's, so the dispatch can ask the next
/// seam and refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn FleetRemoval,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::RESOLVE_REMOVAL => {
            answer(payload, |a: RowClaim| async move {
                seam.resolve_removal(&a.row, a.claimed).await.map(Targets)
            })
            .await
        }
        doors::STAGE_REMOVAL => {
            answer(payload, |a: RowTargets| async move {
                seam.stage_removal(&a.row, a.targets).await
            })
            .await
        }
        doors::SETTLE_REMOVAL => {
            answer(payload, |a: Settle| async move {
                seam.settle_removal(&a.row, a.targets, a.outcome).await
            })
            .await
        }
        doors::FLEET_MEMBERS => {
            answer(payload, |a: Roster| async move {
                let roster = a.rows.into_iter().map(|r| (r.row, r.claimed)).collect();
                seam.fleet_members(roster).await
            })
            .await
        }
        doors::REMOVE_MEMBER => {
            answer(payload, |a: Member| async move {
                seam.remove_member(a.member).await
            })
            .await
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use fauna_account_port::loopback::Loopback;

    use super::*;
    use crate::machine::UnaccountedMember;

    /// A seam whose every method answers what it was told to — `ok` picks
    /// the arm — and records the arguments it saw.
    struct Scripted {
        ok: bool,
        seen: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(ok: bool) -> Arc<Self> {
            Arc::new(Self {
                ok,
                seen: Mutex::new(Vec::new()),
            })
        }
        fn saw(&self, what: String) {
            self.seen.lock().unwrap().push(what);
        }
    }

    fn view() -> FleetMembersView {
        FleetMembersView {
            me: [1; 32],
            unaccounted: vec![UnaccountedMember {
                device_id: [2; 32],
                enrolled_at_ms: -5,
            }],
        }
    }

    #[async_trait::async_trait]
    impl FleetRemoval for Scripted {
        async fn resolve_removal(
            &self,
            row: &str,
            claimed: Option<[u8; 32]>,
        ) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal> {
            self.saw(format!("resolve {row} {claimed:?}"));
            if self.ok {
                Ok(vec![[3; 32], [4; 32]])
            } else {
                Err(FleetRemovalRefusal::RowMismatch)
            }
        }
        async fn stage_removal(&self, row: &str, targets: Vec<[u8; 32]>) -> Result<(), String> {
            self.saw(format!("stage {row} {}", targets.len()));
            if self.ok {
                Ok(())
            } else {
                Err("stage refused".into())
            }
        }
        async fn settle_removal(
            &self,
            row: &str,
            targets: Vec<[u8; 32]>,
            outcome: NestDeletion,
        ) -> Result<(), String> {
            self.saw(format!("settle {row} {} {outcome:?}", targets.len()));
            if self.ok {
                Ok(())
            } else {
                Err("settle refused".into())
            }
        }
        async fn fleet_members(
            &self,
            roster: Vec<(String, Option<[u8; 32]>)>,
        ) -> Result<FleetMembersView, String> {
            self.saw(format!("members {roster:?}"));
            if self.ok {
                Ok(view())
            } else {
                Err("members refused".into())
            }
        }
        async fn remove_member(&self, member: [u8; 32]) -> Result<(), FleetRemovalRefusal> {
            self.saw(format!("remove {}", member[0]));
            if self.ok {
                Ok(())
            } else {
                Err(FleetRemovalRefusal::OwnDevice)
            }
        }
    }

    fn over(seam: Arc<Scripted>) -> PortFleetRemoval<Loopback> {
        PortFleetRemoval::new(Loopback::new(move |door, payload| {
            let seam = Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    /// Every method crosses and comes back with the seam's own answer, its
    /// arguments intact — both arms of every result.
    #[tokio::test]
    async fn every_method_round_trips_both_arms() {
        let seam = Scripted::new(true);
        let port = over(Arc::clone(&seam));
        assert_eq!(
            port.resolve_removal("AB", Some([9; 32])).await,
            Ok(vec![[3; 32], [4; 32]])
        );
        assert_eq!(port.stage_removal("AB", vec![[3; 32]]).await, Ok(()));
        assert_eq!(
            port.settle_removal("AB", vec![[3; 32]], NestDeletion::Unknown)
                .await,
            Ok(())
        );
        assert_eq!(
            port.fleet_members(vec![("r".into(), None)]).await,
            Ok(view())
        );
        assert_eq!(port.remove_member([7; 32]).await, Ok(()));
        assert_eq!(
            *seam.seen.lock().unwrap(),
            vec![
                format!("resolve AB {:?}", Some([9u8; 32])),
                "stage AB 1".to_string(),
                "settle AB 1 Unknown".to_string(),
                format!("members {:?}", vec![("r".to_string(), None::<[u8; 32]>)]),
                "remove 7".to_string(),
            ]
        );

        let port = over(Scripted::new(false));
        assert_eq!(
            port.resolve_removal("AB", None).await,
            Err(FleetRemovalRefusal::RowMismatch)
        );
        assert_eq!(
            port.stage_removal("AB", vec![]).await,
            Err("stage refused".to_string())
        );
        assert_eq!(
            port.settle_removal("AB", vec![], NestDeletion::Gone).await,
            Err("settle refused".to_string())
        );
        assert_eq!(
            port.fleet_members(vec![]).await,
            Err("members refused".to_string())
        );
        assert_eq!(
            port.remove_member([7; 32]).await,
            Err(FleetRemovalRefusal::OwnDevice)
        );
    }

    /// A faulting transport answers every method with its refusal — never a
    /// success, so a removal that cannot reach the runtime deletes nothing.
    #[tokio::test]
    async fn a_faulting_transport_is_refused_on_every_method() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortFleetRemoval::new(Loopback::faulting(fault.clone()));
            let said = fault.to_string();
            assert_eq!(
                port.resolve_removal("AB", None).await,
                Err(FleetRemovalRefusal::Unavailable(said.clone()))
            );
            assert_eq!(
                port.stage_removal("AB", vec![[1; 32]]).await,
                Err(said.clone())
            );
            assert_eq!(
                port.settle_removal("AB", vec![[1; 32]], NestDeletion::Gone)
                    .await,
                Err(said.clone())
            );
            assert_eq!(port.fleet_members(vec![]).await, Err(said.clone()));
            assert_eq!(
                port.remove_member([1; 32]).await,
                Err(FleetRemovalRefusal::Unavailable(said))
            );
        }
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name.
    #[tokio::test]
    async fn a_foreign_door_is_not_this_seams() {
        let seam = Scripted::new(true);
        assert!(serve(&*seam, "succession.load", &[]).await.is_none());
        for door in doors::ALL {
            assert!(serve(&*seam, door, &[0xff]).await.is_some(), "{door}");
        }
    }
}
