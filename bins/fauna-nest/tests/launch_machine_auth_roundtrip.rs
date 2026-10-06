//! tier_3 — `fauna_launch_machine::LaunchMachine` drives its launch-flow auth
//! over the pre-identity WS-RPC `fauna.auth.{challenge,verify,handshake}` kinds
//! against a **real nest binary**, with **zero HTTP** `/api/v1/auth/*` traffic.
//! This is the launch-machine half of the "no HTTP anywhere in production"
//! bearer migration (tracked internally, § S3 step A
//! part 2): `apps/fauna-linux`'s `LaunchMachineBearer` (and the silent-challenge
//! fast path) used to authenticate over HTTP; they now ride the anonymous WS
//! connection via the production `WsAuthConnector`.
//!
//! Three assertions, all end-to-end through the real connector + nest:
//!  1. **Silent challenge → Online** — a *registered* actor's `start()` runs
//!     `fauna.auth.challenge` + `fauna.auth.verify` and reaches `Online` with a
//!     minted bearer.
//!  2. **Unregistered → wizard** — a fresh actor's `verify` returns
//!     `fauna.auth.not_registered`; the flow probes setup status and drops to
//!     the onboarding wizard (assume-claimed fallback → `InviteRequest`).
//!  3. **Handshake refresh** — from `Online`, `refresh_token()` re-mints over
//!     `fauna.auth.handshake` and stays `Online`.
//!
//! The "zero HTTP" property is structural: `WsAuthConnector` only ever opens the
//! anonymous WS connection. The nest here registers **only** the auth-bootstrap
//! kinds (no HTTP auth route is exercised), so a regression to HTTP would fail.

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_launch_machine::{
    InMemoryPersistence, LaunchMachine, LaunchPhase, LaunchWizardEntry, NullObserver,
};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;

/// Spin an in-process real nest whose anonymous endpoint serves the
/// auth-bootstrap kinds. `for_test` defaults to auto-register, so a fresh
/// keypair's `fauna.auth.handshake` both creates the account and mints a
/// bearer. Returns the `http://` base URL (the connector swaps to `ws://`).
/// Returns the base URL **and** the nest's db, so a test can admit an actor —
/// there is no auto-provision to lean on any more.
async fn start() -> (String, Arc<CacheDb>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
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

/// Admit `kp` — give it a real `users` row, as the registration ceremony (or an
/// admin) would.
///
/// This used to "register" by simply minting a bearer over `fauna.auth.handshake`
/// and letting `for_test`'s auto-provision invent the account. That branch is
/// **deleted**: a handshake proves key possession, never admission, so an
/// unadmitted actor now gets `not_registered` from both `handshake` and `verify`.
async fn register(db: &CacheDb, kp: &ActorKeypair) {
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
}

fn persistence(base: &str, kp: &ActorKeypair) -> Arc<InMemoryPersistence> {
    Arc::new(
        InMemoryPersistence::new()
            .with_identity(kp.signing_key().to_bytes().to_vec())
            .with_nest_url(base.to_string()),
    )
}

#[tokio::test]
async fn silent_challenge_reaches_online_over_ws() {
    let (base, db) = start().await;
    let kp = ActorKeypair::generate();
    register(&db, &kp).await;

    let m = LaunchMachine::new(Arc::new(NullObserver), persistence(&base, &kp));
    m.start().await;

    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::Online,
        "registered actor should reach Online via the WS silent challenge; last_error = {:?}",
        m.snapshot().last_error
    );
    assert!(
        m.current_bearer().is_some_and(|b| !b.is_empty()),
        "Online machine should hold a non-empty WS-minted bearer"
    );
}

#[tokio::test]
async fn unregistered_actor_with_a_stored_nest_url_lands_sign_in_refused_over_ws() {
    let (base, _db) = start().await;
    // Fresh, deliberately unregistered actor — never admitted, so the auth path
    // must refuse it. The persistence carries identity + nest_url, the store
    // shape only a prior sign-in here writes, so this is the previously-signed-in
    // row (`onboarding.md` § App-launch routing): the honest refused surface,
    // never the invite wizard.
    let kp = ActorKeypair::generate();

    let m = LaunchMachine::new(Arc::new(NullObserver), persistence(&base, &kp));
    m.start().await;

    // `fauna.auth.verify` → not_registered; the setup-status probe (no
    // `fauna.setup.status` kind on this minimal nest) fails and assumes
    // claimed — the safe default, since verify itself answered.
    let snap = m.snapshot();
    assert_eq!(
        snap.phase,
        LaunchPhase::Offline { transient: false },
        "last_error = {:?}",
        snap.last_error
    );
    assert!(snap.sign_in_refused, "the verdict rides the side channel");
    assert_ne!(
        snap.phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::InviteRequest
        }
    );
}

/// The row's whole reason to exist, end to end over the real WS ceremony: an
/// admitted actor the admin then SUSPENDS relaunches into the refused surface
/// (verify answers the opaque `not_registered` — `login.md` § Silent Challenge), and once the admin restores them
/// (`admin.md` § *Cutting a user off* → Restore: "no client-side recovery step
/// and no re-onboarding") a plain Retry lands them back online — same actor,
/// same keys, no wizard in between.
#[tokio::test]
async fn suspended_actor_is_refused_and_a_retry_after_restore_lands_online() {
    let (base, db) = start().await;
    let kp = ActorKeypair::generate();
    register(&db, &kp).await;
    assert!(
        db.suspend_user_now(&kp.actor_id().0, "test", "other")
            .await
            .unwrap(),
        "the fresh row must take the suspension"
    );

    let m = LaunchMachine::new(Arc::new(NullObserver), persistence(&base, &kp));
    m.start().await;

    let snap = m.snapshot();
    assert_eq!(
        snap.phase,
        LaunchPhase::Offline { transient: false },
        "a suspended actor must not mint; last_error = {:?}",
        snap.last_error
    );
    assert!(snap.sign_in_refused);
    assert!(m.current_bearer().is_none());

    // Restore is the admin's button; the user's way back in is Retry.
    assert!(db.cancel_eviction(&kp.actor_id().0).await.unwrap());
    m.retry_silent_challenge().await;

    let snap = m.snapshot();
    assert_eq!(
        snap.phase,
        LaunchPhase::Online,
        "restored → retry → online with no re-onboarding; last_error = {:?}",
        snap.last_error
    );
    assert!(!snap.sign_in_refused);
    assert!(m.current_bearer().is_some());
}

#[tokio::test]
async fn token_refresh_remints_over_the_ws_silent_challenge() {
    let (base, db) = start().await;
    let kp = ActorKeypair::generate();
    register(&db, &kp).await;

    let m = LaunchMachine::new(Arc::new(NullObserver), persistence(&base, &kp));
    m.start().await;
    assert_eq!(m.snapshot().phase, LaunchPhase::Online);

    let launch_bearer = m.current_bearer().expect("launch minted a bearer");
    // Explicit refresh mints a fresh bearer over the same silent challenge the
    // launch took (`login.md` § When to use which) — a second session, so the
    // nest lists two own ids and the bearer changed.
    m.refresh_token().await;
    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::Online,
        "refresh over the WS silent challenge should keep the machine Online; last_error = {:?}",
        m.snapshot().last_error
    );
    let refreshed = m
        .current_bearer()
        .expect("machine should hold a bearer after a silent-challenge refresh");
    assert!(!refreshed.is_empty());
    assert_ne!(refreshed, launch_bearer, "a refresh mints a NEW session");
    assert_eq!(
        m.own_token_ids().len(),
        2,
        "both the launch and the refresh session are this machine's own"
    );
}
