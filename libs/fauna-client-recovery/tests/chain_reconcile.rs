//! **The chain follows the link** — the registration-chain reconcile's four
//! outcomes, driven over two faithful in-memory nests (`support::FakeNest`
//! runs the real `fauna_core::recovery` verification on every submit, so a
//! reconcile that replayed the wrong bytes, or in the wrong order, is refused
//! here as a real nest would refuse it).
//!
//! Owner goal doc: `docs/goal/behavior/identity-succession.md` § Enforcement
//! on the home nest → *Every nest the identity is linked to*.

mod support;

use fauna_client_recovery::chain_reconcile::{
    ChainReconcile, ChainReconcileError, ChainSide, RpcChainDoor, reconcile_registration_chains,
};
use fauna_client_recovery::{RecoveryClient, create_kit_with_root};
use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::{RecoveryKey, RecoveryKeyRegistration};
use support::FakeNest;

/// One identity signed in at two nests: the one its runtime is bound to and
/// one it links.
fn two_nests() -> (
    ActorKeypair,
    RecoveryClient<FakeNest>,
    RecoveryClient<FakeNest>,
) {
    let identity = ActorKeypair::generate();
    let nest = || RecoveryClient::new(FakeNest::new().signed_in_as(&identity.actor_id()));
    let (bound, linked) = (nest(), nest());
    (identity, bound, linked)
}

async fn reconcile(
    identity: &ActorKeypair,
    bound: &RecoveryClient<FakeNest>,
    linked: &RecoveryClient<FakeNest>,
) -> Result<ChainReconcile, ChainReconcileError> {
    fn door<'a>(
        nest: &'a RecoveryClient<FakeNest>,
        identity: &ActorKeypair,
    ) -> RpcChainDoor<'a, FakeNest> {
        RpcChainDoor {
            rpc: nest.transport(),
            actor_id: identity.actor_id().0,
        }
    }
    reconcile_registration_chains(&door(bound, identity), &door(linked, identity)).await
}

/// A kit minted (or, with `prior`, replaced under the prior kit's authority)
/// at `nest`.
async fn kit(
    nest: &RecoveryClient<FakeNest>,
    identity: &ActorKeypair,
    prior: Option<&RecoveryKey>,
    root: u8,
) -> RecoveryKey {
    create_kit_with_root(
        nest,
        identity,
        prior,
        RecoveryKey::from_bytes([root; 32]),
        &[],
    )
    .await
    .expect("the kit ceremony lands");
    RecoveryKey::from_bytes([root; 32])
}

/// A seed-alone replacement landed by `nest` itself, as after an uncontested
/// window — the one link that carries no prior-key signature.
fn land_seed_alone(nest: &RecoveryClient<FakeNest>, identity: &ActorKeypair, root: u8) {
    let actor = identity.actor_id();
    let head = nest.transport().head(&actor).expect("a head to replace");
    let record = RecoveryKeyRegistration {
        actor_id: actor,
        recovery_pubkey: RecoveryKey::from_bytes([root; 32]).public(),
        seq: head.seq + 1,
        created_at: Timestamp::now(),
    }
    .sign(
        identity.signing_key(),
        &RecoveryKey::from_bytes([root; 32]),
        None,
    )
    .unwrap();
    nest.transport()
        .land_seed_alone(&actor, canonical_encode(&record).unwrap());
}

#[tokio::test]
async fn two_nests_holding_the_same_chain_are_in_step() {
    let (identity, bound, linked) = two_nests();
    assert_eq!(
        reconcile(&identity, &bound, &linked).await,
        Ok(ChainReconcile::InStep),
        "no kit anywhere is the same chain at both"
    );

    kit(&bound, &identity, None, 0x21).await;
    reconcile(&identity, &bound, &linked).await.unwrap();
    assert_eq!(
        reconcile(&identity, &bound, &linked).await,
        Ok(ChainReconcile::InStep),
        "a carried chain needs nothing more"
    );
    assert_eq!(linked.transport().chain_len(&identity.actor_id()), 1);
}

#[tokio::test]
async fn the_longer_chain_extends_the_other_whichever_side_holds_it() {
    let (identity, bound, linked) = two_nests();
    let actor = identity.actor_id();
    let first = kit(&bound, &identity, None, 0x21).await;
    kit(&bound, &identity, Some(&first), 0x22).await;

    assert_eq!(
        reconcile(&identity, &bound, &linked).await,
        Ok(ChainReconcile::Extended {
            side: ChainSide::Linked,
            records: 2,
        })
    );
    assert_eq!(
        linked.transport().chain(&actor),
        bound.transport().chain(&actor),
        "the linked nest holds the bound nest's records verbatim, in order"
    );

    // The other direction: a kit replaced while a device was bound to the
    // linked nest reaches the bound one.
    let second = RecoveryKey::from_bytes([0x22; 32]);
    kit(&linked, &identity, Some(&second), 0x23).await;
    assert_eq!(
        reconcile(&identity, &bound, &linked).await,
        Ok(ChainReconcile::Extended {
            side: ChainSide::Bound,
            records: 1,
        })
    );
    assert_eq!(
        bound.transport().chain(&actor),
        linked.transport().chain(&actor)
    );
}

#[tokio::test]
async fn two_different_first_registrations_are_forked_and_nothing_is_submitted() {
    let (identity, bound, linked) = two_nests();
    let actor = identity.actor_id();
    kit(&bound, &identity, None, 0x21).await;
    // What a seed thief does at a nest that held no chain: a kit of their own.
    kit(&linked, &identity, None, 0x66).await;
    let (before_bound, before_linked) = (
        bound.transport().chain(&actor),
        linked.transport().chain(&actor),
    );

    assert_eq!(
        reconcile(&identity, &bound, &linked).await,
        Ok(ChainReconcile::Forked)
    );
    assert_eq!(bound.transport().chain(&actor), before_bound);
    assert_eq!(linked.transport().chain(&actor), before_linked);
}

#[tokio::test]
async fn a_seed_alone_link_is_owed_and_the_links_before_it_still_land() {
    let (identity, bound, linked) = two_nests();
    let actor = identity.actor_id();
    kit(&bound, &identity, None, 0x21).await;
    land_seed_alone(&bound, &identity, 0x22);

    assert_eq!(
        reconcile(&identity, &bound, &linked).await,
        Ok(ChainReconcile::Owed {
            side: ChainSide::Linked,
            submitted: 1,
            requested: true,
        }),
        "the first registration replays; the landed seed-alone link cannot"
    );
    assert_eq!(linked.transport().chain_len(&actor), 1);

    // Asked again, nothing is left to replay and the link stays owed — never
    // a submit the linked nest would have to refuse.
    assert_eq!(
        reconcile(&identity, &bound, &linked).await,
        Ok(ChainReconcile::Owed {
            side: ChainSide::Linked,
            submitted: 0,
            requested: true,
        })
    );
    assert_eq!(linked.transport().chain_len(&actor), 1);
}

/// Clause (c): the owed seed-alone link is requested at the lagging nest, which
/// parks it for its own window — so the honest seed-alone replacement reaches
/// every linked nest, 30 days later, and a replayed request keeps the window
/// the first one opened.
#[tokio::test]
async fn an_owed_seed_alone_link_opens_its_own_window_at_the_lagging_nest() {
    let (identity, bound, linked) = two_nests();
    let actor = identity.actor_id();
    kit(&bound, &identity, None, 0x21).await;
    land_seed_alone(&bound, &identity, 0x22);
    assert!(linked.transport().pending(&actor).is_none());

    reconcile(&identity, &bound, &linked).await.unwrap();
    let pending = linked
        .transport()
        .pending(&actor)
        .expect("the linked nest parked the owed link");
    assert_eq!(
        pending.new_recovery_pubkey.as_ref(),
        RecoveryKey::from_bytes([0x22; 32]).public().as_slice(),
        "the window is for the very key the bound nest landed"
    );
    // The window is that nest's own; the chain there is untouched until it lands.
    assert_eq!(linked.transport().chain_len(&actor), 1);
}

#[tokio::test]
async fn a_nest_that_refuses_a_record_is_an_error_naming_that_side() {
    let (identity, bound, linked) = two_nests();
    kit(&bound, &identity, None, 0x21).await;
    linked
        .transport()
        .forget_kind("fauna.recovery.registration.submit");

    let err = reconcile(&identity, &bound, &linked).await.unwrap_err();
    assert!(
        matches!(
            err,
            ChainReconcileError::Submit {
                side: ChainSide::Linked,
                ..
            }
        ),
        "{err}"
    );
}
