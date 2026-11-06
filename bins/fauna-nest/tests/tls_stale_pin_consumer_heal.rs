//! In-process — a **pin-consumer process across a move of the nest's served
//! SPKI** (`transport-trust.md` § Cross-connection binding, rule 3, and § Pin
//! custody across processes: a consumer never mints, and the re-bind that
//! heals its stale SPKI pin verifies against the identity the app pinned).
//!
//! The process shape under test is the desktop sync agent (and the apple File
//! Provider extension): a supervised `NestClient` holding a **bearer but no
//! identity key**, over a **read-only** pin store the interactive app writes.
//! It is connected when the nest's served cert changes — an IP bridge cert
//! installed, the floor's first ACME cert, a floor-key rotation — so its
//! socket drops and its next bearer dial meets a leaf whose SPKI is not the
//! one it pinned. Before the heal (2026-11-05) that dial, and
//! every one after it, was refused with `served cert SPKI does not match the
//! pinned nest identity` for the life of the process: the sync agent in the
//! docker mail venue sat there for a whole journey,
//! waiting for an interactive app that never re-handshakes on its behalf.
//!
//! - the same identity signs the new leaf → the supervisor's reconnect runs
//!   the never-minting re-bind against the app's identity pin and comes back
//!   Connected under the new SPKI;
//! - another identity signs it → the re-bind is refused, and the dial's typed
//!   identity verdict stops the supervisor: `NestIdentityChanged`, no redial
//!   at the forwarder, and an `ensure_connected` after it parks instead of
//!   dialling (§ Post-auth surfacing — the plumbing rule).
//!
//! Lives in its own integration-test binary: it installs the process-global
//! pin store as a read-only backend, which would race the writable twin
//! (`tls_stale_pin_heal.rs`) if they shared a process. Both tests here share
//! the one read-only store, installed once; pins are keyed by authority, and
//! each test dials its own nest and forwarder ports.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use common::{SwitchableForwarder, rotating_nest, wait_for_connected};
use fauna_anon_client::cert_binding::{DiskPinStore, NestIdentityPinStore};
use fauna_client::auth_client::AuthClient;
use fauna_client::types::ConnectionState;
use fauna_client::ws_challenge_bearer::WsChallengeBearer;
use fauna_client::{NestClient, NestClientError};
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest_http::BearerSource;

/// The nest's Ed25519 deployment identity seed.
const NEST_SIGNING_SEED: [u8; 32] = [5u8; 32];
/// Another box's identity: the one a stale pin must never be healed onto.
const OTHER_SIGNING_SEED: [u8; 32] = [9u8; 32];

fn identity_of(seed: [u8; 32]) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes()
}

/// The directory the "app" writes its pins into, read through the one
/// read-only store this process installs — the consumer's shape. Installed
/// once, before any test dials, so no test ever runs over the default
/// in-memory store.
fn consumer_store_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let r = fauna_anon_client::trust::fresh_nonce();
        let dir = std::env::temp_dir().join(format!("fauna-consumer-heal-{}", hex::encode(r)));
        std::fs::create_dir_all(&dir).unwrap();
        fauna_anon_client::trust::install_pin_store(Arc::new(
            fauna_anon_client::cert_binding::ReadOnlyDiskPinStore::open_in_dir(&dir),
        ));
        dir
    })
}

/// "The app" pins the nest identity for `authority`, as its own login would.
///
/// Through ONE writer for the whole binary, as the app holds one: a
/// `DiskPinStore` loads the file once at open and rewrites it whole from that
/// cache, so a fresh writer per call let this file's two tests — run in
/// parallel over the one shared dir — overwrite each other's pins (a lost
/// update; the losing test's own mint then found no pin for its host).
fn app_pins(authority: &str, identity: [u8; 32]) {
    static WRITER: OnceLock<DiskPinStore> = OnceLock::new();
    WRITER
        .get_or_init(|| DiskPinStore::open_in_dir(consumer_store_dir()))
        .set(authority, identity);
}

/// Admit an actor and mint a bearer the signed way, directly against the nest
/// at `direct` — the app's half; the identity it verifies against is the one
/// the app pinned for the direct authority.
async fn app_bearer(
    db: &Arc<CacheDb>,
    direct: std::net::SocketAddr,
    secret: [u8; 32],
) -> Arc<WsChallengeBearer> {
    let kp = ActorKeypair::from_secret(secret);
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
    let bearer = Arc::new(WsChallengeBearer::new(
        format!("https://{direct}"),
        kp.actor_id().0,
        kp.signing_key().clone(),
    ));
    bearer.bearer().await.expect("app-side mint over wss");
    bearer
}

/// "The consumer process": a supervised client over the forwarder, holding the
/// app's bearer and no identity key.
fn consumer_client(
    base: &str,
    secret: [u8; 32],
    bearer: Arc<WsChallengeBearer>,
) -> Arc<NestClient> {
    let auth = Arc::new(AuthClient::with_bearer_source(
        base.to_string(),
        ActorKeypair::from_secret(secret),
        bearer as Arc<dyn BearerSource>,
        fauna_client::auth_client::pinned_http_client(base),
    ));
    NestClient::with_auth(auth)
}

/// Wait for the supervisor to notice the dropped socket: the state leaves
/// `Connected` once the read side sees EOF. Bounded well above the keepalive
/// the adapter would otherwise need to notice a silent peer.
async fn wait_for_disconnected(nest: &Arc<NestClient>) {
    let mut state = nest.connection_state();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if *state.borrow() != ConnectionState::Connected {
                return;
            }
            if state.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    })
    .await
    .expect("the supervisor notices the dropped socket");
}

#[tokio::test]
async fn a_consumer_reconnects_under_the_new_spki_after_the_served_cert_moves() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let identity = || ed25519_dalek::SigningKey::from_bytes(&NEST_SIGNING_SEED);
    let rotating = rotating_nest(db.clone(), identity()).await;
    let (before, spki_before) = rotating.serve_leaf().await;
    let (after, spki_after) = rotating.serve_leaf().await;
    assert_ne!(
        spki_before, spki_after,
        "fixture: the rotation moves the SPKI"
    );
    let via = SwitchableForwarder::start(before).await;
    let base = via.nest_url();
    let authority = fauna_anon_client::authority_of(&base);
    let app_pin = identity_of(NEST_SIGNING_SEED);

    // The app's pins for both authorities — the nest's direct one its login
    // verified against, and the forwarder's the consumer dials — as they are
    // for a sync agent the app provisioned. No SPKI graduated yet.
    app_pins(
        &fauna_anon_client::authority_of(&format!("https://{before}")),
        app_pin,
    );
    app_pins(&authority, app_pin);
    let secret = [0x6cu8; 32];
    let bearer = app_bearer(&db, before, secret).await;
    assert_eq!(fauna_anon_client::pinned_spki(&authority), None);

    let nest = consumer_client(&base, secret, bearer);
    nest.connect().await.expect("connect spawns the supervisor");
    wait_for_connected(&nest).await;
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_before),
        "the consumer's first dial graduated the leaf served before the rotation"
    );

    // The rotation, mid-session: the nest serves the new leaf (and signs its
    // binding over it) and the open socket is gone.
    via.switch(after);
    rotating.rotate_to(spki_after);
    wait_for_disconnected(&nest).await;

    // The supervisor's reconnect: the pinned dial is refused for the moved
    // SPKI, the never-minting re-bind verifies the new leaf against the app's
    // identity pin, and the retry comes back Connected under the new SPKI.
    wait_for_connected(&nest).await;
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_after),
        "the consumer re-pinned the leaf the pinned identity signed"
    );
    assert_eq!(
        DiskPinStore::open_in_dir(consumer_store_dir()).get(&authority),
        Some(app_pin),
        "the app's identity pin is untouched: the consumer verified, never minted"
    );
}

#[tokio::test]
async fn a_consumer_stops_on_the_identity_verdict_when_another_identity_signs_the_moved_cert() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let nest_box = rotating_nest(
        db.clone(),
        ed25519_dalek::SigningKey::from_bytes(&NEST_SIGNING_SEED),
    )
    .await;
    let (before, spki_before) = nest_box.serve_leaf().await;
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
    let base = via.nest_url();
    let authority = fauna_anon_client::authority_of(&base);
    let app_pin = identity_of(NEST_SIGNING_SEED);
    app_pins(
        &fauna_anon_client::authority_of(&format!("https://{before}")),
        app_pin,
    );
    app_pins(&authority, app_pin);
    let secret = [0x5du8; 32];
    let bearer = app_bearer(&db, before, secret).await;

    let nest = consumer_client(&base, secret, bearer);
    // A supervisor that WOULD redial would do so within milliseconds: a loop on
    // the verdict shows up as a climbing count below, never as a slow test.
    nest.set_reconnect_backoff_for_test(Some((
        Duration::from_millis(20),
        Duration::from_millis(50),
    )));
    nest.connect().await.expect("connect spawns the supervisor");
    wait_for_connected(&nest).await;
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_before)
    );

    // The served cert "moves" to a box another identity signs; the open socket
    // is gone.
    via.reset_count();
    via.switch(other);

    // The supervisor's reconnect: the pinned dial is refused for the moved
    // SPKI, the re-bind's handshake proves another identity, and the dial's
    // typed verdict stops the supervisor for good.
    let stop = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(stop) = nest.supervisor_stop() {
                return stop;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the supervisor stops on the identity verdict");
    let NestClientError::NestIdentityChanged {
        host,
        pinned_hex,
        seen_hex,
    } = &stop
    else {
        panic!("the supervisor must stop on the identity verdict, got {stop:?}");
    };
    assert_eq!(host, &authority);
    assert_eq!(pinned_hex, &hex::encode(app_pin));
    assert_eq!(
        seen_hex,
        &Some(hex::encode(identity_of(OTHER_SIGNING_SEED)))
    );
    assert_ne!(
        *nest.connection_state().borrow(),
        ConnectionState::Connected,
        "the session is not connected to a box the pin refused"
    );

    // Many backoff intervals later, still the refused pinned dial and the
    // re-bind's anonymous handshake — a third connection would be a redial.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        via.accepted(),
        2,
        "the refused pinned dial and the re-bind's handshake, then nothing: the supervisor \
         stopped instead of redialling a box the pin refused"
    );

    // A caller that brings the connection back on its own (the sync agent's
    // relay seat, an engine's control plane) parks on the verdict: nothing is
    // dialled while the refused pin is still the one held.
    let parked = nest.ensure_connected().await;
    assert!(
        matches!(parked, Err(NestClientError::NestIdentityChanged { .. })),
        "ensure_connected parks on the verdict, got {parked:?}"
    );
    assert_eq!(via.accepted(), 2, "the park dials nothing");
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_before),
        "the refused re-bind leaves the SPKI cache as it was"
    );
    assert_eq!(
        DiskPinStore::open_in_dir(consumer_store_dir()).get(&authority),
        Some(app_pin),
        "the app's identity pin is untouched"
    );
}
