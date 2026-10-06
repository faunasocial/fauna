//! The **bind door** under test: the same two-party offline-share ceremony
//! the capstone proves, but driven entirely through
//! `group_ceremony_node` — the composition apps actually call
//! (`p2p.md` § Offline share initiation + `p2p-shared-set-build.md` § Cross-user shared-set
//! transfer → *Build contract*, "the listener is the contact-plane node, in the app
//! process").
//!
//! What this adds over `group_ceremony_over_wire.rs` (which hand-assembles a
//! `ShareServer` and hand-walks the carriage): the **brake** is what decides
//! whether a listener exists at all, and the initiator's walk is the shared
//! one every app gets rather than seven hand-rolled poll loops.
//!
//! No wall-clock waits: the ceremony clock is injected, and the consent gap
//! is stepped by the test (`poll_consent` answers `false`, the recipient's
//! driver consents, the same call answers `true`).

#![cfg(feature = "p2p-share")]

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use fauna_client_capabilities::group_ceremony::build_group_accept;
use fauna_client_capabilities::group_ceremony_node::{
    CeremonyBindRefusal, CeremonyBindVerdict, CeremonyDriveError, CeremonyNode,
    GroupShareInitiator, P2P_SHARE_CAPABILITY, Redial, admit_delivered_share,
    ceremony_bind_verdict, decline_group_share,
};
use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_generation::GroupReceptionKeyRecord;
use fauna_core::identity::ActorKeypair;
use fauna_transport::testing::{Listeners, MemTransport, await_listening, listeners};
use fauna_transport::{EndpointKey, PeerTransport};

const NOW: Timestamp = Timestamp(1_700_000_000);

fn alice() -> ActorKeypair {
    ActorKeypair::from_secret([21u8; 32])
}

fn bob() -> ActorKeypair {
    ActorKeypair::from_secret([31u8; 32])
}

fn mallory() -> ActorKeypair {
    ActorKeypair::from_secret([41u8; 32])
}

fn alices_device() -> SigningKey {
    SigningKey::from_bytes(&[0x41; 32])
}

fn alices_device_cert() -> Vec<u8> {
    let cert = DeviceAuthorization {
        actor_id: alice().actor_id(),
        device_key: alices_device().verifying_key().to_bytes(),
        capabilities: vec![Capability::RenewBearer],
        created_at: Timestamp(1_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(&alice(), &cert).expect("sign cert");
    canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
        .expect("encode carriage")
        .to_vec()
}

fn mem_transport(listeners: &Listeners, me: &ActorKeypair) -> Arc<dyn PeerTransport> {
    Arc::new(MemTransport {
        me: EndpointKey::from_bytes(me.actor_id().0),
        listeners: Arc::clone(listeners),
    })
}

/// A seat: the shared config the driver persists, and the bound node.
struct Seat {
    config: Arc<Mutex<GroupShareConfig>>,
    node: CeremonyNode,
}

async fn seat(listeners: &Listeners, who: ActorKeypair, name: &str) -> Seat {
    let config = Arc::new(Mutex::new(GroupShareConfig::default()));
    let node = CeremonyNode::bind(
        ceremony_bind_verdict(Some(&[P2P_SHARE_CAPABILITY.to_string()])),
        mem_transport(listeners, &who),
        who.actor_id(),
        name.into(),
        Arc::clone(&config),
        Arc::new(|| NOW),
        Arc::new(|| {}),
    )
    .await
    .map_err(|e| e.to_string())
    .expect("the brake is off — the door opens");
    await_listening(listeners, &who.actor_id().0).await;
    Seat { config, node }
}

/// The door will not open without positive brake evidence — the rule-7 client
/// half, checked where it cannot be forgotten.
#[tokio::test]
async fn the_listener_refuses_to_bind_without_the_p2p_share_token() {
    let listeners = listeners();
    let config = Arc::new(Mutex::new(GroupShareConfig::default()));

    for (verdict, expected) in [
        (
            ceremony_bind_verdict(None),
            CeremonyBindRefusal::NoBrakeEvidence,
        ),
        (
            ceremony_bind_verdict(Some(&["peer-sync".to_string()])),
            CeremonyBindRefusal::BrakeOn,
        ),
    ] {
        let refusal = CeremonyNode::bind(
            verdict,
            mem_transport(&listeners, &bob()),
            bob().actor_id(),
            "bob".into(),
            Arc::clone(&config),
            Arc::new(|| NOW),
            Arc::new(|| {}),
        )
        .await
        .map(|_| ())
        .expect_err("no token, no listener");
        assert_eq!(refusal, expected);
    }

    // And nothing is listening — the refusal is a door that never opened,
    // not a door that opened and then answered rudely.
    assert!(
        !listeners.lock().unwrap().contains_key(&bob().actor_id().0),
        "a refused bind must leave no listener behind"
    );
    assert_eq!(
        verdict_for(&[P2P_SHARE_CAPABILITY.to_string()]),
        CeremonyBindVerdict::Bind
    );
}

fn verdict_for(caps: &[String]) -> CeremonyBindVerdict {
    ceremony_bind_verdict(Some(caps))
}

/// The whole ceremony through the door apps call: two bound nodes, the code
/// compare, the receive act, the shared initiator walk, and a joiner that
/// ends up admitted by the full verification chain.
#[tokio::test]
async fn the_ceremony_runs_end_to_end_through_the_bound_nodes() {
    let listeners = listeners();
    let bob_seat = seat(&listeners, bob(), "bob").await;
    let alice_seat = seat(&listeners, alice(), "alice").await;

    // ── The in-person compare: each side reads its own code aloud. The code
    // IS the actor key, which is why comparing it is worth anything.
    assert_eq!(alice_seat.node.own_code(), alice().actor_id().to_hex());
    assert_eq!(bob_seat.node.own_code(), bob().actor_id().to_hex());

    // ── Before Bob's receive act, Alice cannot even open the ceremony.
    let channel = alice_seat
        .node
        .dial(bob().actor_id())
        .await
        .expect("the dial itself is fine — admission is per-kind");
    let mut premature = GroupShareInitiator::new(
        Arc::clone(&channel),
        Arc::clone(&alice_seat.config),
        bob().actor_id(),
    );
    premature
        .begin(&alice(), NOW)
        .await
        .map(|_| ())
        .expect_err("no receive act yet — the offer is refused");

    // ── Bob's receive act, from the code Alice read out.
    bob_seat.node.expect_share_from(alice().actor_id());

    // ── Alice's side, driven step by step so the consent gap is visible.
    let mut initiator = GroupShareInitiator::new(
        Arc::clone(&channel),
        Arc::clone(&alice_seat.config),
        bob().actor_id(),
    );
    let begun = initiator
        .begin(&alice(), NOW)
        .await
        .expect("the offer lands");
    let scope = begun.scope_id;
    assert_eq!(initiator.scope_id(), Some(scope));

    // Bob's real state machine recorded it, over the wire.
    {
        let cfg = bob_seat.config.lock().unwrap();
        assert_eq!(cfg.invited.len(), 1);
        assert_eq!(cfg.invited[0].initiator, alice().actor_id());
    }

    // ── The consent gap: pending is not an error, and not a hang.
    assert!(
        !initiator
            .poll_consent(&alice(), NOW)
            .await
            .expect("pending polls succeed"),
        "Bob's user has not answered yet"
    );

    // ── Bob consents (the `folder-share-accept-button` arm).
    let bob_reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    {
        let mut cfg = bob_seat.config.lock().unwrap();
        build_group_accept(&mut cfg, &bob(), &scope, &bob_reception, NOW).expect("bob consents");
    }
    assert!(
        initiator
            .poll_consent(&alice(), NOW)
            .await
            .expect("the accept is owed now"),
        "the same poll now carries the accept"
    );

    // ── Deliver, then admit through the full chain.
    let alice_reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let built = initiator
        .deliver(
            &alice(),
            &alices_device(),
            alices_device_cert(),
            &alice_reception,
            NOW,
        )
        .await
        .expect("the deliver crosses");

    let admitted = admit_delivered_share(&bob_seat.config, &bob(), &bob_reception, &scope, NOW)
        .expect("bob admits from bytes that crossed a real channel");
    assert_eq!(admitted.held_root_row.scope_id, scope);
    assert_eq!(admitted.rows, built.plane_rows);
    assert_eq!(admitted.generation_id, built.generation_id);

    // ── The receive act was never a blanket door: a stranger dialing the
    // very same live node, mid-ceremony state and all, is still refused.
    let mallory_seat = seat(&listeners, mallory(), "mallory").await;
    let mallory_channel = mallory_seat
        .node
        .dial(bob().actor_id())
        .await
        .expect("dial");
    let mut stranger = GroupShareInitiator::new(
        mallory_channel,
        Arc::clone(&mallory_seat.config),
        bob().actor_id(),
    );
    stranger
        .begin(&mallory(), NOW)
        .await
        .map(|_| ())
        .expect_err("no expectation or record names Mallory");
}

/// Rule 6, over the wire and through the node's own door: withdrawing the
/// receive act revokes admission. The barrier is causal — the cancel
/// happens-before the offer — so there is no timing in the assertion.
#[tokio::test]
async fn withdrawing_the_receive_act_revokes_admission_through_the_node() {
    let listeners = listeners();
    let bob_seat = seat(&listeners, bob(), "bob").await;
    let alice_seat = seat(&listeners, alice(), "alice").await;
    let channel = alice_seat.node.dial(bob().actor_id()).await.expect("dial");

    // Expect, then change your mind before anything crossed.
    bob_seat.node.expect_share_from(alice().actor_id());
    bob_seat.node.cancel_expectation(&alice().actor_id());

    GroupShareInitiator::new(channel, Arc::clone(&alice_seat.config), bob().actor_id())
        .begin(&alice(), NOW)
        .await
        .map(|_| ())
        .expect_err("the withdrawn expectation no longer admits Alice");

    // Nothing was recorded on Bob's side — a refused frame never reached the
    // state machine at all (rule 1: admission runs before parsing).
    assert!(bob_seat.config.lock().unwrap().invited.is_empty());
}

/// The addressing half, end to end: a ceremony whose ONLY path to the
/// counterpart is what the compare code carried.
///
/// This is the regression proof for the RED `p2p.md` § Offline share
/// initiation measured — the initiator's panel reporting
/// `iroh connect: No addressing information available` because the code was
/// the actor key and nothing else. The transport here refuses any dial whose
/// candidates do not name the listener's address, exactly as a real substrate
/// does, so a code that lost its addressing fails the test instead of quietly
/// succeeding through the in-memory map.
#[tokio::test]
async fn a_ceremony_dials_on_the_addressing_its_compare_code_carried() {
    use fauna_client_capabilities::group_ceremony_view::{PeerCode, parse_peer_code};
    use fauna_transport::testing::RequiresAddressing;

    let listeners = listeners();
    let bobs_addr: std::net::SocketAddr = "192.168.1.42:41234".parse().unwrap();

    // Bob's seat records where it bound the way the app does — from the
    // assembler, not from the seam.
    let bob_config = Arc::new(Mutex::new(GroupShareConfig::default()));
    let bob_node = CeremonyNode::bind(
        ceremony_bind_verdict(Some(&[P2P_SHARE_CAPABILITY.to_string()])),
        Arc::new(MemTransport {
            me: EndpointKey::from_bytes(bob().actor_id().0),
            listeners: Arc::clone(&listeners),
        }),
        bob().actor_id(),
        "bob".into(),
        Arc::clone(&bob_config),
        Arc::new(|| NOW),
        Arc::new(|| {}),
    )
    .await
    .expect("the brake is off — the door opens")
    .with_bound_addrs(vec![bobs_addr]);
    await_listening(&listeners, &bob().actor_id().0).await;

    // Alice is the one who DIALS, so hers is the transport that needs a path:
    // it refuses unless the dial's candidates name where Bob is.
    let alice_config = Arc::new(Mutex::new(GroupShareConfig::default()));
    let alice_node = CeremonyNode::bind(
        ceremony_bind_verdict(Some(&[P2P_SHARE_CAPABILITY.to_string()])),
        Arc::new(RequiresAddressing::new(
            MemTransport {
                me: EndpointKey::from_bytes(alice().actor_id().0),
                listeners: Arc::clone(&listeners),
            },
            bobs_addr,
        )),
        alice().actor_id(),
        "alice".into(),
        Arc::clone(&alice_config),
        Arc::new(|| NOW),
        Arc::new(|| {}),
    )
    .await
    .expect("the brake is off — the door opens");
    await_listening(&listeners, &alice().actor_id().0).await;

    // ── What Bob reads aloud now carries where he is, not just who he is.
    let bobs_code = bob_node.own_code();
    assert!(
        bobs_code.starts_with(&bob().actor_id().to_hex()),
        "the key is still the first thing in the code: {bobs_code}"
    );
    assert_ne!(
        bobs_code,
        bob().actor_id().to_hex(),
        "a bare key is precisely the code that could not dial"
    );

    // ── Alice types it. The parse is the app's own door.
    let typed = parse_peer_code(&bobs_code, &alice().actor_id()).expect("Alice types Bob's code");
    assert_eq!(typed.actor, bob().actor_id());
    assert_eq!(typed.lan_endpoints, vec![bobs_addr]);

    // ── The dial the affordance makes — and the whole point: it connects.
    alice_node
        .dial_code(&typed)
        .await
        .expect("the code carried addressing, so there is a path");

    // ── The counter-proof, so this test cannot pass for the wrong reason:
    // the same dial with the key alone is exactly the measured failure.
    let keyed_only = PeerCode {
        actor: bob().actor_id(),
        lan_endpoints: Vec::new(),
    };
    alice_node
        .dial_code(&keyed_only)
        .await
        .map(|_| ())
        .expect_err("no addressing information — the RED this slice resolves");
}

/// A redial for `from`'s seat back to `to` — the shape an app builds from
/// the compare code already typed.
fn redial_to(from: &Arc<Seat>, to: ActorKeypair) -> Redial {
    let seat = Arc::clone(from);
    let peer = to.actor_id();
    Arc::new(move || {
        let seat = Arc::clone(&seat);
        Box::pin(async move { seat.node.dial(peer).await })
    })
}

/// Wait on a condition the other half of a `join!` makes true. A bounded
/// sampling loop, never a timing assertion: the budget only stops a broken
/// test from hanging.
async fn until(what: &str, condition: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} never happened"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// `p2p.md` § Offline share initiation: from offer-recorded on, the durable
/// record admits the initiator, "so a dropped co-present connection redials
/// without re-scanning". The drive picks the SAME ceremony up again. It dials
/// again, keeps asking about the same scope and finishes, and the recipient
/// never repeats the receive act. The expectation is withdrawn before the
/// drop, so only the record can admit the redial.
#[tokio::test]
async fn a_connection_dropped_mid_ceremony_is_redialed_and_the_same_ceremony_finishes() {
    let listeners = listeners();
    let bob_seat = Arc::new(seat(&listeners, bob(), "bob").await);
    let alice_seat = Arc::new(seat(&listeners, alice(), "alice").await);
    bob_seat.node.expect_share_from(alice().actor_id());

    let channel = alice_seat.node.dial(bob().actor_id()).await.expect("dial");
    let mut initiator =
        GroupShareInitiator::new(channel, Arc::clone(&alice_seat.config), bob().actor_id())
            .with_redial(redial_to(&alice_seat, bob()));
    let alice_reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let bob_reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);

    let (alice_key, alice_device) = (alice(), alices_device());
    let drive = initiator.drive(
        &alice_key,
        &alice_device,
        alices_device_cert(),
        &alice_reception,
        NOW,
        &|_| {},
    );
    let recipient = async {
        until("the offer to be recorded on Bob's side", || {
            !bob_seat.config.lock().unwrap().invited.is_empty()
        })
        .await;
        // The receive act is withdrawn: from here on, only the record admits.
        bob_seat.node.cancel_expectation(&alice().actor_id());
        // The link drops while Alice is waiting for consent.
        assert!(
            bob_seat.node.close_inbound() >= 1,
            "Alice's connection must be open to be dropped"
        );
        let scope = bob_seat.config.lock().unwrap().invited[0].scope_id;
        let mut cfg = bob_seat.config.lock().unwrap();
        build_group_accept(&mut cfg, &bob(), &scope, &bob_reception, NOW).expect("bob consents");
    };
    let (driven, ()) = tokio::join!(drive, recipient);
    let driven = driven.expect("the redial picks the ceremony up and finishes it");

    // The same ceremony, not a second one: Bob holds one invitation, for the
    // scope Alice's drive reports, and its deliver arrived.
    {
        let cfg = bob_seat.config.lock().unwrap();
        assert_eq!(cfg.invited.len(), 1, "a redial is not a new offer");
        assert_eq!(cfg.invited[0].scope_id, driven.scope_id);
        assert!(!cfg.invited[0].deliver.is_empty());
    }
    let admitted = admit_delivered_share(
        &bob_seat.config,
        &bob(),
        &bob_reception,
        &driven.scope_id,
        NOW,
    )
    .expect("bob admits the deliver that crossed the redialed connection");
    assert_eq!(admitted.generation_id, driven.built.generation_id);
}

/// Without a redial wired, the same drop ends the drive and says what
/// happened, rather than surfacing as an anonymous transport failure.
#[tokio::test]
async fn a_dropped_connection_with_no_redial_is_reported_as_a_lost_connection() {
    let listeners = listeners();
    let bob_seat = seat(&listeners, bob(), "bob").await;
    let alice_seat = seat(&listeners, alice(), "alice").await;
    bob_seat.node.expect_share_from(alice().actor_id());

    let channel = alice_seat.node.dial(bob().actor_id()).await.expect("dial");
    let mut initiator =
        GroupShareInitiator::new(channel, Arc::clone(&alice_seat.config), bob().actor_id());
    let reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let (alice_key, alice_device) = (alice(), alices_device());
    let drive = initiator.drive(
        &alice_key,
        &alice_device,
        alices_device_cert(),
        &reception,
        NOW,
        &|_| {},
    );
    let recipient = async {
        until("the offer to be recorded on Bob's side", || {
            !bob_seat.config.lock().unwrap().invited.is_empty()
        })
        .await;
        assert!(bob_seat.node.close_inbound() >= 1);
    };
    let (driven, ()) = tokio::join!(drive, recipient);
    assert!(
        matches!(driven, Err(CeremonyDriveError::ConnectionLost(_))),
        "{:?}",
        driven.map(|d| d.scope_id)
    );
}

/// A refusal is the other side's answer and is never redialed: a declined
/// invitation ends the drive at once, even with a redial wired, instead of
/// asking again until the consent budget runs out.
#[tokio::test]
async fn a_declined_invitation_is_not_redialed() {
    let listeners = listeners();
    let bob_seat = Arc::new(seat(&listeners, bob(), "bob").await);
    let alice_seat = Arc::new(seat(&listeners, alice(), "alice").await);
    bob_seat.node.expect_share_from(alice().actor_id());

    let channel = alice_seat.node.dial(bob().actor_id()).await.expect("dial");
    let mut initiator =
        GroupShareInitiator::new(channel, Arc::clone(&alice_seat.config), bob().actor_id())
            .with_redial(redial_to(&alice_seat, bob()));
    let reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let (alice_key, alice_device) = (alice(), alices_device());
    let drive = initiator.drive(
        &alice_key,
        &alice_device,
        alices_device_cert(),
        &reception,
        NOW,
        &|_| {},
    );
    let recipient = async {
        until("the offer to be recorded on Bob's side", || {
            !bob_seat.config.lock().unwrap().invited.is_empty()
        })
        .await;
        let scope = bob_seat.config.lock().unwrap().invited[0].scope_id;
        assert!(decline_group_share(&bob_seat.config, &scope, NOW));
    };
    let (driven, ()) = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::join!(drive, recipient)
    })
    .await
    .expect("a decline ends the drive well inside the consent budget");
    assert!(
        matches!(driven, Err(CeremonyDriveError::Transport(_))),
        "{:?}",
        driven.map(|d| d.scope_id)
    );
}
