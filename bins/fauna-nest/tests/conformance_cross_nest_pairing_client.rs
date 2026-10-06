//! **One-action-seeds-both pairing** (tier_3) — real-wire, two-nest,
//! *client-driven*. Proves the `linked-nests.md` § "One action seeds both ends
//! (target)" UX end-to-end through the production client stack: the shared
//! `fauna_client_pair::LinkedNestsMachine`'s native `LinkBoth` seam, driven over
//! an authenticated `NestClient`, writes the reciprocal `fauna.pair.add`
//! authorization row to **both** nests in one action.
//!
//! Topology — two in-process nests on distinct sockets with **distinct** nest
//! identities (the home-with-public-relay deployment: one user, two of their own
//! nests). Each serves the auth-bootstrap kinds (so an authenticated
//! `NestClient` can mint its bearer), the anonymous discovery kinds (so
//! `fauna.nest.info` reports each nest's own id), and the pairing kinds
//! (`fauna.pair.{list,add,revoke}`). Alice's single identity is registered on
//! **both** (the premise: the same keypair authenticates on each).
//!
//! What only this test catches over the crate's `FakeNest` unit tests: that the
//! native seam's `this_nest` (`fauna.nest.info` on the authed connection) +
//! `connect_peer` (a second `NestClient::new(url, keypair).connect()`) actually
//! resolve each nest's real Ed25519 id and write the real `fauna.pair.add` rows
//! the nest's handlers store — a break in that client→seam→handler→DB chain
//! fails here but passes the in-process fakes.
//!
//! Mirrors `conformance_cross_nest_conversations_client.rs`'s two-nest harness
//! (`start_*_nest` + `connected_client`), scoped to pairing (no MLS).

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_client_pair::LinkedNestsAction;
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::pair::default_self_sync;

/// One of the user's nests: auth-bootstrap + anonymous discovery + the pairing
/// kinds, `require_registration` (so the actor must be a registered user to auth
/// and pass the `User`-class pairing gate), a distinct nest identity, and
/// `handle_domain` set to its own authority. Returns `(http_base, authority,
/// state)`.
async fn start_pairing_nest() -> (String, String, Arc<AppState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::generate()),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::pair_handlers::register_pair_handlers(&mut b);
            // The RecoveryKey registration chain the link reconciles before
            // it writes a row, and the kit ceremony that builds it.
            fauna_nest::recovery_handlers::register_recovery_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), authority, state)
}

#[tokio::test]
async fn link_both_seeds_reciprocal_rows_on_both_nests_real_wire() {
    let (a_base, _a_auth, a_state) = start_pairing_nest().await;
    let (b_base, _b_auth, b_state) = start_pairing_nest().await;

    // Alice's single identity, registered on BOTH nests (the home-relay premise:
    // the user is authenticated to each).
    let alice = ActorKeypair::generate();
    let alice_id = alice.actor_id();
    a_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();
    b_state
        .db
        .create_user(&alice_id.0, "free", "alice")
        .await
        .unwrap();

    let a_nest_id = a_state.nest_identity.public_key_bytes();
    let b_nest_id = b_state.nest_identity.public_key_bytes();
    assert_ne!(
        a_nest_id, b_nest_id,
        "the two nests have distinct identities"
    );

    // Connect to A and build the production native machine over it.
    let client_a = connected_client(&a_base, alice).await;
    let machine = fauna_client_pair::build_linked_nests_machine(client_a);

    // ONE action: link A and B by B's address. The machine discovers id_A (on the
    // connected conn) + id_B (on a second authenticated conn to B), then writes
    // the reciprocal rows.
    machine
        .dispatch(LinkedNestsAction::LinkBoth {
            other_nest_url: b_base.clone(),
            capabilities: vec![], // → default_self_sync()
            expires_at: None,
            label: Some("public relay".into()),
        })
        .await
        .expect("LinkBoth should seed both ends");

    // The connected nest A authorized B (its row names B's id) …
    assert!(
        a_state.db.is_paired(&alice_id.0, &b_nest_id).await.unwrap(),
        "nest A must hold a pairing row naming nest B (A authorizes B to pull)"
    );
    // … and nest B authorized A (its row names A's id) — the row whose presence
    // makes a private nest's relay worker run for the actor.
    assert!(
        b_state.db.is_paired(&alice_id.0, &a_nest_id).await.unwrap(),
        "nest B must hold the reciprocal pairing row naming nest A"
    );

    // Each nest holds exactly one row, default self-sync caps (incl. mail_pull),
    // and the other nest's url for display.
    let a_rows = a_state
        .db
        .list_pairings_for_actor(&alice_id.0)
        .await
        .unwrap();
    assert_eq!(a_rows.len(), 1);
    assert_eq!(a_rows[0].private_nest_id, b_nest_id.to_vec());
    assert_eq!(a_rows[0].capabilities, default_self_sync());
    assert_eq!(a_rows[0].label.as_deref(), Some("public relay"));
    assert_eq!(a_rows[0].nest_url.as_deref(), Some(b_base.as_str()));

    let b_rows = b_state
        .db
        .list_pairings_for_actor(&alice_id.0)
        .await
        .unwrap();
    assert_eq!(b_rows.len(), 1);
    assert_eq!(b_rows[0].private_nest_id, a_nest_id.to_vec());
    assert_eq!(b_rows[0].capabilities, default_self_sync());
    // The reciprocal row carries no user label (the label rides the row the user
    // is looking at, on the connected nest).
    assert!(b_rows[0].label.is_none());

    // The machine's snapshot re-lists the connected nest's pairings.
    let snap = machine.snapshot();
    assert_eq!(snap.pairings.len(), 1);
    assert_eq!(snap.pairings[0].nest_id, hex::encode(b_nest_id));
}

// ── The chain follows the link ───────────────────────────────────────────────
//
// `identity-succession.md` § Enforcement on the home nest → *Every nest the
// identity is linked to*, clause (a): the link action reconciles the two
// nests' RecoveryKey registration chains before it writes a pairing row.

/// One of Alice's nests: its address, its state, and her own authenticated
/// connection to it.
type AliceNest = (String, Arc<AppState>, Arc<fauna_client::NestClient>);

/// Alice on two nests, each reached over its own authenticated connection.
async fn alice_on_two_nests() -> (ActorKeypair, AliceNest, AliceNest) {
    let alice = ActorKeypair::generate();
    let mut nests = Vec::new();
    for _ in 0..2 {
        let (base, _authority, state) = start_pairing_nest().await;
        state
            .db
            .create_user(&alice.actor_id().0, "free", "alice")
            .await
            .unwrap();
        let client =
            connected_client(&base, ActorKeypair::from_secret(*alice.secret_bytes())).await;
        nests.push((base, state, client));
    }
    let b = nests.pop().unwrap();
    let a = nests.pop().unwrap();
    (alice, a, b)
}

/// The chain `state`'s nest holds for `actor`, as the verbatim records.
async fn registration_chain(state: &AppState, actor: &[u8; 32]) -> Vec<Vec<u8>> {
    state
        .db
        .list_recovery_registrations(actor)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.record)
        .collect()
}

/// A recovery kit minted for `alice` at the nest behind `client`.
async fn kit_at(client: &Arc<fauna_client::NestClient>, alice: &ActorKeypair, root: u8) {
    fauna_client_recovery::create_kit_with_root(
        &fauna_client_recovery::RecoveryClient::new(Arc::clone(client)),
        alice,
        None,
        fauna_core::recovery::RecoveryKey::from_bytes([root; 32]),
        &[],
    )
    .await
    .expect("the kit ceremony lands");
}

fn link_both(other_nest_url: &str) -> LinkedNestsAction {
    LinkedNestsAction::LinkBoth {
        other_nest_url: other_nest_url.to_string(),
        capabilities: vec![],
        expires_at: None,
        label: None,
    }
}

/// A link made by an identity that holds a kit leaves the same chain at both
/// nests: the nest being linked receives the connected nest's records
/// verbatim, through its own `registration.submit`, before either row.
#[tokio::test]
async fn link_both_leaves_the_same_recovery_chain_at_both_nests_real_wire() {
    let (alice, (_a_base, a_state, client_a), (b_base, b_state, _client_b)) =
        alice_on_two_nests().await;
    let actor = alice.actor_id().0;
    kit_at(&client_a, &alice, 0x21).await;
    assert!(registration_chain(&b_state, &actor).await.is_empty());

    fauna_client_pair::build_linked_nests_machine(client_a)
        .dispatch(link_both(&b_base))
        .await
        .expect("LinkBoth carries the chain and seeds both ends");

    let home = registration_chain(&a_state, &actor).await;
    assert_eq!(home.len(), 1);
    assert_eq!(
        registration_chain(&b_state, &actor).await,
        home,
        "the linked nest holds the connected nest's chain, byte for byte"
    );
    let (a_nest_id, b_nest_id) = (
        a_state.nest_identity.public_key_bytes(),
        b_state.nest_identity.public_key_bytes(),
    );
    assert!(a_state.db.is_paired(&actor, &b_nest_id).await.unwrap());
    assert!(b_state.db.is_paired(&actor, &a_nest_id).await.unwrap());
}

/// Two nests holding different first registrations for one identity — what a
/// seed thief leaves at a nest that held no chain — refuse to link, with the
/// page's own wording, and no pairing row is written at either.
#[tokio::test]
async fn link_both_refuses_two_nests_holding_different_recovery_keys_real_wire() {
    let (alice, (_a_base, a_state, client_a), (b_base, b_state, client_b)) =
        alice_on_two_nests().await;
    let actor = alice.actor_id().0;
    kit_at(&client_a, &alice, 0x21).await;
    kit_at(&client_b, &alice, 0x66).await;

    let machine = fauna_client_pair::build_linked_nests_machine(client_a);
    let err = machine
        .dispatch(link_both(&b_base))
        .await
        .expect_err("a fork is never linked");
    assert!(
        matches!(
            err,
            fauna_client_pair::PairDispatchError::RecoveryKeysDiffer
        ),
        "{err:?}"
    );
    assert_eq!(
        machine.snapshot().error.as_deref(),
        Some(fauna_i18n::strings::nests::LINK_RECOVERY_KEYS_DIFFER)
    );
    for state in [&a_state, &b_state] {
        assert!(
            state
                .db
                .list_pairings_for_actor(&actor)
                .await
                .unwrap()
                .is_empty(),
            "no pairing row is written"
        );
        assert_eq!(
            registration_chain(state, &actor).await.len(),
            1,
            "and neither chain is submitted over the other"
        );
    }
}

// ── The link action delivers first ───────────────────────────────────────────
//
// `identity-succession.md` § Enforcement on the home nest → *Every nest the
// identity is linked to*, **The road**'s last sentence: linking a nest as an
// identity that has predecessors submits their statements there before it
// connects, so re-linking lands the succession at a nest no owed list named.

/// An identity succeeded at H whose account at L was never paired — so the
/// succession owes L nothing — with H's RecoveryKey chain for the retired
/// identity at L when `chain_at_l` (what a kit made while signed in there, or
/// an earlier link, leaves). Returns the successor's connection to H, the
/// retired and successor keypairs, and the statement H applied.
struct Succeeded {
    h: AliceNest,
    l: (String, Arc<AppState>),
    old: ActorKeypair,
    new: ActorKeypair,
    statement: Vec<u8>,
}

async fn succeeded_at_h(chain_at_l: bool) -> Succeeded {
    let (h_base, _, h_state) = start_pairing_nest().await;
    let (l_base, _, l_state) = start_pairing_nest().await;
    let old = ActorKeypair::from_secret([0x11; 32]);
    let new = ActorKeypair::from_secret([0x33; 32]);
    let old_id = old.actor_id().0;
    for state in [&h_state, &l_state] {
        state
            .db
            .create_user(&old_id, "free", "alice")
            .await
            .unwrap();
    }
    {
        let old_at_h = connected_client(&h_base, ActorKeypair::from_secret([0x11; 32])).await;
        kit_at(&old_at_h, &old, 0x21).await;
    }
    if chain_at_l {
        let old_at_l = connected_client(&l_base, ActorKeypair::from_secret([0x11; 32])).await;
        for record in registration_chain(&h_state, &old_id).await {
            fauna_client_core::recovery_chain::submit_registration_record(&*old_at_l, &record)
                .await
                .expect("H's chain registers at L, verbatim");
        }
    }
    let statement = common::succession_bytes(
        &fauna_core::recovery::RecoveryKey::from_bytes([0x21; 32]),
        old_id,
        new.signing_key(),
        Some(old.signing_key()),
        2,
    );
    common::submit_succession(&h_state.rpc_router, &h_state, statement.clone())
        .await
        .expect("the succession lands at H");
    let new_at_h = connected_client(&h_base, ActorKeypair::from_secret([0x33; 32])).await;
    let owed = fauna_client_core::succession_delivery::fetch_owed_nests(&*new_at_h)
        .await
        .unwrap();
    assert!(owed.is_empty(), "precondition: no owed list names L");
    Succeeded {
        h: (h_base, h_state, new_at_h),
        l: (l_base, l_state),
        old,
        new,
        statement,
    }
}

/// A sign-in at `base` as `keypair`: `Ok` or the refusal it ended on.
async fn sign_in(base: &str, keypair: &ActorKeypair) -> Result<(), fauna_client::NestClientError> {
    fauna_client::NestClient::new(
        base.to_string(),
        ActorKeypair::from_secret(*keypair.secret_bytes()),
    )
    .connect()
    .await
}

/// The main arm: L holds the retired identity's chain, so the statement the
/// link submits before it connects lands there. L then answers the retired key
/// `fauna.auth.superseded`, and the link completes under the successor.
#[tokio::test]
async fn link_both_lands_the_succession_at_a_nest_no_owed_list_named_real_wire() {
    let s = succeeded_at_h(true).await;
    let (h_base, h_state, new_at_h) = s.h;
    let (l_base, l_state) = s.l;
    let (old_id, new_id) = (s.old.actor_id().0, s.new.actor_id().0);
    sign_in(&l_base, &s.old)
        .await
        .expect("precondition: L has not heard of the succession");

    fauna_client_pair::build_linked_nests_machine(new_at_h)
        .dispatch(link_both(&l_base))
        .await
        .expect("the link delivers, then links under the successor");

    let row = l_state
        .db
        .succession_for(&old_id)
        .await
        .unwrap()
        .expect("L applied the succession");
    assert_eq!(row.new_actor_id, new_id.to_vec());
    assert_eq!(
        row.statement, s.statement,
        "the statement H applied, verbatim"
    );
    let err = sign_in(&l_base, &s.old)
        .await
        .expect_err("L refuses the retired key");
    // The bearer mint refuses before any session exists, so the code rides
    // the auth error's text rather than a typed `Rpc` refusal.
    assert!(
        err.to_string()
            .contains(fauna_protocol::RpcError::CODE_SUPERSEDED),
        "{err:?}"
    );
    let (h_nest_id, l_nest_id) = (
        h_state.nest_identity.public_key_bytes(),
        l_state.nest_identity.public_key_bytes(),
    );
    assert!(h_state.db.is_paired(&new_id, &l_nest_id).await.unwrap());
    assert!(l_state.db.is_paired(&new_id, &h_nest_id).await.unwrap());
    let _ = h_base;
}

/// The chain-less arm (stated bound 2): L holds the retired identity's account
/// and no chain, so it refuses the statement. That refusal does not stop the
/// link, and the link registers nothing: its sign-in as the successor is
/// refused at L, the successor holds no account there — which would make the
/// runtime's later delivery `successor_exists` (bound 4) — and the retired key
/// still signs in, for the runtime road to answer with the retired seed.
#[tokio::test]
async fn a_chain_less_nest_refuses_the_delivery_and_the_link_registers_no_successor_there() {
    let s = succeeded_at_h(false).await;
    let (_h_base, h_state, new_at_h) = s.h;
    let (l_base, l_state) = s.l;
    let (old_id, new_id) = (s.old.actor_id().0, s.new.actor_id().0);

    let machine = fauna_client_pair::build_linked_nests_machine(new_at_h);
    let err = machine
        .dispatch(link_both(&l_base))
        .await
        .expect_err("L does not know the successor");
    assert!(
        matches!(err, fauna_client_pair::PairDispatchError::Nest(_)),
        "the link ends at its own sign-in at L: {err:?}"
    );

    assert!(l_state.db.succession_for(&old_id).await.unwrap().is_none());
    assert!(
        l_state.db.get_user(&new_id).await.unwrap().is_none(),
        "no successor account at L"
    );
    sign_in(&l_base, &s.old)
        .await
        .expect("the retired key still signs in at L");
    for (state, actor) in [(&h_state, &new_id), (&l_state, &old_id)] {
        assert!(
            state
                .db
                .list_pairings_for_actor(actor)
                .await
                .unwrap()
                .is_empty(),
            "no pairing row is written"
        );
    }
}
