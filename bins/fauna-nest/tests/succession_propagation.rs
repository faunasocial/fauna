//! **tier_3** — identity-succession slice 4: federation propagation, both legs,
//! between two real in-process nests over a real `fauna.federation.hello`
//! handshake and a real anonymous WS-RPC dial.
//!
//! Goal doc: `identity-succession.md:81` § Propagation → *Federation peers* —
//! "the home nest pushes the statement to every peer it has residue with (a new
//! federation kind), and peers can pull the chain from the old identity's home.
//! A peer that verifies it: persists its own succession row, re-points its
//! remote-identity residue …, and refuses *new* content signed by the superseded
//! key. Historical content keeps verifying and stays attributed to the old id."
//!
//! **The property under test is that the receiving peer trusts nothing.** A
//! federation push arrives with a verified `origin_nest_id`, but a nest id is
//! self-minted and free — so unlike every gated federation kind, this one has no
//! authorization gate at all, and must not need one: the statement's own
//! `recovery_sig` plus the delivered registration chain (each link signed by the
//! identity's own seed) is the whole verdict. Tests here drive that from both a
//! benign home nest and a lying one.
//!
//! The two legs are deliberately proven the same way, because the doc promises
//! they reach the same state: a peer that receives the push and a peer that
//! receives nothing but asks must end up identical.

mod common;
use common::identity;
use common::signed_inbox_payload;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorId;
use fauna_core::recovery::{IdentitySuccession, RecoveryKey, RecoveryKeyRegistration};
use fauna_nest::db::CacheDb;
use fauna_nest::db::channels::RebindPower;
use fauna_nest::federation_handlers::{FedSuccessionPushReply, FedSuccessionPushRequest};
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{
    account_handlers, auth_handlers, discovery_handlers, federation_handlers, inbox_handlers,
    recovery_handlers,
};
use fauna_protocol::recovery::{
    RegistrationSubmitReply, RegistrationSubmitRequest, SuccessionSubmitReply,
    SuccessionSubmitRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_bytes::ByteBuf;

/// A shared channel id — the residue edge that makes a peer a propagation
/// target in one direction and a puller in the other.
const CHANNEL: [u8; 32] = [0x70; 32];

/// A second channel — carries the *later-added* residue binding in the
/// anchor-diversion pin.
const CHANNEL2: [u8; 32] = [0x71; 32];

/// A third channel — carries the *planted* binding in the anchor-URL denial
/// pins, which is about an unrelated actor's row on its own channel.
const CHANNEL3: [u8; 32] = [0x72; 32];

/// Generous ceiling for the detached push fan-out to land. Sized far above any
/// non-pathological delay: a green run never pays it (the poll returns as soon
/// as the state is there), and it is a deadline, never a settle-sleep
/// (`testing.md` § convention 14).
const FANOUT_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

// ═════════════════════════════════════════════════════════════════════════════
// Harness
// ═════════════════════════════════════════════════════════════════════════════

/// A real nest on loopback: own identity, federation router, client router, and
/// the anonymous discovery + recovery surfaces both propagation legs need.
async fn start_nest() -> (String, Arc<RpcRouter>, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let client_router = Arc::new({
        let mut b = RpcRouter::builder();
        recovery_handlers::register_recovery_handlers(&mut b);
        auth_handlers::register_auth_handlers(&mut b);
        account_handlers::register_account_handlers(&mut b);
        discovery_handlers::register_discovery_handlers(&mut b);
        inbox_handlers::register_inbox_handlers(&mut b);
        b.build()
    });
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::clone(&client_router),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), client_router, state)
}

fn registration_bytes(
    seed: &SigningKey,
    actor: [u8; 32],
    recovery: &RecoveryKey,
    seq: u64,
) -> Vec<u8> {
    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(actor),
        recovery_pubkey: recovery.public(),
        seq,
        created_at: Timestamp(1_753_000_000),
    };
    canonical_encode(&reg.sign(seed, recovery, None).expect("sign registration")).expect("encode")
}

fn succession_bytes(
    recovery: &RecoveryKey,
    old: [u8; 32],
    new_seed: &SigningKey,
    seq: u64,
) -> Vec<u8> {
    let statement = IdentitySuccession {
        old_actor_id: ActorId(old),
        new_actor_id: ActorId(new_seed.verifying_key().to_bytes()),
        recovery_pubkey: recovery.public(),
        seq,
        created_at: Timestamp(1_753_200_000),
    };
    canonical_encode(
        &statement
            .sign(recovery, new_seed, None)
            .expect("sign statement"),
    )
    .expect("encode")
}

/// An account with a handle and a registered RecoveryKey, on its home nest.
async fn account_with_recovery_key(
    router: &RpcRouter,
    state: &Arc<AppState>,
    seed: &SigningKey,
    actor: [u8; 32],
    recovery: &RecoveryKey,
    handle: &str,
) {
    state
        .db
        .create_user_with_handle(&actor, "free", handle, None)
        .await
        .expect("account created");
    let _: RegistrationSubmitReply = common::call(
        router,
        state,
        actor,
        "fauna.recovery.registration.submit",
        &RegistrationSubmitRequest {
            registration: ByteBuf::from(registration_bytes(seed, actor, recovery, 1)),
            extra: Default::default(),
        },
    )
    .await
    .expect("registration lands");
}

/// Give the peer the residue a real cross-nest conversation would have left:
/// `alice` is a foreign member of a channel homed here, and a local user has a
/// contact edge to her.
async fn seed_peer_residue(
    peer: &Arc<AppState>,
    home_nest_id: [u8; 32],
    home_url: &str,
    alice: [u8; 32],
    local: [u8; 32],
) {
    peer.db
        .create_user_with_handle(&local, "free", "bob", None)
        .await
        .expect("local account");
    peer.db
        .register_foreign_channel_member(
            &CHANNEL,
            &alice,
            &home_nest_id,
            Some(home_url),
            RebindPower::Standing,
        )
        .await
        .expect("foreign member");
    peer.db
        .accept_contact(&local, &alice)
        .await
        .expect("contact edge");
}

/// Give the *home* nest the residue that makes the peer a push target: alice is
/// on a channel whose roster includes a member homed at the peer.
async fn seed_home_residue(
    home: &Arc<AppState>,
    alice: [u8; 32],
    peer_nest_id: [u8; 32],
    peer_url: &str,
) {
    home.db
        .register_actor_channel(&alice, &CHANNEL)
        .await
        .expect("alice is on the channel");
    home.db
        .register_foreign_channel_member(
            &CHANNEL,
            &[0xBB; 32],
            &peer_nest_id,
            Some(peer_url),
            RebindPower::Standing,
        )
        .await
        .expect("foreign member on the home nest");
}

async fn foreign_members(state: &Arc<AppState>) -> Vec<[u8; 32]> {
    state
        .db
        .list_foreign_channel_members(&CHANNEL)
        .await
        .unwrap()
}

async fn contact_peer_ids(state: &Arc<AppState>, owner: [u8; 32]) -> Vec<Vec<u8>> {
    state
        .db
        .list_contacts(&owner)
        .await
        .unwrap()
        .into_iter()
        .map(|(peer, _status)| peer)
        .collect()
}

/// Relay an inbox payload to the peer over the **federation** kind — the path a
/// superseded author's content actually arrives on, and the one with no
/// authenticated caller at all. `fauna.inbox.send` is deliberately *not* used:
/// it requires the sender to hold an account on the receiving nest, which a
/// remote author never does, so it could not model this arrival.
async fn relay_inbox(
    conn: &fauna_nest::federation_channel::FederationConnection,
    idem: u8,
    payload: Vec<u8>,
    recipient: [u8; 32],
) -> Result<fauna_nest::federation_handlers::FedInboxDeliverReply, RpcError> {
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.inbox.deliver",
            [idem; 16],
            to_value(&fauna_nest::federation_handlers::FedInboxDeliverRequest {
                recipient_actor_id: hex::encode(recipient),
                payload_bytes: payload,
            }),
            None,
        )
        .await
        .expect("request rides the channel");
    call.await_reply().await.map(|v| from_value(&v))
}

// ═════════════════════════════════════════════════════════════════════════════
// The push leg
// ═════════════════════════════════════════════════════════════════════════════

/// **The capstone.** Home nest H pushes over the ordinary federation handshake;
/// peer P — which has never heard of the RecoveryKey — verifies from the bytes,
/// records the link, re-points every piece of residue, and starts refusing new
/// content from the superseded key while the *same* bytes delivered before.
#[tokio::test]
async fn a_pushed_succession_re_points_peer_residue_and_refuses_the_old_key() {
    let (home_url, home_router, home) = start_nest().await;
    let (peer_url, peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();
    let peer_nest_id = peer.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);

    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &recovery, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;
    seed_home_residue(&home, alice, peer_nest_id, &peer_url).await;

    // Alice's home nest is also what relays her content to the peer, so one
    // channel serves both the precondition and the post-succession assertion.
    let relay = fauna_nest::federation_channel::dial(
        &home,
        &peer_url,
        &hex::encode(peer.nest_identity.public_key_bytes()),
    )
    .await
    .expect("federation channel establishes");

    // Precondition: the peer accepts alice's relayed content today, so the
    // refusal below cannot pass for an unrelated reason.
    relay_inbox(
        &relay,
        1,
        signed_inbox_payload(&alice_seed, alice, bob),
        bob,
    )
    .await
    .expect("precondition: the peer accepts alice's content before the succession");

    // Alice succeeds her identity on her home nest. Nothing in this call names
    // the peer — the fan-out finds it from residue.
    let statement = succession_bytes(&recovery, alice, &successor_seed, 2);
    let _: SuccessionSubmitReply = common::call(
        &home_router,
        &home,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(statement.clone()),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands on the home nest");

    // The push is detached — the owner's recovery must never wait on a peer's
    // availability — so poll for the state rather than assuming an ordering.
    let deadline = std::time::Instant::now() + FANOUT_BUDGET;
    loop {
        if peer.db.succession_for(&alice[..]).await.unwrap().is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the peer never learned the succession within {FANOUT_BUDGET:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // The link the peer stored is the statement's own bytes, verbatim.
    let row = peer.db.succession_for(&alice[..]).await.unwrap().unwrap();
    assert_eq!(row.new_actor_id, successor.to_vec());
    assert_eq!(row.statement, statement);

    // Residue re-pointed, not duplicated or dropped.
    assert_eq!(foreign_members(&peer).await, vec![successor]);
    assert_eq!(contact_peer_ids(&peer, bob).await, vec![successor.to_vec()]);

    // And the enforcement the whole propagation exists for: the stolen key's
    // *new* content is refused at the one path with no authenticated caller.
    let err = relay_inbox(
        &relay,
        2,
        signed_inbox_payload(&alice_seed, alice, bob),
        bob,
    )
    .await
    .expect_err("new content from the superseded key must be refused on the peer");
    assert!(
        format!("{err:?}").contains("superseded"),
        "expected a supersession refusal, got {err:?}"
    );
    let _ = peer_router;
}

/// A verified succession folds a holder's two contact edges into one, and the
/// stricter status survives in both directions (`succession-aftermath.md`
/// § Propagation → Contacts): bob had blocked alice and accepted her successor
/// before learning they were the same person — the collapse may re-point who
/// the edge names, never un-block her behind bob's back; carol's mirror
/// (accepted the predecessor, blocked the successor) keeps her block the same
/// way.
#[tokio::test]
async fn a_pushed_succession_collision_keeps_the_blocked_edge() {
    let (home_url, home_router, home) = start_nest().await;
    let (peer_url, peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();
    let peer_nest_id = peer.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);
    let carol = [0x55; 32];

    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &recovery, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;
    seed_home_residue(&home, alice, peer_nest_id, &peer_url).await;
    peer.db
        .create_user_with_handle(&carol, "free", "carol", None)
        .await
        .expect("carol's account");

    // The colliding pairs, minted through the production writers: bob blocked
    // the predecessor and accepted the successor; carol the mirror.
    peer.db.block_contact(&bob, &alice).await.unwrap();
    peer.db.accept_contact(&bob, &successor).await.unwrap();
    peer.db.accept_contact(&carol, &alice).await.unwrap();
    peer.db.block_contact(&carol, &successor).await.unwrap();

    let statement = succession_bytes(&recovery, alice, &successor_seed, 2);
    let _: SuccessionSubmitReply = common::call(
        &home_router,
        &home,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(statement.clone()),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands on the home nest");

    let deadline = std::time::Instant::now() + FANOUT_BUDGET;
    loop {
        if peer.db.succession_for(&alice[..]).await.unwrap().is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the peer never learned the succession within {FANOUT_BUDGET:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    for holder in [bob, carol] {
        assert_eq!(
            peer.db
                .get_contact_status(&holder, &successor)
                .await
                .unwrap()
                .as_deref(),
            Some("blocked"),
            "the block must survive the collapse for holder {:02x}",
            holder[0]
        );
        assert!(
            peer.db
                .get_contact_status(&holder, &alice)
                .await
                .unwrap()
                .is_none(),
            "no edge may keep naming the superseded identity"
        );
    }
    let _ = peer_router;
}

/// A lying relay's push teaches the peer nothing. The pusher presents a
/// structurally perfect statement signed by a RecoveryKey it minted itself
/// (the push no longer carries a chain to pair with it — that field left the
/// wire with the compat-remnant sweep); the peer takes only "go check alice"
/// from it, dials the anchor it
/// already holds for her, finds no succession there, and learns nothing.
#[tokio::test]
async fn a_push_the_peer_cannot_verify_from_the_bytes_changes_nothing() {
    let (home_url, _home_router, home) = start_nest().await;
    let (peer_url, _peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();
    let peer_nest_id = peer.nest_identity.public_key_bytes();

    let (_alice_seed, alice) = identity(0x11);
    let (_bob_seed, bob) = identity(0x44);
    let (thief_seed, _thief) = identity(0x77);
    let attacker_key = RecoveryKey::from_bytes([0x99; 32]);
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    // A statement about alice signed by a key the attacker controls.
    let req = FedSuccessionPushRequest {
        statement: succession_bytes(&attacker_key, alice, &thief_seed, 2),
    };

    let conn = fauna_nest::federation_channel::dial(
        &home,
        &peer_url,
        &hex::encode(peer.nest_identity.public_key_bytes()),
    )
    .await
    .expect("federation channel establishes");
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.succession.push",
            [9u8; 16],
            to_value(&req),
            None,
        )
        .await
        .expect("request rides the channel");
    let reply: FedSuccessionPushReply = from_value(&call.await_reply().await.unwrap());
    assert!(
        !reply.recorded,
        "a push the anchor does not confirm must teach the peer nothing"
    );

    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "a refused push must leave no link"
    );
    assert_eq!(
        foreign_members(&peer).await,
        vec![alice],
        "a refused push must leave residue untouched"
    );
    let _ = peer_nest_id;
}

/// **The seed thief, first shape.** The adversary this whole plane exists to
/// defeat holds alice's identity seed — so unlike the lying relay above, every
/// `seed_sig` in a chain they mint is GENUINE. A single-link chain naming a
/// thief-controlled RecoveryKey is self-consistent by construction
/// (`recovery.rs` accepts a first link with no prior and no seq constraint),
/// and a statement signed by that key verifies against that chain. The push
/// must nonetheless change nothing: the peer's trust anchor is what *it* knows
/// about alice's home — never the bytes a pusher delivered. If this pin is
/// red, a thief forges a chain, permanently preempts the owner's real
/// succession on every peer (first-succession-wins), and keeps the victim's
/// trust-flagged residue — `identity-succession.md:28`'s "cannot forge" broken.
#[tokio::test]
async fn a_push_carrying_a_chain_minted_with_a_stolen_seed_changes_nothing() {
    let (home_url, home_router, home) = start_nest().await;
    let (peer_url, _peer_router, peer) = start_nest().await;
    let (_thief_url, _thief_router, thief_nest) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (_bob_seed, bob) = identity(0x44);
    // Alice registered her REAL RecoveryKey on her honest home nest; the peer
    // holds ordinary residue about her, anchored at that home.
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    // The thief: holds alice's seed, mints their own RecoveryKey and successor,
    // and builds a structurally perfect single-link chain + statement.
    let stolen_key = RecoveryKey::from_bytes([0x99; 32]);
    let (thief_successor_seed, thief_successor) = identity(0x77);
    let req = FedSuccessionPushRequest {
        statement: succession_bytes(&stolen_key, alice, &thief_successor_seed, 6),
    };

    let conn = fauna_nest::federation_channel::dial(
        &thief_nest,
        &peer_url,
        &hex::encode(peer.nest_identity.public_key_bytes()),
    )
    .await
    .expect("federation channel establishes");
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.succession.push",
            [7u8; 16],
            to_value(&req),
            None,
        )
        .await
        .expect("request rides the channel");
    // The reply shape is not what this pin is about — refusal-as-error and
    // recorded=false are both acceptable; a recorded=true or any recorded state
    // is not.
    if let Ok(reply) = call.await_reply().await {
        let reply: FedSuccessionPushReply = from_value(&reply);
        assert!(
            !reply.recorded,
            "a chain minted with a stolen seed must never be believed"
        );
    }

    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "the thief's forged succession must not be recorded"
    );
    assert_eq!(
        foreign_members(&peer).await,
        vec![alice],
        "the victim's channel membership must not be re-pointed to the thief"
    );
    assert_eq!(
        contact_peer_ids(&peer, bob).await,
        vec![alice.to_vec()],
        "the victim's trust-flagged contact edge must not follow the thief"
    );
    assert_eq!(thief_successor.len(), 32);
}

/// **The seed thief, second shape.** Instead of a from-scratch chain, the thief
/// appends a fabricated *seed-alone* link to alice's GENUINE chain. On the home
/// nest that arm is home-nest-attested (it lands only after the 30-day
/// loudly-notified window); delivered by a pusher it is attested by nothing —
/// the thief signs both mandatory signatures with the stolen seed and their own
/// key, no `prior_recovery_sig` needed. Same required outcome: nothing changes.
#[tokio::test]
async fn a_forged_seed_alone_link_on_the_genuine_chain_changes_nothing() {
    let (home_url, home_router, home) = start_nest().await;
    let (peer_url, _peer_router, peer) = start_nest().await;
    let (_thief_url, _thief_router, thief_nest) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    let stolen_key = RecoveryKey::from_bytes([0x99; 32]);
    let (thief_successor_seed, _thief_successor) = identity(0x77);
    // The statement names the thief's key. (Before the push dropped its
    // `chain`, this test also shipped the genuine first link plus a forged
    // seed-alone link; the receiver never read the chain, and now the wire
    // cannot carry one.)
    let req = FedSuccessionPushRequest {
        statement: succession_bytes(&stolen_key, alice, &thief_successor_seed, 3),
    };

    let conn = fauna_nest::federation_channel::dial(
        &thief_nest,
        &peer_url,
        &hex::encode(peer.nest_identity.public_key_bytes()),
    )
    .await
    .expect("federation channel establishes");
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.succession.push",
            [6u8; 16],
            to_value(&req),
            None,
        )
        .await
        .expect("request rides the channel");
    if let Ok(reply) = call.await_reply().await {
        let reply: FedSuccessionPushReply = from_value(&reply);
        assert!(
            !reply.recorded,
            "a pushed seed-alone link is attested by nothing and must not be believed"
        );
    }

    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "the forged seed-alone succession must not be recorded"
    );
    assert_eq!(foreign_members(&peer).await, vec![alice]);
    assert_eq!(contact_peer_ids(&peer, bob).await, vec![alice.to_vec()]);
}

/// A push naming an identity the receiver *homes* is a no-op, not a write. Only
/// `succession.submit` may supersede a local account, because only it re-points
/// the account too — recording the link alone would strand the user behind a
/// `superseded` refusal naming a successor with no account here.
#[tokio::test]
async fn a_push_naming_a_locally_homed_identity_does_not_supersede_it() {
    let (_home_url, _home_router, home) = start_nest().await;
    let (peer_url, peer_router, peer) = start_nest().await;

    let (alice_seed, alice) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, _successor) = identity(0x33);
    // Alice is homed on the *receiver* of the push — the case the guard covers.
    account_with_recovery_key(&peer_router, &peer, &alice_seed, alice, &recovery, "alice").await;

    let req = FedSuccessionPushRequest {
        statement: succession_bytes(&recovery, alice, &successor_seed, 2),
    };
    let conn = fauna_nest::federation_channel::dial(
        &home,
        &peer_url,
        &hex::encode(peer.nest_identity.public_key_bytes()),
    )
    .await
    .expect("federation channel establishes");
    let call = conn
        .dispatcher
        .request_raw(
            "fauna.federation.succession.push",
            [8u8; 16],
            to_value(&req),
            None,
        )
        .await
        .unwrap();
    let reply: FedSuccessionPushReply = from_value(&call.await_reply().await.unwrap());

    assert!(!reply.recorded, "a home nest learns nothing from a relay");
    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "the local account must not be superseded by a relayed statement"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The pull leg
// ═════════════════════════════════════════════════════════════════════════════

/// A peer that receives **no** push reaches the same state by asking. Same
/// residue, same verification, same end state — which is the doc's promise, and
/// the reason an unreachable peer is a delay rather than a permanent hole.
#[tokio::test]
async fn a_peer_that_never_received_a_push_reaches_the_same_state_by_pulling() {
    let (home_url, home_router, home) = start_nest().await;
    let (peer_url, peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);

    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &recovery, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    // The home nest has NO residue naming the peer, so the fan-out has nothing
    // to push to — the exact "peer push could not reach" case.
    assert!(
        home.db
            .succession_push_targets(&alice)
            .await
            .unwrap()
            .is_empty(),
        "precondition: this succession is not pushed anywhere"
    );
    let _: SuccessionSubmitReply = common::call(
        &home_router,
        &home,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(succession_bytes(&recovery, alice, &successor_seed, 2)),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands on the home nest");
    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "precondition: the peer is still unaware"
    );

    let learned = fauna_nest::succession_pull::pull_successions_once(&peer)
        .await
        .expect("pull pass runs");
    assert_eq!(learned, 1);

    assert_eq!(
        peer.db
            .succession_for(&alice[..])
            .await
            .unwrap()
            .unwrap()
            .new_actor_id,
        successor.to_vec()
    );
    assert_eq!(foreign_members(&peer).await, vec![successor]);
    assert_eq!(contact_peer_ids(&peer, bob).await, vec![successor.to_vec()]);

    let relay = fauna_nest::federation_channel::dial(
        &home,
        &peer_url,
        &hex::encode(peer.nest_identity.public_key_bytes()),
    )
    .await
    .expect("federation channel establishes");
    let err = relay_inbox(
        &relay,
        3,
        signed_inbox_payload(&alice_seed, alice, bob),
        bob,
    )
    .await
    .expect_err("the pulled link enforces exactly like a pushed one");
    assert!(format!("{err:?}").contains("superseded"));
    let _ = peer_router;

    // A second pass is a no-op: the work list skips what is already known.
    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .unwrap(),
        0
    );
}

/// The pull is a fetch of self-verifying bytes, not a question whose answer is
/// believed. A home nest that serves a statement its own registration chain does
/// not authorize teaches the puller nothing.
#[tokio::test]
async fn a_pull_from_a_lying_home_nest_records_nothing() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let forged = RecoveryKey::from_bytes([0x99; 32]);
    let (successor_seed, _successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);

    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    // The home nest writes a link its own chain does not authorize — modelling a
    // compromised or malicious home nest, which is the one party a peer would
    // otherwise be tempted to trust.
    home.db
        .record_succession(
            &alice[..],
            &successor_seed.verifying_key().to_bytes()[..],
            &succession_bytes(&forged, alice, &successor_seed, 2),
            2,
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        0,
        "a statement the served chain does not authorize teaches the puller nothing"
    );
    assert!(peer.db.succession_for(&alice[..]).await.unwrap().is_none());
    assert_eq!(foreign_members(&peer).await, vec![alice]);
}

// ── a planted address must not deny succession ─────────────────────
//
// The anchor IDENTITY is pinned (oldest row, then the write-once
// `foreign_recovery_heads` pin) and every dial proves it, so a planted binding
// cannot cause a *takeover* — the pin above proves that. What it could do
// until the fix was cause a permanent *denial*: URL resolution
// returned exactly one row, newest-wins and nest-wide across every actor, so
// one planted binding naming the honest identity at a box that cannot sign as
// it ended the actor's pass — every pass, forever. That falsifies
// `identity-succession.md`'s ratified "delay rather than a permanent hole".
//
// Both pins below plant a row for an **unrelated** actor (planting for the
// actor under test is weaker: it can pass for the wrong reason, because that
// actor's own anchor identity would move). They differ only in WHERE the
// planted row sits in the candidate order, and each bites a different half of
// the fix: the first is red without a fallback of any kind, the second is red
// unless the walk genuinely tries every candidate.
//
// The "attacker box" is a real third nest rather than a dead port, so the
// refusal is a fast, deterministic identity-proof failure with no connect
// timeout anywhere in the pin (testing.md § point 14).

/// **Newest planted row.** The exact differential the review ran: byte-identical
/// to the pull pin above plus one extra residue row, about a different actor,
/// naming the honest home's identity at a box that cannot sign as it. The
/// succession must still be learned.
#[tokio::test]
async fn a_planted_newest_address_does_not_deny_the_pull() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let (decoy_url, _decoy_router, _decoy) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);
    let (_carol_seed, carol) = identity(0x55);

    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &recovery, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    // The plant: an unrelated actor, on an unrelated channel, claiming the
    // honest home identity lives at the decoy's address. Any User-class caller
    // can mint this through `welcome_deliver_core`, and it lands NEWEST.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL3,
            &carol,
            &home_nest_id,
            Some(&decoy_url),
            RebindPower::Standing,
        )
        .await
        .expect("planted binding");

    let _: SuccessionSubmitReply = common::call(
        &home_router,
        &home,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(succession_bytes(&recovery, alice, &successor_seed, 2)),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands on the home nest");

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        1,
        "a planted address for an unrelated actor must not deny alice's \
         succession — availability, not just authenticity"
    );
    assert_eq!(
        peer.db
            .succession_for(&alice[..])
            .await
            .unwrap()
            .unwrap()
            .new_actor_id,
        successor.to_vec()
    );
}

/// **Oldest planted row.** Same attack, but the planted address is registered
/// *before* the honest one, so it sorts first in the candidate order. Ordering
/// alone cannot save this: only actually walking past a failed candidate does.
#[tokio::test]
async fn a_planted_oldest_address_does_not_deny_the_pull() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let (decoy_url, _decoy_router, _decoy) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);
    let (_carol_seed, carol) = identity(0x55);

    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &recovery, "alice").await;
    // The plant goes in FIRST — the honest binding is the later sighting of
    // this identity's address.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL3,
            &carol,
            &home_nest_id,
            Some(&decoy_url),
            RebindPower::Standing,
        )
        .await
        .expect("planted binding");
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    let _: SuccessionSubmitReply = common::call(
        &home_router,
        &home,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(succession_bytes(&recovery, alice, &successor_seed, 2)),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands on the home nest");

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        1,
        "the walk must try the next candidate after a failed proof — a planted \
         row that sorts FIRST is what an ordering-only fix leaves open"
    );
    assert_eq!(
        peer.db
            .succession_for(&alice[..])
            .await
            .unwrap()
            .unwrap()
            .new_actor_id,
        successor.to_vec()
    );
}

/// **The seed thief, pull shape.** The thief stands up their own nest, plants a
/// self-consistent forged chain + succession for alice there, and gets a NEWER
/// residue row added on the peer binding alice to that nest (with the stolen
/// seed they can authenticate as alice from anywhere, so new cross-nest edges
/// naming a thief-chosen home are reachable). The pull must anchor on the
/// binding the peer learned FIRST — the honest home — and never dial the
/// later-added one for this identity: first-contact TOFU is the declared grade,
/// and a later binding must not be able to displace it.
#[tokio::test]
async fn a_later_added_binding_does_not_divert_the_pull_anchor() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let (thief_url, _thief_router, thief_nest) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();
    let thief_nest_id = thief_nest.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    // Oldest binding: the honest home (inserted first — the anchor).
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;
    // Later binding: a second channel's row pointing alice at the thief's nest.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL2,
            &alice,
            &thief_nest_id,
            Some(&thief_url),
            RebindPower::Standing,
        )
        .await
        .expect("later-added binding");

    // The thief's nest serves a fully self-consistent forgery: a local `users`
    // row for alice, a from-scratch chain under the thief's key, and a recorded
    // succession to the thief's successor. Every byte it serves verifies.
    let stolen_key = RecoveryKey::from_bytes([0x99; 32]);
    let (thief_successor_seed, _thief_successor) = identity(0x77);
    thief_nest
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("thief plants alice locally");
    let forged_link = registration_bytes(&alice_seed, alice, &stolen_key, 5);
    thief_nest
        .db
        .append_recovery_registration(&alice[..], 5, &stolen_key.public()[..], &forged_link)
        .await
        .expect("thief plants a forged chain");
    thief_nest
        .db
        .record_succession(
            &alice[..],
            &thief_successor_seed.verifying_key().to_bytes()[..],
            &succession_bytes(&stolen_key, alice, &thief_successor_seed, 6),
            6,
        )
        .await
        .unwrap()
        .unwrap();

    // One pull pass. The honest home has no succession to serve; the thief's
    // nest is never the anchor for alice — so nothing may be learned.
    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        0,
        "a later-added binding must not become the pull anchor"
    );
    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "the thief's forged succession must not be recorded via the poisoned binding"
    );
    assert_eq!(foreign_members(&peer).await, vec![alice]);
    assert_eq!(contact_peer_ids(&peer, bob).await, vec![alice.to_vec()]);
}

/// A quiet pull — no succession anywhere — persists the identity's chain head,
/// so a later chain must *extend* what this nest saw today instead of getting
/// first-contact TOFU. This is what shrinks the thief's window to the time
/// before the peer's first hourly pass ever ran.
#[tokio::test]
async fn a_quiet_pull_learns_the_chain_head_for_later() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        0,
        "no succession exists — nothing to learn on that axis"
    );
    let head = peer
        .db
        .foreign_recovery_head(&alice[..])
        .await
        .unwrap()
        .expect("the quiet pass must persist the verified chain head");
    assert_eq!(head.recovery_pubkey, real.public());
    assert_eq!(head.seq, 1);
}

/// Once a head is persisted, even the **anchor itself turning hostile** cannot
/// re-mint the chain: a served chain that never visits the known head is
/// refused. Modeled as a home nest whose store was rebuilt around a thief
/// chain after the peer had already learned the real head.
#[tokio::test]
async fn a_chain_that_rewrites_a_known_head_is_refused_even_from_the_anchor() {
    let (home_url, _home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let stolen_key = RecoveryKey::from_bytes([0x99; 32]);
    let (thief_successor_seed, _thief_successor) = identity(0x77);
    let (_bob_seed, bob) = identity(0x44);

    // The peer learned alice's real head — and pinned the honest anchor
    // identity — while the home was honest.
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;
    peer.db
        .record_foreign_recovery_head(&alice[..], &real.public(), 1, &home_nest_id)
        .await
        .unwrap();

    // The home nest now serves a from-scratch thief chain + succession — every
    // signature genuine (the seed is stolen), nothing visiting the known head.
    home.db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .unwrap();
    let forged_link = registration_bytes(&alice_seed, alice, &stolen_key, 5);
    home.db
        .append_recovery_registration(&alice[..], 5, &stolen_key.public()[..], &forged_link)
        .await
        .unwrap();
    home.db
        .record_succession(
            &alice[..],
            &thief_successor_seed.verifying_key().to_bytes()[..],
            &succession_bytes(&stolen_key, alice, &thief_successor_seed, 6),
            6,
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        0,
        "a chain that rewrites the known head must be refused even from the anchor"
    );
    assert!(peer.db.succession_for(&alice[..]).await.unwrap().is_none());
    assert_eq!(foreign_members(&peer).await, vec![alice]);
}

// ═════════════════════════════════════════════════════════════════════════════
// The anchor-plantability hardening
//
// The pull leg above anchors on the oldest *addressable* residue row and never
// proves the anchor's nest identity, so a URL-less (NULL `nest_url`) history (what a
// room accept with an empty invitee URL writes) or a not-yet-bound identity lets an attacker's binding *become*
// the anchor, and a later re-invite rewriting the oldest row's home hands the
// anchor to a nest the peer never chose. These pins nail the closed shape:
// the anchor identity is the oldest row OVERALL, resolved to a URL only through
// a binding sharing that identity, and every dial must PROVE that identity.
// ═════════════════════════════════════════════════════════════════════════════

/// Plant a self-consistent forgery for `alice` on the thief's own nest: a local
/// account, a from-scratch chain under `stolen`, and a recorded succession to
/// `successor`. Every byte it serves verifies — the point being that verifying
/// bytes is not enough; only the anchor's *identity* separates it from the home.
async fn plant_thief_forgery(
    thief_nest: &Arc<AppState>,
    alice_seed: &SigningKey,
    alice: [u8; 32],
    stolen: &RecoveryKey,
    successor_seed: &SigningKey,
) {
    thief_nest
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("thief plants alice locally");
    let forged_link = registration_bytes(alice_seed, alice, stolen, 5);
    thief_nest
        .db
        .append_recovery_registration(&alice[..], 5, &stolen.public()[..], &forged_link)
        .await
        .expect("thief plants a forged chain");
    thief_nest
        .db
        .record_succession(
            &alice[..],
            &successor_seed.verifying_key().to_bytes()[..],
            &succession_bytes(stolen, alice, successor_seed, 6),
            6,
        )
        .await
        .unwrap()
        .unwrap();
}

/// Plant the review § 1.1 B/C shape on the thief's nest: alice's GENUINE first
/// link (seq 1, real key) EXTENDED by a seed-alone link at seq 2 under the
/// stolen key, plus a succession at seq 3 to the thief's successor. Unlike
/// [`plant_thief_forgery`]'s from-scratch chain, this chain **visits** a
/// persisted `known` head (the real seq-1) and then extends past it — the shape
/// the head alone does not reject, so only the anchor identity stands between it
/// and a takeover.
async fn plant_thief_extension(
    thief_nest: &Arc<AppState>,
    alice_seed: &SigningKey,
    alice: [u8; 32],
    real: &RecoveryKey,
    stolen: &RecoveryKey,
    successor_seed: &SigningKey,
) {
    thief_nest
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("thief plants alice locally");
    thief_nest
        .db
        .append_recovery_registration(
            &alice[..],
            1,
            &real.public()[..],
            &registration_bytes(alice_seed, alice, real, 1),
        )
        .await
        .expect("genuine first link");
    thief_nest
        .db
        .append_recovery_registration(
            &alice[..],
            2,
            &stolen.public()[..],
            &registration_bytes(alice_seed, alice, stolen, 2),
        )
        .await
        .expect("seed-alone extension link");
    thief_nest
        .db
        .record_succession(
            &alice[..],
            &successor_seed.verifying_key().to_bytes()[..],
            &succession_bytes(stolen, alice, successor_seed, 3),
            3,
        )
        .await
        .unwrap()
        .unwrap();
}

/// **Pin 1 (RED before the fix).** Alice's honest history on this peer is a
/// URL-less binding (NULL `nest_url`, as a room accept with an empty invitee
/// URL writes). An attacker then gets ONE addressable binding recorded naming alice
/// at a nest they control. The pull must anchor on the oldest row OVERALL (the
/// honest URL-less row's `home_nest_id`) and, finding no addressable binding that
/// shares that identity, **refuse** — never dial the attacker's binding just
/// because it is the only one with a URL.
#[tokio::test]
async fn a_url_less_binding_is_not_displaced_by_an_attacker_binding() {
    let (_home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let (thief_url, _thief_router, thief_nest) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();
    let thief_nest_id = thief_nest.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;

    // Alice's honest home, recorded WITHOUT a URL (a NULL `nest_url`).
    peer.db
        .create_user_with_handle(&bob, "free", "bob", None)
        .await
        .expect("local account");
    peer.db
        .register_foreign_channel_member(
            &CHANNEL,
            &alice,
            &home_nest_id,
            None,
            RebindPower::Standing,
        )
        .await
        .expect("url-less honest binding");
    peer.db.accept_contact(&bob, &alice).await.expect("contact");
    // The attacker's later addressable binding on a different channel.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL2,
            &alice,
            &thief_nest_id,
            Some(&thief_url),
            RebindPower::Standing,
        )
        .await
        .expect("attacker binding");
    plant_thief_forgery(
        &thief_nest,
        &alice_seed,
        alice,
        &RecoveryKey::from_bytes([0x99; 32]),
        &{
            let (s, _) = identity(0x77);
            s
        },
    )
    .await;

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        0,
        "an attacker binding must not become the anchor for a URL-less identity"
    );
    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "the thief's forged succession must not be recorded via the planted anchor"
    );
    assert_eq!(foreign_members(&peer).await, vec![alice]);
    assert_eq!(contact_peer_ids(&peer, bob).await, vec![alice.to_vec()]);
}

/// **Pin 2 (regression guard).** The fix must NOT break availability for a
/// URL-less oldest row whose *own* home is still reachable through another,
/// addressable binding that shares its `home_nest_id`. The honest home genuinely
/// superseded alice; the peer must learn it by resolving the URL from the
/// sibling binding rather than refusing.
#[tokio::test]
async fn a_url_less_oldest_row_resolves_its_url_from_a_sibling_binding() {
    let (home_url, home_router, home) = start_nest().await;
    let (peer_url, peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;

    peer.db
        .create_user_with_handle(&bob, "free", "bob", None)
        .await
        .expect("local account");
    // Oldest row: URL-less, honest home identity.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL,
            &alice,
            &home_nest_id,
            None,
            RebindPower::Standing,
        )
        .await
        .expect("url-less honest binding");
    peer.db.accept_contact(&bob, &alice).await.expect("contact");
    // A later addressable binding for the SAME honest home — the URL source.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL2,
            &alice,
            &home_nest_id,
            Some(&home_url),
            RebindPower::Standing,
        )
        .await
        .expect("addressable sibling binding");

    // The honest home supersedes alice.
    let _: SuccessionSubmitReply = common::call(
        &home_router,
        &home,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(succession_bytes(&real, alice, &successor_seed, 2)),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands on the home nest");

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        1,
        "the URL-less oldest row must resolve its URL from the sibling binding and learn"
    );
    assert_eq!(
        peer.db
            .succession_for(&alice[..])
            .await
            .unwrap()
            .unwrap()
            .new_actor_id,
        successor.to_vec()
    );
    let _ = (peer_url, peer_router);
}

/// **Pin 3 (RED before the fix) — the reachable takeover, and the possession
/// proof that closes it.** The peer verifies alice once while her home is honest
/// (pinning the anchor identity). A re-invite then REWRITES alice's oldest
/// binding row to the attacker's URL *while claiming the honest home's nest id*
/// (the lying-`nest.info` / poisoned-URL shape:
/// `register_foreign_channel_member` upserts both `home_nest_id` and `nest_url`
/// on the PK, and `resolve_peer_nest_id` believes whatever the attacker's box
/// reports). URL resolution therefore points the dial at the thief — so only a
/// **signed possession challenge** separates them: the thief's box cannot sign
/// as the honest home the peer pinned, so the dial is refused and the takeover
/// is not recorded.
///
/// This is review § 1.2's reachable core. (Review § 1.1 — that the known head
/// does not reject a seed-alone *extension* — is a true unit fact but is NOT a
/// separate fix: the seed-alone arm is the legitimate honest-loss path, so it is
/// only exploitable through a hostile anchor, which this pin closes.)
#[tokio::test]
async fn a_rewritten_anchor_row_cannot_move_a_pinned_identity() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let (thief_url, _thief_router, thief_nest) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    // First contact while the home is honest: pins alice's anchor identity.
    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("first pull runs"),
        0,
        "no succession yet — the pass only pins the anchor"
    );

    // A re-invite rewrites alice's oldest binding to the thief's URL while still
    // CLAIMING the honest home's nest id (a box that lies in its `nest.info`).
    // URL resolution now points at the thief, so the possession proof is the
    // only thing standing between the pin and the takeover.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL,
            &alice,
            &home_nest_id,
            Some(&thief_url),
            RebindPower::Standing,
        )
        .await
        .expect("binding rewritten to the thief's URL under the honest id");
    // The thief serves the review § 1.1 B/C chain — genuine seq-1 extended by a
    // seed-alone link — so the persisted known head (seq-1, real) is VISITED and
    // does not by itself reject the forgery. Only the anchor identity can.
    let (thief_successor_seed, _) = identity(0x77);
    plant_thief_extension(
        &thief_nest,
        &alice_seed,
        alice,
        &real,
        &RecoveryKey::from_bytes([0x99; 32]),
        &thief_successor_seed,
    )
    .await;

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("second pull runs"),
        0,
        "a rewritten binding must not move the pinned anchor identity"
    );
    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "the takeover via the rewritten anchor must not be recorded"
    );
    assert_eq!(foreign_members(&peer).await, vec![alice]);
    assert_eq!(contact_peer_ids(&peer, bob).await, vec![alice.to_vec()]);
}

/// **Pin 5 (RED before the fix) — the write-once anchor pin.** The twin of pin
/// 3: the re-invite rewrites alice's oldest binding to the thief's *own*
/// identity (not merely the thief's URL under the honest id). Without the
/// persisted pin, the oldest-row identity is now the thief and the pull would
/// trust the box that proves it; the write-once `anchor_nest_id` pin, learned at
/// first contact, overrides the rewritten row so the expected identity stays the
/// honest home — for which no addressable binding remains, so the pull refuses.
#[tokio::test]
async fn a_rewrite_to_a_new_identity_cannot_move_the_pinned_anchor() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let (thief_url, _thief_router, thief_nest) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();
    let thief_nest_id = thief_nest.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    // First contact pins the honest anchor identity.
    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("first pull runs"),
        0
    );

    // The re-invite rewrites the oldest binding to the THIEF's own identity —
    // so the oldest-row identity is now the thief. Only the write-once pin keeps
    // the expected identity honest.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL,
            &alice,
            &thief_nest_id,
            Some(&thief_url),
            RebindPower::Standing,
        )
        .await
        .expect("binding rewritten to the thief's own identity");
    let (thief_successor_seed, _) = identity(0x77);
    plant_thief_extension(
        &thief_nest,
        &alice_seed,
        alice,
        &real,
        &RecoveryKey::from_bytes([0x99; 32]),
        &thief_successor_seed,
    )
    .await;

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("second pull runs"),
        0,
        "the write-once pin must keep the expected identity honest despite the rewrite"
    );
    assert!(
        peer.db.succession_for(&alice[..]).await.unwrap().is_none(),
        "the takeover via an identity rewrite must not be recorded"
    );
    assert_eq!(foreign_members(&peer).await, vec![alice]);
    assert_eq!(contact_peer_ids(&peer, bob).await, vec![alice.to_vec()]);
}

/// **Pin 4 (characterization).** An identity with no prior residue whose FIRST
/// binding names the honest home is TOFU-anchored and learns — the grade
/// `identity-succession.md:63` grants a consumer that never saw a registration.
/// The fix refuses *unresolvable* and *pin-violating* anchors, never a genuine
/// first contact.
#[tokio::test]
async fn a_first_binding_is_the_tofu_anchor_and_learns() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    // Alice's very first binding on this peer, addressable, honest.
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    let _: SuccessionSubmitReply = common::call(
        &home_router,
        &home,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(succession_bytes(&real, alice, &successor_seed, 2)),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands on the home nest");

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        1,
        "a genuine first-contact binding must TOFU-anchor and learn"
    );
    assert_eq!(
        peer.db
            .succession_for(&alice[..])
            .await
            .unwrap()
            .unwrap()
            .new_actor_id,
        successor.to_vec()
    );
}

/// **the plant that ERASES rather than joins.**
///
/// gave both propagation legs a candidate *walk*, on the stated premise
/// that *"an attacker can only add rows later"*. That is true of an INSERT, and
/// every pin plants through a fresh channel id — which is why they all
/// stayed green while this hole was open. The membership write is an **upsert**
/// on `(channel_id, actor_id)`: a second call for the SAME pair rewrites the row
/// in place, so a plant inherited the honest row's sighting *and* deleted the
/// honest address. For an identity known through a single cross-nest row — the
/// ordinary 1:1 DM or folder share, exactly what [`seed_peer_residue`] builds
/// — the candidate list afterwards held nothing honest, the walk had nothing to
/// fail over to, and the permanent silent denial was restored with the cap and
/// the ordering both irrelevant.
///
/// The conflict path IS the finding, so this plants on the established
/// `(CHANNEL, alice)` pair. The anchor is pinned by a first quiet pull before
/// the plant, so what is under test is purely **availability**: the identity is
/// settled, and the only question is whether an address for it survives.
///
/// The third nest is real and honest about its own id — no lying `nest.info`
/// needed. That is the sharper shape: the rewrite moves the row to the thief's
/// identity, so the honest identity is left with *no addressable row at all*.
#[tokio::test]
async fn a_rewritten_binding_does_not_erase_the_honest_address() {
    let (home_url, home_router, home) = start_nest().await;
    let (_peer_url, _peer_router, peer) = start_nest().await;
    let (thief_url, _thief_router, thief) = start_nest().await;
    let home_nest_id = home.nest_identity.public_key_bytes();
    let thief_nest_id = thief.nest_identity.public_key_bytes();

    let (alice_seed, alice) = identity(0x11);
    let real = RecoveryKey::from_bytes([0x22; 32]);
    let (successor_seed, successor) = identity(0x33);
    let (_bob_seed, bob) = identity(0x44);
    account_with_recovery_key(&home_router, &home, &alice_seed, alice, &real, "alice").await;
    // Alice is known to this peer through exactly ONE cross-nest row.
    seed_peer_residue(&peer, home_nest_id, &home_url, alice, bob).await;

    // A first quiet pull pins alice's anchor identity, so the plant below cannot
    // be dismissed as an identity question — it is purely about the address.
    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("first pull runs"),
        0,
        "no succession yet — the pass only pins the anchor"
    );

    // THE PLANT, through the conflict path: the same `(CHANNEL, alice)` pair,
    // re-pointed at the thief. One User-class `welcome.deliver` reaches this.
    peer.db
        .register_foreign_channel_member(
            &CHANNEL,
            &alice,
            &thief_nest_id,
            Some(&thief_url),
            RebindPower::Standing,
        )
        .await
        .expect("binding rewritten to the thief");

    // The honest address must have JOINED the directory, not been replaced by
    // it. Before the fix this list was empty: the sole row now named the thief's
    // identity, so the pinned anchor had no address left anywhere.
    let candidates = peer
        .db
        .resolve_foreign_nest_urls(&home_nest_id, 8)
        .await
        .expect("resolve candidates");
    assert!(
        candidates.contains(&home_url.trim_end_matches('/').to_string()),
        "the rewrite ERASED the honest address — candidates {candidates:?} for the pinned \
         anchor, so the walk has nothing to dial and the succession is denied forever"
    );

    let _: SuccessionSubmitReply = common::call(
        &home_router,
        &home,
        [0u8; 32],
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(succession_bytes(&real, alice, &successor_seed, 2)),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands on the home nest");

    assert_eq!(
        fauna_nest::succession_pull::pull_successions_once(&peer)
            .await
            .expect("pull pass runs"),
        1,
        "a rewritten binding must not deny alice's succession — the honest address is \
         still a candidate and the walk reaches it"
    );
    assert_eq!(
        peer.db
            .succession_for(&alice[..])
            .await
            .unwrap()
            .unwrap()
            .new_actor_id,
        successor.to_vec()
    );
}

/// The same erasure, on the **push** leg — which had no walk at all: it read the
/// membership row's own address column, so the address written *last* was the
/// only target. Re-pointing a binding at a second address for the same peer
/// identity therefore *replaced* the honest target instead of adding to it.
///
/// **Scope note, established by execution while writing this pin.** An earlier
/// version planted a *different nest identity* on the row and asserted the
/// honest peer was still a target. It failed — and correctly so: the directory
/// preserves addresses per identity, so a row re-pointed to another identity
/// takes its targets with it. That is **not** a hole this fix should close, and
/// the goal doc already says why: a push is a wake-up hint, and the honest
/// peer's own hourly pull reads *its own* residue and *its own* pinned anchor
/// (`pull_successions_once` → `distinct_foreign_member_actors`), so it learns
/// the succession regardless of whom the home nest pushed to. The cost of an
/// identity rewrite is bounded by `PULL_INTERVAL`, which is the declared
/// "delay, not a permanent hole" contract. Filed to the security review as an
/// adjacent observation rather than silently widened into this pin.
#[tokio::test]
async fn a_rewritten_address_does_not_redirect_the_whole_push() {
    let (peer_url, _peer_router, peer) = start_nest().await;
    let (second_url, _second_router, _second) = start_nest().await;
    let (_alice_seed, alice) = identity(0x11);
    let peer_nest_id = peer.nest_identity.public_key_bytes();

    let (_home_url, _home_router, home) = start_nest().await;
    seed_home_residue(&home, alice, peer_nest_id, &peer_url).await;

    // THE PLANT, through the conflict path: the same `(CHANNEL, 0xBB)` pair
    // `seed_home_residue` created, re-pointed at a second address while still
    // naming the same peer identity — the lying-`nest.info` shape, and the one
    // any User-class caller can mint through `welcome_deliver_core`.
    home.db
        .register_foreign_channel_member(
            &CHANNEL,
            &[0xBB; 32],
            &peer_nest_id,
            Some(&second_url),
            RebindPower::Standing,
        )
        .await
        .expect("binding re-pointed to a second address");

    let targets = home
        .db
        .succession_push_targets(&alice)
        .await
        .expect("push targets");
    assert!(
        targets.contains(&peer_url.trim_end_matches('/').to_string()),
        "the rewrite redirected the push away from the honest address; targets {targets:?}"
    );
    assert!(
        targets.contains(&second_url.trim_end_matches('/').to_string()),
        "the second address should JOIN the target set, not replace it; targets {targets:?}"
    );
}

/// **the pool half — the mint that made the plant free.**
///
/// `get_or_dial` was keyed on `peer_nest_id` **alone**, so once an identity was
/// pooled the URL became irrelevant: a box that merely *claimed* that id in its
/// self-declared `nest.info` was never dialed, proved nothing, and still got its
/// address recorded as that identity's. An attacker arranges the pooling
/// themselves with one prior honest-URL call, so the whole mint costs them
/// nothing. With the URL in the key, an address this nest has not dialed is
/// always dialed — and the handshake, not the claim, decides.
///
/// Proven by reaching one real in-process nest under **two** loopback hosts:
/// `start_nest` serves `http://127.0.0.1:PORT`, and `localhost` is an accepted
/// peer-URL host (`validate_peer_url`) resolving to the same listener. The only
/// writer of `nest_addresses` here is `get_or_dial`'s own proof stamp — nothing
/// in this test registers a membership row — so the second address appearing at
/// all *is* the evidence that it was dialed rather than answered from the pool.
#[tokio::test]
async fn a_second_address_for_a_pooled_identity_is_still_dialed() {
    let (peer_url, _peer_router, peer) = start_nest().await;
    let (_home_url, _home_router, home) = start_nest().await;
    let peer_nest_id = peer.nest_identity.public_key_bytes();

    let alt_url = peer_url.replace("127.0.0.1", "localhost");
    assert_ne!(alt_url, peer_url, "the two addresses must differ");

    let req = FedSuccessionPushRequest {
        statement: Vec::new(),
    };
    // Pool a channel to the peer at its first address. The far end refuses the
    // empty statement — irrelevant here: the DIAL is what is under test, and it
    // completes before any handler runs.
    let _ = fauna_nest::federation_pool::originate_succession_push(
        &home.federation_pool,
        &home,
        &peer_url,
        &req,
    )
    .await;
    // Now reach the SAME identity at a second address.
    let _ = fauna_nest::federation_pool::originate_succession_push(
        &home.federation_pool,
        &home,
        &alt_url,
        &req,
    )
    .await;

    let addrs = home
        .db
        .resolve_foreign_nest_urls(&peer_nest_id, 8)
        .await
        .expect("resolve candidates");
    assert!(
        addrs.contains(&peer_url.trim_end_matches('/').to_string()),
        "the first dial must record its proven address; got {addrs:?}"
    );
    assert!(
        addrs.contains(&alt_url.trim_end_matches('/').to_string()),
        "the second address was answered from the pool WITHOUT a dial, so nothing proved the \
         identity holds it — the mint; got {addrs:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Small helpers that need the channel value shapes
// ═════════════════════════════════════════════════════════════════════════════

fn to_value<T: Serialize>(t: &T) -> fauna_protocol::Value {
    decode::<fauna_protocol::Value>(&encode_canonical(t).unwrap()).unwrap()
}

fn from_value<T: DeserializeOwned>(v: &fauna_protocol::Value) -> T {
    decode::<T>(&encode_canonical(v).unwrap()).unwrap()
}

// ═════════════════════════════════════════════════════════════════════════════
// The collapse is a rebind, and a succession holds no rebind power
//
// ═════════════════════════════════════════════════════════════════════════════

/// Seed one foreign-member binding and optionally pin it, the way the live
/// paths do: `register_foreign_channel_member` writes the grant, and
/// `confirm_foreign_member` — which only `require_foreign_member`'s first
/// successful serve fires — stamps it.
async fn bind_foreign_member(
    state: &Arc<AppState>,
    channel: &[u8; 32],
    actor: &[u8; 32],
    home: &[u8; 32],
    power: RebindPower,
    pin: bool,
) {
    state
        .db
        .register_foreign_channel_member(channel, actor, home, None, power)
        .await
        .expect("register foreign member");
    if pin {
        state
            .db
            .confirm_foreign_member(channel, actor, home)
            .await
            .expect("pin the binding");
    }
}

/// **The finding, at its narrowest.** A caller with no standing
/// on the channel plants a grant for the *successor's* actor id — a fresh
/// `(channel, actor)` pair, so `RebindPower::InsertOnly` still writes it — and
/// then triggers the succession, whose collision arm resolved the primary-key
/// clash in favour of whichever row named the successor. That handed the
/// planted, unwitnessed home nest a binding the genuine home nest had already
/// earned the first-use pin on, and on an unclaimed conversation channel
/// nothing could move it back afterwards.
///
/// The rule this pins: **a succession re-points the identity and never the
/// binding.** `federation.md` § Cross-nest shared folders + channel append
/// enumerates the powers over an existing grant — the claimant, or a rostered
/// actor while it is unconfirmed — and the succession statement
/// (`identity-succession.md` § The succession statement (wire)) carries no
/// home-nest field at all, so a succession is none of them and has nothing of
/// its own to install.
#[tokio::test]
async fn a_succession_does_not_hand_a_pinned_binding_to_a_planted_successor_row() {
    let (_url, _router, state) = start_nest().await;
    let channel = [0xC1u8; 32];
    let old_actor = [0xA0u8; 32];
    let new_actor = [0xA1u8; 32];
    let genuine_home = [0x11u8; 32];
    let planted_home = [0x66u8; 32];

    // The genuine member, whose home nest has exercised the grant.
    bind_foreign_member(
        &state,
        &channel,
        &old_actor,
        &genuine_home,
        RebindPower::Standing,
        true,
    )
    .await;
    // The plant: a first grant for the successor id, written with the only
    // power a stranger holds.
    bind_foreign_member(
        &state,
        &channel,
        &new_actor,
        &planted_home,
        RebindPower::InsertOnly,
        false,
    )
    .await;

    state
        .db
        .record_peer_succession(&old_actor, &new_actor, b"statement", 1)
        .await
        .expect("apply")
        .expect("not refused");

    assert_eq!(
        state
            .db
            .foreign_member_binding(&channel, &new_actor)
            .await
            .unwrap(),
        Some((genuine_home, true)),
        "the succession moved a PINNED binding to a nest that never earned a confirmation — the \
         planted row won the collapse"
    );
    assert_eq!(
        state
            .db
            .foreign_member_home_nest(&channel, &old_actor)
            .await
            .unwrap(),
        None,
        "the superseded key must hold no live membership — that is what the propagation is for"
    );
}

/// The same plant against an **unconfirmed** binding. `federation.md`'s narrowed
/// residual accepts that an unconfirmed grant is still movable — but bounds the
/// movers to *rostered actors*, never to every caller who knows the 32-byte
/// channel id. A collapse that preferred the planted row would launder exactly
/// that power, so the pin is not what decides this: the predecessor's binding
/// survives either way.
///
/// It survives *unconfirmed*, which is what keeps a legitimate re-homing
/// available: any rostered actor re-inviting the successor moves it with
/// `RebindPower::Standing`, the documented path.
#[tokio::test]
async fn a_succession_does_not_hand_an_unconfirmed_binding_to_a_planted_successor_row() {
    let (_url, _router, state) = start_nest().await;
    let channel = [0xC2u8; 32];
    let old_actor = [0xB0u8; 32];
    let new_actor = [0xB1u8; 32];
    let genuine_home = [0x22u8; 32];
    let planted_home = [0x66u8; 32];

    bind_foreign_member(
        &state,
        &channel,
        &old_actor,
        &genuine_home,
        RebindPower::Standing,
        false,
    )
    .await;
    bind_foreign_member(
        &state,
        &channel,
        &new_actor,
        &planted_home,
        RebindPower::InsertOnly,
        false,
    )
    .await;

    state
        .db
        .record_peer_succession(&old_actor, &new_actor, b"statement", 1)
        .await
        .expect("apply")
        .expect("not refused");

    assert_eq!(
        state
            .db
            .foreign_member_binding(&channel, &new_actor)
            .await
            .unwrap(),
        Some((genuine_home, false)),
        "an unconfirmed grant is movable by a ROSTERED actor, and the collapse must not lend that \
         power to a caller who merely knows the channel id"
    );

    // …and the documented healing path still works on the survivor.
    state
        .db
        .register_foreign_channel_member(
            &channel,
            &new_actor,
            &planted_home,
            None,
            RebindPower::Standing,
        )
        .await
        .expect("a rostered re-invite");
    assert_eq!(
        state
            .db
            .foreign_member_binding(&channel, &new_actor)
            .await
            .unwrap(),
        Some((planted_home, false)),
        "the surviving row must stay movable by standing — that is what keeps a legitimate \
         re-homing after a succession available at all"
    );
}

/// The mirror the finding asked for: with **no** collision the re-point is
/// unchanged, and it carries the pin across. Nothing about the collision rule
/// may cost the ordinary path its stamp — a re-pointed row is the same
/// witnessed binding under a new name for the same person, not a new one.
#[tokio::test]
async fn a_succession_with_no_collision_re_points_the_binding_and_its_pin_intact() {
    let (_url, _router, state) = start_nest().await;
    let channel = [0xC3u8; 32];
    let old_actor = [0xD0u8; 32];
    let new_actor = [0xD1u8; 32];
    let home = [0x33u8; 32];

    bind_foreign_member(
        &state,
        &channel,
        &old_actor,
        &home,
        RebindPower::Standing,
        true,
    )
    .await;

    let applied = state
        .db
        .record_peer_succession(&old_actor, &new_actor, b"statement", 1)
        .await
        .expect("apply")
        .expect("not refused");

    assert_eq!(applied.foreign_memberships, 1, "the row moved");
    assert_eq!(
        state
            .db
            .foreign_member_binding(&channel, &new_actor)
            .await
            .unwrap(),
        Some((home, true)),
        "a plain re-point must carry the binding AND its first-use pin"
    );
}

/// A collision where both rows name the **same** home nest — the successor was
/// separately welcomed to the same channel at the same nest. Nothing is in
/// dispute, so the collapse must not spend the predecessor's witness: the
/// surviving row keeps `confirmed_at`, and the count reports the row it moved.
#[tokio::test]
async fn a_same_home_collision_keeps_the_predecessors_confirmation() {
    let (_url, _router, state) = start_nest().await;
    let channel = [0xC4u8; 32];
    let old_actor = [0xE0u8; 32];
    let new_actor = [0xE1u8; 32];
    let home = [0x44u8; 32];

    bind_foreign_member(
        &state,
        &channel,
        &old_actor,
        &home,
        RebindPower::Standing,
        true,
    )
    .await;
    bind_foreign_member(
        &state,
        &channel,
        &new_actor,
        &home,
        RebindPower::Standing,
        false,
    )
    .await;

    let applied = state
        .db
        .record_peer_succession(&old_actor, &new_actor, b"statement", 1)
        .await
        .expect("apply")
        .expect("not refused");

    assert_eq!(
        state
            .db
            .foreign_member_binding(&channel, &new_actor)
            .await
            .unwrap(),
        Some((home, true)),
        "an undisputed collapse must not reset a pin the bound nest already earned"
    );
    assert_eq!(
        applied.foreign_memberships, 1,
        "the surviving predecessor row is the one that moved, and the count says so"
    );
}

/// **The reverse direction.** The collision
/// arm's rule is structural — the successor's colliding row is always the one
/// dropped, whatever either side's `confirmed_at` says — never "prefer the
/// pinned row". The four tests above never probe that distinction: every one
/// of them binds the *successor's* planted row unconfirmed, so a
/// pin-preferring refactor that kept today's structural fallback for ties
/// would pass all four while quietly reopening the laundering path in this
/// direction. Here the planted row is the one that is confirmed and the
/// predecessor's genuine row is not, so a pin-preferring rule would keep the
/// planted row's home nest — the caller who merely knew the channel id would
/// have parlayed a self-earned confirm into displacing a real, if
/// not-yet-confirmed, binding.
#[tokio::test]
async fn a_succession_does_not_let_a_confirmed_planted_row_launder_its_pin() {
    let (_url, _router, state) = start_nest().await;
    let channel = [0xC5u8; 32];
    let old_actor = [0xF0u8; 32];
    let new_actor = [0xF1u8; 32];
    let genuine_home = [0x55u8; 32];
    let planted_home = [0x66u8; 32];

    // The genuine member, unconfirmed — a legitimate binding still mid-flight.
    bind_foreign_member(
        &state,
        &channel,
        &old_actor,
        &genuine_home,
        RebindPower::Standing,
        false,
    )
    .await;
    // The plant: a first grant for the successor id, then confirmed by the
    // planted home nest's own first served call — the only power a stranger
    // needs to earn a pin.
    bind_foreign_member(
        &state,
        &channel,
        &new_actor,
        &planted_home,
        RebindPower::InsertOnly,
        true,
    )
    .await;

    state
        .db
        .record_peer_succession(&old_actor, &new_actor, b"statement", 1)
        .await
        .expect("apply")
        .expect("not refused");

    assert_eq!(
        state
            .db
            .foreign_member_binding(&channel, &new_actor)
            .await
            .unwrap(),
        Some((genuine_home, false)),
        "a pin on the successor's PLANTED row must buy it nothing — the collision rule drops the \
         successor's row unconditionally, it never prefers whichever side is confirmed"
    );
}
