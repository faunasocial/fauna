//! In-process — the bearer dial's **stale-pin heal** against a real nest
//! router served TLS (`transport-trust.md` § Cross-connection binding, rule 3:
//! a served-cert rotation makes a pinned-SPKI mismatch *expected and benign*,
//! the client re-binds and re-pins when the bound identity still matches; and
//! § Pin custody across processes: the graduation that re-binds never mints).
//!
//! The SPKI pin is a per-process cache of the leaf a binding last proved, and
//! the nest's served leaf moves legitimately — the floor's first ACME cert, an
//! IP bridge cert installed or lapsing, a floor-key rotation. Before the heal
//! (2026-11-05) a held pin disarmed the graduate-and-retry arm
//! outright, so a client that pinned the old leaf refused every bearer dial
//! for the life of the process — measured on the sync agent in the docker
//! mail venue, whose nest re-keyed its served cert after the app had pinned
//! it.
//!
//! One nest state is served over two certs — the nest before and after a
//! rotation (`common::RotatingNest`) — behind a forwarder whose upstream the
//! test switches. A second nest serves its own cert under **another
//! identity**: the on-path box the pin exists to refuse.
//!
//! - the same identity signs the new leaf → the pinned dial is refused, one
//!   anonymous handshake re-binds against the held identity pin, the retry
//!   goes out pinned to the new SPKI and completes (three connections);
//! - another identity signs it → the handshake is refused, no retry is sent,
//!   the pin stays the old SPKI and the bearer never leaves (two connections),
//!   and the dial fails with the typed identity verdict.
//!
//! Lives in its own integration-test binary: the process-global pin store it
//! installs must stay writable for the whole run, which the read-only twin
//! (`tls_stale_pin_consumer_heal.rs`) would race.

mod common;

use std::sync::{Arc, OnceLock};

use common::{SwitchableForwarder, rotating_nest};
use fauna_anon_client::cert_binding::{DiskPinStore, NestIdentityPinStore};
use fauna_anon_client::tls_dial::{TrustedDialError, dial_ws_trusted_with_graduate_retry};
use fauna_client::ws_challenge_bearer::WsChallengeBearer;
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest_http::BearerSource;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;

/// The nest's Ed25519 deployment identity seed — what the interactive path
/// pinned for the host.
const NEST_SIGNING_SEED: [u8; 32] = [5u8; 32];
/// Another box's identity: the one a stale pin must never be healed onto.
const OTHER_SIGNING_SEED: [u8; 32] = [9u8; 32];

/// One writable `DiskPinStore` for the whole process, installed once — the
/// interactive app's / tui's store shape (both tests share it; pins are keyed
/// by authority, and each test dials its own forwarder port).
fn writable_pin_store() -> &'static Arc<DiskPinStore> {
    static STORE: OnceLock<Arc<DiskPinStore>> = OnceLock::new();
    STORE.get_or_init(|| {
        let r = fauna_anon_client::trust::fresh_nonce();
        let dir = std::env::temp_dir().join(format!("fauna-stale-pin-heal-{}", hex::encode(r)));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(DiskPinStore::open_in_dir(&dir));
        fauna_anon_client::trust::install_pin_store(store.clone());
        store
    })
}

/// Admit an actor and mint it a real bearer the interactive way, directly
/// against the nest's own authority, so the dials under test carry a valid
/// bearer. (The mint's own signed graduation pins the nest's *direct*
/// authority; the dials under test go through the forwarder, a different
/// authority, whose pins each test seeds itself.)
async fn mint_bearer(nest_addr: std::net::SocketAddr, db: &CacheDb, kp: &ActorKeypair) -> String {
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
    let bearer = WsChallengeBearer::new(
        format!("https://{nest_addr}"),
        kp.actor_id().0,
        kp.signing_key().clone(),
    );
    bearer.bearer().await.expect("interactive mint over wss")
}

/// The bearer WS request `fauna_client::ws_adapter` builds, aimed through the
/// forwarder.
fn bearer_request(via: &SwitchableForwarder, kp: &ActorKeypair, token: &str) -> Request {
    let url = fauna_ws_substrate::actor_ws_url(&via.nest_url(), &kp.actor_id_hex());
    let mut request: Request = url.into_client_request().expect("valid ws url");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        fauna_ws_substrate::bearer_subprotocol_header(token).expect("bearer header"),
    );
    request
}

/// The state every heal starts from: the identity pinned for the forwarder's
/// authority (the interactive path's doing) and the SPKI cache holding the
/// leaf the nest served **before** the rotation — graduated the real way, by
/// one dial through the forwarder while it still fronts that leaf.
async fn pin_the_first_leaf(
    via: &SwitchableForwarder,
    kp: &ActorKeypair,
    token: &str,
    first_spki: [u8; 32],
) -> String {
    let authority = fauna_anon_client::authority_of(&via.nest_url());
    let nest_identity = ed25519_dalek::SigningKey::from_bytes(&NEST_SIGNING_SEED)
        .verifying_key()
        .to_bytes();
    writable_pin_store().set(&authority, nest_identity);
    let first = dial_ws_trusted_with_graduate_retry(
        bearer_request(via, kp, token),
        fauna_ws_substrate::rpc_ws_config(),
    )
    .await;
    assert!(first.is_ok(), "precondition: the first leaf graduates");
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(first_spki),
        "precondition: the SPKI cache holds the first leaf"
    );
    drop(first);
    via.reset_count();
    authority
}

#[tokio::test]
async fn a_stale_pin_heals_when_the_pinned_identity_signs_the_new_leaf() {
    let store = writable_pin_store();
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let identity = || ed25519_dalek::SigningKey::from_bytes(&NEST_SIGNING_SEED);
    let nest = rotating_nest(db.clone(), identity()).await;
    let (before, spki_before) = nest.serve_leaf().await;
    let (after, spki_after) = nest.serve_leaf().await;
    assert_ne!(
        spki_before, spki_after,
        "fixture: the rotation moves the SPKI"
    );
    let via = SwitchableForwarder::start(before).await;
    let kp = ActorKeypair::from_secret([0x7du8; 32]);
    let token = mint_bearer(before, &db, &kp).await;
    let authority = pin_the_first_leaf(&via, &kp, &token, spki_before).await;
    let nest_identity = identity().verifying_key().to_bytes();

    // The rotation: the same nest now serves the new leaf (and signs its
    // binding over it), and the socket that saw the old one is gone.
    via.switch(after);
    nest.rotate_to(spki_after);

    let result = dial_ws_trusted_with_graduate_retry(
        bearer_request(&via, &kp, &token),
        fauna_ws_substrate::rpc_ws_config(),
    )
    .await;

    assert!(
        result.is_ok(),
        "the pinned dial is refused for the moved SPKI, the re-bind verifies the new leaf \
         against the pinned identity, and the pinned retry completes: {:?}",
        result.err()
    );
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_after),
        "the heal re-pinned the leaf the pinned identity signed"
    );
    assert_eq!(
        store.get(&authority),
        Some(nest_identity),
        "the identity pin is verified, never rewritten"
    );
    assert_eq!(
        via.accepted(),
        3,
        "the refused pinned dial, the re-bind's anonymous handshake, the pinned retry"
    );
}

#[tokio::test]
async fn a_stale_pin_is_never_healed_onto_another_identity() {
    let store = writable_pin_store();
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let nest = rotating_nest(
        db.clone(),
        ed25519_dalek::SigningKey::from_bytes(&NEST_SIGNING_SEED),
    )
    .await;
    let (before, spki_before) = nest.serve_leaf().await;
    // The box that answers after the "rotation" signs its binding with another
    // key — an on-path box, or a replaced nest the user has not re-trusted.
    let stranger = rotating_nest(
        db.clone(),
        ed25519_dalek::SigningKey::from_bytes(&OTHER_SIGNING_SEED),
    )
    .await;
    let (other, spki_other) = stranger.serve_leaf().await;
    assert_ne!(
        spki_before, spki_other,
        "fixture: the other box serves its own leaf"
    );
    let via = SwitchableForwarder::start(before).await;
    let kp = ActorKeypair::from_secret([0x7eu8; 32]);
    let token = mint_bearer(before, &db, &kp).await;
    let authority = pin_the_first_leaf(&via, &kp, &token, spki_before).await;
    let nest_identity = ed25519_dalek::SigningKey::from_bytes(&NEST_SIGNING_SEED)
        .verifying_key()
        .to_bytes();

    via.switch(other);

    let result = dial_ws_trusted_with_graduate_retry(
        bearer_request(&via, &kp, &token),
        fauna_ws_substrate::rpc_ws_config(),
    )
    .await;

    // The dial fails with the identity verdict typed — never the first dial's
    // TLS error, which the reconnect supervisor would back off and redial
    // (`transport-trust.md` § Post-auth surfacing, the plumbing rule).
    let other_identity = ed25519_dalek::SigningKey::from_bytes(&OTHER_SIGNING_SEED)
        .verifying_key()
        .to_bytes();
    match result {
        Err(TrustedDialError::IdentityChanged { host, verdict }) => {
            assert_eq!(host, authority, "the verdict names the pinned authority");
            assert_eq!(verdict.pinned_hex, hex::encode(nest_identity));
            assert_eq!(verdict.seen_hex, Some(hex::encode(other_identity)));
            assert!(
                !verdict.fork,
                "a plain change, not rotation-chain fork evidence"
            );
        }
        Err(other) => panic!("the refused re-bind must surface the identity verdict, got {other}"),
        Ok(_) => panic!("a leaf another identity signed must not be re-pinned: the dial fails"),
    }
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_before),
        "the refused re-bind leaves the SPKI cache as it was"
    );
    assert_eq!(
        store.get(&authority),
        Some(nest_identity),
        "the identity pin is untouched"
    );
    assert_eq!(
        via.accepted(),
        2,
        "the refused pinned dial and the re-bind's anonymous handshake — a third connection \
         would be a pinned retry carrying the bearer to a box the pin refused"
    );
}
