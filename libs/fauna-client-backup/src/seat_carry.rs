//! The writer-seat carry — an owner's device moving a rotated source box's
//! writer seat at a destination onto the box's new identity
//! (`docs/goal/architecture/segment-backup-protocol.md` § Cross-location backup
//! protocol → *The writer seat*, the *How the seat moves* and *Where the carry
//! runs* paragraphs).
//!
//! A source box that rotates its identity is refused by every destination: the
//! seat there still names the identity it had before. The destination's
//! handover (`fauna.backup.writer_grant.register { succeeds }`) moves the seat
//! and renames the predecessor's mirror sets, and it verifies **no chain** — the
//! owner is the authority over the grant. So the decision to name a predecessor
//! is made here, on the owner's device, and only on the strongest evidence it
//! has: a rotation chain fetched from the box the device is bound to, verified
//! hop by hop (`fauna_protocol::nest_rotation::verify_chain`, both signatures at
//! every hop) from the seat's holder to the bound identity.
//!
//! Never the destination acting on a chain the box presents, and never a carry
//! on anything weaker: that chain is licensed by the **superseded** key's
//! signature, the key a thief of a compromised box holds (*Why a device, and not
//! the destination on a chain*). The device's own pin of the bound identity is
//! what the chain must end at, and that pin is not the box's to say.
//!
//! What the carry does at one destination, given the seat it lists:
//!
//! | The seat | Outcome |
//! |---|---|
//! | none | [`SeatCarry::NoSeat`] — nothing to carry |
//! | held by the bound identity | [`SeatCarry::AlreadyHeld`] — nothing to do (a repeat run lands here) |
//! | revoked | [`SeatCarry::Revoked`] — the freeze outlives the rotation |
//! | held by an identity no verified chain links to the bound one | [`SeatCarry::Unlinked`] — another box's |
//! | held by a verified predecessor, in force | `register { writer: bound, succeeds: holder }` → [`SeatCarry::Carried`] |
//!
//! Two callers run it, each over a destination connection it already holds
//! ([`crate::trust::connect_destination`]): the audit pass, ahead of its
//! freshness arm ([`crate::audit::audit_all`]), and the Nests-page trust
//! facet's read ([`crate::trust::backup_trust_rows`]). Any of the owner's
//! devices may carry and the act is idempotent, so no device needs to have seen
//! the predecessor.

use async_trait::async_trait;
use fauna_core::data::{BackupDestination, DestinationKind};
use fauna_protocol::MaybeSendSync;
use fauna_protocol::backup::WriterGrantListReply;
use fauna_protocol::nest_rotation::{SignedNestRotation, verify_chain};

use crate::trust::BackupNestSeam;

/// What one destination's carry found, or did. Every variant is settled: a run
/// that could not ask (a failed read, a chain the bound nest would not serve, a
/// refused handover) is the `Err` of [`carry_writer_seat`] instead, and is
/// retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatCarry {
    /// The owner holds no seat at this destination: nothing to carry. An
    /// enrollment from this box takes the seat by itself.
    NoSeat,
    /// The seat already names the bound identity — never rotated, or carried
    /// by this or another of the owner's devices.
    AlreadyHeld,
    /// The seat names another identity and its grant is revoked: not carried.
    /// The freeze outlives the rotation; the owner's next enrollment of this
    /// destination from the rotated box names the predecessor then.
    Revoked,
    /// The seat names an identity no verified rotation chain links to the
    /// bound one: another box's seat, left alone.
    Unlinked,
    /// The seat named a verified predecessor and the destination has handed it
    /// to the bound identity.
    Carried,
}

/// Where the carry reads the bound box's rotation chain from. Asked only when a
/// seat names another identity — never on an ordinary pass.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RotationChainSource: MaybeSendSync {
    /// The chain of the box the device is bound to, oldest hop first.
    async fn rotation_chain(&self) -> Result<Vec<SignedNestRotation>, String>;
}

/// A [`RotationChainSource`] over a seam already open to the bound box — the
/// trust facet's home connection.
pub struct SeamChain<'a>(pub &'a dyn BackupNestSeam);

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl RotationChainSource for SeamChain<'_> {
    async fn rotation_chain(&self) -> Result<Vec<SignedNestRotation>, String> {
        self.0.rotation_chain().await
    }
}

/// Whether `destination` holds a writer seat a carry could move — a peer nest.
/// A client-device custodian pulls with the owner's own authority and holds no
/// seat; an inert row is dialled by nothing.
pub fn carries_a_seat(destination: &BackupDestination) -> bool {
    matches!(destination.kind_view(), DestinationKind::Nest { .. })
}

/// Read the owner's seat at `destination` and carry it to `bound` when a
/// verified chain links its holder there ([`carry_listed_seat`]).
///
/// # Errors
/// The seat could not be read, the chain could not be fetched, or the
/// destination refused the handover — nothing is settled, and a later run
/// tries again.
pub async fn carry_writer_seat(
    destination: &dyn BackupNestSeam,
    bound: &[u8; 32],
    chain: &dyn RotationChainSource,
) -> Result<SeatCarry, String> {
    let listed = destination.writer_grant_list().await?;
    carry_listed_seat(destination, &listed, bound, chain).await
}

/// [`carry_writer_seat`] over a seat list the caller has already read from
/// `destination` — the trust facet reads it for its row anyway.
///
/// # Errors
/// As [`carry_writer_seat`].
pub async fn carry_listed_seat(
    destination: &dyn BackupNestSeam,
    listed: &WriterGrantListReply,
    bound: &[u8; 32],
    chain: &dyn RotationChainSource,
) -> Result<SeatCarry, String> {
    // At most one seat per owner; a list naming the bound identity anywhere is
    // already carried, whatever else it carries.
    let seat = match listed
        .grants
        .iter()
        .find(|g| fauna_core::hex32::decode(&g.writer_nest_id).ok() == Some(*bound))
    {
        Some(_) => return Ok(SeatCarry::AlreadyHeld),
        None => match listed.grants.first() {
            Some(seat) => seat,
            None => return Ok(SeatCarry::NoSeat),
        },
    };
    if seat.revoked {
        return Ok(SeatCarry::Revoked);
    }
    let holder = fauna_core::hex32::decode(&seat.writer_nest_id)
        .map_err(|e| format!("the destination lists a malformed seat holder: {e}"))?;
    let hops = chain.rotation_chain().await?;
    if verify_chain(&hops, &holder, bound).is_err() {
        return Ok(SeatCarry::Unlinked);
    }
    destination
        .writer_grant_succeed(
            fauna_core::hex32::encode(bound),
            fauna_core::hex32::encode(&holder),
        )
        .await?;
    Ok(SeatCarry::Carried)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::backup::{
        BackupStatusReply, CustodyListReply, GenerationListReply, GenerationRestoreReply,
        WriterGrantItem,
    };
    use fauna_protocol::nest_rotation::NestRotation;
    use std::sync::Mutex;

    /// A destination's writer-grant plane, recording every handover it is
    /// asked for and applying it the way the nest does (the successor takes
    /// the seat, in force).
    #[derive(Default)]
    struct Destination {
        seat: Mutex<Vec<WriterGrantItem>>,
        handovers: Mutex<Vec<(String, String)>>,
        refuse_handover: bool,
    }

    impl Destination {
        fn holding(holder: &[u8; 32], revoked: bool) -> Self {
            Self {
                seat: Mutex::new(vec![WriterGrantItem {
                    writer_nest_id: fauna_core::hex32::encode(holder),
                    granted_at: 1_700_000_000,
                    revoked,
                    ..Default::default()
                }]),
                ..Default::default()
            }
        }

        fn handovers(&self) -> Vec<(String, String)> {
            self.handovers.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl BackupNestSeam for Destination {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            unreachable!("the carry never asks a destination for status")
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            unreachable!()
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            Ok(WriterGrantListReply {
                grants: self.seat.lock().unwrap().clone(),
                ..Default::default()
            })
        }
        async fn writer_grant_revoke(&self, _: String) -> Result<bool, String> {
            unreachable!("the carry never revokes")
        }
        async fn custody_list(&self, _: Option<String>) -> Result<CustodyListReply, String> {
            unreachable!()
        }
        async fn generation_list(&self, _: Option<String>) -> Result<GenerationListReply, String> {
            unreachable!()
        }
        async fn generation_restore(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<GenerationRestoreReply, String> {
            unreachable!()
        }
        async fn writer_grant_succeed(
            &self,
            writer_nest_id: String,
            succeeds: String,
        ) -> Result<(), String> {
            if self.refuse_handover {
                return Err("fauna.backup.writer_seat_held".into());
            }
            self.handovers
                .lock()
                .unwrap()
                .push((writer_nest_id.clone(), succeeds));
            *self.seat.lock().unwrap() = vec![WriterGrantItem {
                writer_nest_id,
                granted_at: 1_800_000_000,
                ..Default::default()
            }];
            Ok(())
        }
    }

    /// The bound box's chain, counting how often it is asked for.
    struct Chain {
        hops: Option<Vec<SignedNestRotation>>,
        asked: Mutex<usize>,
    }

    impl Chain {
        fn of(hops: Vec<SignedNestRotation>) -> Self {
            Self {
                hops: Some(hops),
                asked: Mutex::new(0),
            }
        }
        fn unservable() -> Self {
            Self {
                hops: None,
                asked: Mutex::new(0),
            }
        }
        fn asked(&self) -> usize {
            *self.asked.lock().unwrap()
        }
    }

    #[async_trait]
    impl RotationChainSource for Chain {
        async fn rotation_chain(&self) -> Result<Vec<SignedNestRotation>, String> {
            *self.asked.lock().unwrap() += 1;
            self.hops
                .clone()
                .ok_or_else(|| "kind not registered: fauna.auth.rotation_chain".into())
        }
    }

    fn key(byte: u8) -> ActorKeypair {
        ActorKeypair::from_secret([byte; 32])
    }

    fn id(k: &ActorKeypair) -> [u8; 32] {
        k.actor_id().0
    }

    /// The signed hop `old → new`, minted with both real keys exactly as the
    /// rotation transaction mints it.
    fn hop(old: &ActorKeypair, new: &ActorKeypair, seq: u64) -> SignedNestRotation {
        NestRotation {
            old_nest_actor_id: id(old),
            new_nest_actor_id: id(new),
            seq,
            rotated_at: 1_800_000_000 + seq as i64,
        }
        .sign(old.signing_key(), new.signing_key())
        .expect("sign the hop")
    }

    /// The seat names the predecessor, the bound box's chain verifies the hop
    /// to the bound identity: the device hands the seat over, naming the
    /// predecessor — and a second run finds it carried and does nothing.
    #[test]
    fn a_verified_chain_carries_the_seat_and_a_second_run_is_a_no_op() {
        let (p, b) = (key(0x11), key(0x22));
        let dest = Destination::holding(&id(&p), false);
        let chain = Chain::of(vec![hop(&p, &b, 1)]);

        let outcome = block_on(carry_writer_seat(&dest, &id(&b), &chain)).unwrap();

        assert_eq!(outcome, SeatCarry::Carried);
        assert_eq!(
            dest.handovers(),
            vec![(
                fauna_core::hex32::encode(&id(&b)),
                fauna_core::hex32::encode(&id(&p))
            )],
            "the successor registered, naming the predecessor it succeeds"
        );

        let again = block_on(carry_writer_seat(&dest, &id(&b), &chain)).unwrap();
        assert_eq!(again, SeatCarry::AlreadyHeld);
        assert_eq!(dest.handovers().len(), 1, "the repeat issues nothing");
        assert_eq!(chain.asked(), 1, "and asks for no chain");
    }

    /// A box rotated twice: a device whose destination still names the
    /// grandparent carries in one step, the chain walking both hops.
    #[test]
    fn a_seat_several_rotations_back_is_carried_in_one_step() {
        let (g, p, b) = (key(0x10), key(0x11), key(0x22));
        let dest = Destination::holding(&id(&g), false);
        let chain = Chain::of(vec![hop(&g, &p, 1), hop(&p, &b, 2)]);

        assert_eq!(
            block_on(carry_writer_seat(&dest, &id(&b), &chain)).unwrap(),
            SeatCarry::Carried
        );
        assert_eq!(dest.handovers()[0].1, fauna_core::hex32::encode(&id(&g)));
    }

    /// A seat whose holder no verified chain links to the bound identity is
    /// another box's: nothing is registered. Covers the forged hop too — a
    /// statement claiming the link, signed by a key that is not the holder's.
    #[test]
    fn no_chain_linking_holder_to_bound_carries_nothing() {
        let (p, b, stranger) = (key(0x11), key(0x22), key(0x33));

        // The bound box never rotated: an empty chain.
        let dest = Destination::holding(&id(&stranger), false);
        let empty = Chain::of(Vec::new());
        assert_eq!(
            block_on(carry_writer_seat(&dest, &id(&b), &empty)).unwrap(),
            SeatCarry::Unlinked
        );

        // A chain that is real but starts elsewhere.
        let elsewhere = Chain::of(vec![hop(&p, &b, 1)]);
        assert_eq!(
            block_on(carry_writer_seat(&dest, &id(&b), &elsewhere)).unwrap(),
            SeatCarry::Unlinked
        );

        // A hop claiming `stranger → b`, its old signature made by `p`.
        let mut forged = hop(&p, &b, 1);
        forged.statement.old_nest_actor_id = id(&stranger);
        let forged = Chain::of(vec![forged]);
        assert_eq!(
            block_on(carry_writer_seat(&dest, &id(&b), &forged)).unwrap(),
            SeatCarry::Unlinked
        );

        assert!(dest.handovers().is_empty(), "nothing was registered");
    }

    /// A revoked seat is not carried, even on a verified chain: the freeze
    /// outlives the rotation. The chain is not even asked for.
    #[test]
    fn a_revoked_seat_is_not_carried() {
        let (p, b) = (key(0x11), key(0x22));
        let dest = Destination::holding(&id(&p), true);
        let chain = Chain::of(vec![hop(&p, &b, 1)]);

        assert_eq!(
            block_on(carry_writer_seat(&dest, &id(&b), &chain)).unwrap(),
            SeatCarry::Revoked
        );
        assert!(dest.handovers().is_empty());
        assert_eq!(chain.asked(), 0);
    }

    /// No seat, or the seat already the bound identity's: nothing to carry,
    /// and no chain is fetched on an ordinary pass.
    #[test]
    fn nothing_to_carry_asks_for_no_chain() {
        let b = key(0x22);
        let chain = Chain::unservable();

        let empty = Destination::default();
        assert_eq!(
            block_on(carry_writer_seat(&empty, &id(&b), &chain)).unwrap(),
            SeatCarry::NoSeat
        );
        let held = Destination::holding(&id(&b), false);
        assert_eq!(
            block_on(carry_writer_seat(&held, &id(&b), &chain)).unwrap(),
            SeatCarry::AlreadyHeld
        );
        assert_eq!(chain.asked(), 0);
        assert!(empty.handovers().is_empty() && held.handovers().is_empty());
    }

    /// A chain the bound nest cannot serve, or a handover the destination
    /// refuses, settles nothing: the run is an `Err`, retried later.
    #[test]
    fn an_unservable_chain_or_a_refused_handover_is_retried() {
        let (p, b) = (key(0x11), key(0x22));
        let dest = Destination::holding(&id(&p), false);
        assert!(block_on(carry_writer_seat(&dest, &id(&b), &Chain::unservable())).is_err());
        assert!(dest.handovers().is_empty());

        let refusing = Destination {
            refuse_handover: true,
            ..Destination::holding(&id(&p), false)
        };
        assert!(
            block_on(carry_writer_seat(
                &refusing,
                &id(&b),
                &Chain::of(vec![hop(&p, &b, 1)])
            ))
            .is_err()
        );
    }

    /// **The carry never registers without `succeeds`.** The seam it speaks
    /// has no such call (`BackupNestSeam::writer_grant_succeed` takes the
    /// predecessor by value, not as an option), so every handover a carry
    /// issues names the seat's holder — pinned here across every outcome.
    #[test]
    fn every_registration_the_carry_issues_names_the_seat_holder() {
        let (g, p, b) = (key(0x10), key(0x11), key(0x22));
        for (holder, hops) in [
            (id(&p), vec![hop(&p, &b, 1)]),
            (id(&g), vec![hop(&g, &p, 1), hop(&p, &b, 2)]),
        ] {
            let dest = Destination::holding(&holder, false);
            block_on(carry_writer_seat(&dest, &id(&b), &Chain::of(hops))).unwrap();
            for (writer, succeeds) in dest.handovers() {
                assert_eq!(writer, fauna_core::hex32::encode(&id(&b)));
                assert_eq!(succeeds, fauna_core::hex32::encode(&holder));
            }
        }
    }
}
