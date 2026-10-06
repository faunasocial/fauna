//! tier_3 — `fauna_client::ws_challenge_bearer::WsChallengeBearer` mints its
//! bearer over the pre-identity WS-RPC silent challenge (`fauna.auth.challenge`
//! then `fauna.auth.verify`) against a **real nest**, with no `fauna.auth.handshake`
//! and no HTTP. The `AuthClient` path (`FfiNestClient` → the native UniFFI apps,
//! and `bins/fauna-sync`) and the `fauna-ffi` `mint_bearer` export both ride it
//! (`login.md` § When to use which: every app-held bearer, refresh included, is
//! minted over the silent challenge, whose freshness is the nest's nonce rather
//! than a client timestamp held to ±30 s).
//!
//! The witness nest serves **only** the silent-challenge triple
//! (`register_silent_challenge_handlers`: the opening `fauna.auth.nest_handshake`
//! identity read every login binds, then challenge + verify) — so a regression
//! back to the handshake is an unknown-kind refusal here, not a quiet pass. The
//! first test pins that the nest really does refuse the handshake, so the
//! others' greens mean what they say. And since the nest verifies only the
//! nest-bound form (`login.md` § Binding the nest), every green mint below is
//! also the witness that the shared client read the identity and bound it.
//!
//! Assertions, all load-bearing:
//!  1. **Mint over the silent challenge** — `WsChallengeBearer::bearer()`
//!     returns a token, caches it (a second call does not re-mint), and
//!     re-mints after `notify_401`.
//!  2. **The free mint** the FFI export rides returns a usable token whose
//!     expiry is anchored on the client's clock (`now + expires_in`).
//!  3. **End-to-end** — a `NestClient` (which builds a `WsChallengeBearer`
//!     internally) reaches `Connected`: the minted bearer authenticates the
//!     separate authenticated `GET /api/v1/ws/{actor_id}` upgrade.

use std::sync::Arc;
use std::time::Duration;

use fauna_client::NestClient;
use fauna_client::ws_challenge_bearer::WsChallengeBearer;
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest_http::BearerSource;

/// Spin an in-process real nest whose anonymous endpoint serves **only** the
/// silent challenge. Returns the `http://` base URL (the connector swaps to
/// `ws://`) **and** the nest's db: every actor below must be admitted
/// explicitly — a mint proves key possession, never admission.
async fn start() -> (String, Arc<CacheDb>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_silent_challenge_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), db)
}

/// Admit an actor — a real `users` row, as the ceremony or an admin would create.
async fn admit(db: &CacheDb, kp: &ActorKeypair) {
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

#[tokio::test]
async fn the_witness_nest_refuses_the_handshake() {
    // The discriminator the rest of this file leans on: were a mint below to
    // fall back to `fauna.auth.handshake`, it would meet this refusal.
    let (base, db) = start().await;
    let kp = ActorKeypair::generate();
    admit(&db, &kp).await;
    let refused =
        fauna_anon_client::mint_bearer_over_handshake(&base, kp.actor_id().0, kp.signing_key())
            .await;
    assert!(
        refused.is_err(),
        "a nest serving only the silent challenge must refuse the handshake"
    );
}

#[tokio::test]
async fn ws_challenge_bearer_mints_and_caches_over_the_silent_challenge() {
    let (base, db) = start().await;
    let kp = ActorKeypair::generate();
    admit(&db, &kp).await;
    let bearer = WsChallengeBearer::new(base, kp.actor_id().0, kp.signing_key().clone());

    // First mint: a real token over `fauna.auth.{challenge,verify}`.
    let t1 = bearer
        .bearer()
        .await
        .expect("mint bearer over the silent challenge");
    assert!(!t1.is_empty(), "minted bearer should be non-empty");

    // Cached: a second call returns the same token without re-minting (the
    // 1-hour TTL is far from the 60 s pre-expiry buffer).
    let t2 = bearer.bearer().await.expect("cached bearer");
    assert_eq!(t1, t2, "second bearer() should hit the cache");

    // After a 401, the cache is cleared and the next call re-mints.
    bearer.notify_401().await;
    let t3 = bearer.bearer().await.expect("re-mint after notify_401");
    assert!(!t3.is_empty(), "re-minted bearer should be non-empty");
    assert_ne!(t1, t3, "a re-mint after notify_401 is a fresh session");
}

#[tokio::test]
async fn mint_bearer_over_silent_challenge_returns_a_usable_token_on_the_clients_clock() {
    let (base, db) = start().await;
    let kp = ActorKeypair::generate();
    admit(&db, &kp).await;

    // The shared free fn both `WsChallengeBearer` and the `fauna-ffi`
    // `mint_bearer` export ride.
    let before = now_secs();
    let minted = fauna_client::ws_challenge_bearer::mint_bearer_over_silent_challenge(
        &base,
        kp.signing_key(),
    )
    .await
    .expect("mint bearer over the silent challenge");
    let after = now_secs();
    assert!(
        !minted.token.is_empty(),
        "minted bearer should be non-empty"
    );
    assert!(!minted.token_id.is_empty(), "the session id rides along");
    // Anchored at receipt: `now_client + expires_in`, with the nest's 1-hour
    // session TTL as `expires_in`. (Same clock in-process, so this pins the
    // plumbing; the wrong-clock direction is `fauna-anon-client`'s
    // `a_client_hours_ahead_anchors_on_its_own_clock`.)
    assert!(
        (before + 3600..=after + 3600).contains(&minted.expires_at),
        "expiry {} should be the client's now + 3600 (now in {before}..={after})",
        minted.expires_at
    );
}

#[tokio::test]
async fn nest_client_authenticates_with_a_silent_challenge_bearer() {
    let (base, db) = start().await;
    let kp = ActorKeypair::generate();
    admit(&db, &kp).await;

    // `NestClient::new` builds a `WsChallengeBearer` internally; `connect`
    // mints over the silent challenge then opens the authenticated WS with
    // that bearer.
    let nest = NestClient::new(base, kp);
    nest.connect()
        .await
        .expect("connect (silent-challenge mint + WS upgrade)");

    let mut state = nest.connection_state();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if *state.borrow() == fauna_client::types::ConnectionState::Connected {
                return;
            }
            state
                .changed()
                .await
                .expect("connection-state channel open");
        }
    })
    .await
    .expect("client reached Connected within 5s on a silent-challenge bearer");
}
