//! **mail-multidomain Phase — tier_3 acceptance for the per-domain MTA-STS
//! policy-file serving route** (`mail-multidomain.md` § Policy-file server;
//! `tls-certificates.md` § D cert-honesty coupling).
//!
//! Proves the served `/.well-known/mta-sts.txt` body is assembled per-domain
//! **through the real `build_router`** — the same axum app `main.rs` serves —
//! against a real [`fauna_nest::acme::MultiDomainCertResolver`] serving a real
//! `mail.<primary>` leaf:
//!
//! - the requested **Host** is matched against the active `mail_domains` set
//!   (multi-domain); the legacy single boot-time `state.email.domain` (always
//!   `None` in production → the route used to 404 + never serve) is removed;
//! - the served `mode:` is the domain's **stored** `mta_sts_mode` coupled to the
//!   **primary** MX (`mail.<primary>`) served-cert reality — floor ⇒ `enforce`
//!   downgrades to `testing`, trusted ⇒ the stored mode stands;
//! - the stored mode is the nest's own: a domain added over the wire (no request
//!   names a mode) is stored `testing`, and the nest's advance moves it to
//!   `enforce` once the mail name is trusted and the 7-day window has passed
//!   (`mail-multidomain.md` § The advance) — [`nest_advances_an_added_domain_to_enforce`];
//! - every domain advertises the one shared `mx: mail.<primary>` target
//!   (`mail-multidomain.md` § Architectural rules — one MX across all domains);
//! - an unknown host ⇒ 404.
//!
//! The coupling rule itself (`MtaStsMode::coupled_to_cert`) is unit-tested in
//! `fauna-mail`; the handler-body coupling in `lib.rs::mta_sts_honesty_tests`.
//! This test owns the **full-route** integration — that the route is mounted on
//! `build_router`, reads `mail_domains` over the live `db`, and threads the
//! served-cert SPKI through `served_cert_spki` into the body — the closest
//! achievable analog to a binary-nest run.
//!
//! *(Why not a binary `nest_instance`: the tier_3 e2e nest serves plain HTTP with
//! **no** TLS resolver — `served_cert_spki == None` — so it can only prove the
//! never-enforce floor branch, and the no-variant-config rule forbids a
//! domain-configured HTTPS nest. The real-HTTPS floor→trusted flip is the Phase-6
//! tier_4 docker slice, `test_mta_sts_serving.py`.)*

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ed25519_dalek::SigningKey;
use fauna_nest::acme::ServedCertSpki;
use fauna_nest::db::CacheDb;
use fauna_nest::db::mail_domains::MTA_STS_TESTING_WINDOW_MS;
use fauna_nest::mta_sts_advance::advance_mta_sts_modes_at;
use fauna_nest::routes::AppState;
use fauna_protocol::bridge_routing::{
    AddLocalDomainReply, AddLocalDomainRequest, ListLocalDomainsReply, ListLocalDomainsRequest,
};
use tower::ServiceExt; // oneshot

const PRIMARY: &str = "fauna.test";
const MX_SNI: &str = "mail.fauna.test";
const ADDITIONAL: &str = "community.test";

/// An `AppState` whose `served_cert_spki` is `resolver` (or `None` to model a
/// plain-HTTP nest), with a fresh in-memory db.
fn nest(resolver: Option<Arc<dyn ServedCertSpki>>) -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    Arc::new(AppState {
        served_cert_spki: resolver,
        ..AppState::for_test(db)
    })
}

// `resolver_from_pem`/`floor_resolver`/`trusted_resolver`: this file and
// `conformance_dns_dane_coupling.rs` had this exact trio under the same names
// (`common::floor_resolver`/`common::trusted_resolver`).

/// Seed `state` with an admin actor and a `is_primary=true` mail domain whose
/// stored MTA-STS mode is `mode`.
async fn seed_primary(state: &Arc<AppState>, domain: &str, mode: &str) {
    let actor_sk = SigningKey::from_bytes(&[0x22u8; 32]);
    let actor: [u8; 32] = actor_sk.verifying_key().to_bytes();
    state.db.add_admin_actor(&actor[..]).await.unwrap();
    state
        .db
        .add_mail_domain(domain, true, mode, "expand_primary", None, None)
        .await
        .unwrap();
}

/// GET `/.well-known/mta-sts.txt` over the real router with `Host: <host>`.
/// Returns `(status, body)`.
async fn serve(state: &Arc<AppState>, host: &str) -> (StatusCode, String) {
    let app = fauna_nest::build_router(state.clone());
    let req = Request::builder()
        .uri("/.well-known/mta-sts.txt")
        .header("host", host)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// Floor primary ⇒ the served body advertises `mode: testing` (the stored
/// `enforce` downgraded against the floor MX) over the shared `mail.<primary>`.
#[tokio::test]
async fn floor_primary_serves_testing() {
    let state = nest(Some(common::floor_resolver(PRIMARY, MX_SNI)));
    seed_primary(&state, PRIMARY, "enforce").await;

    let (status, body) = serve(&state, &format!("mta-sts.{PRIMARY}")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "primary host serves a policy: {body}"
    );
    assert!(
        body.contains("mode: testing") && !body.contains("mode: enforce"),
        "a floor MX must downgrade the stored enforce → testing: {body}"
    );
    assert!(
        body.contains(&format!("mx: {MX_SNI}")),
        "shared MX target: {body}"
    );
    assert!(body.contains("version: STSv1"), "{body}");
    assert!(body.contains("max_age: 86400"), "default max_age: {body}");
}

/// Trusted primary ⇒ the stored `enforce` stands.
#[tokio::test]
async fn trusted_primary_serves_enforce() {
    let state = nest(Some(common::trusted_resolver(PRIMARY, MX_SNI)));
    seed_primary(&state, PRIMARY, "enforce").await;

    let (status, body) = serve(&state, &format!("mta-sts.{PRIMARY}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("mode: enforce"),
        "a trusted covering MX advertises the stored enforce: {body}"
    );
}

/// An **additional** (non-primary) domain serves its own stored mode, but over
/// the **shared** `mail.<primary>` MX target — and the coupling reads the
/// primary's MX cert (floor here), so a stored `enforce` on the additional domain
/// still downgrades to `testing`. Proves per-domain matching + one-MX coupling.
#[tokio::test]
async fn additional_domain_serves_per_domain_mode_over_shared_mx() {
    let state = nest(Some(common::floor_resolver(PRIMARY, MX_SNI)));
    seed_primary(&state, PRIMARY, "testing").await;
    state
        .db
        .add_mail_domain(ADDITIONAL, false, "enforce", "expand_primary", None, None)
        .await
        .unwrap();

    let (status, body) = serve(&state, &format!("mta-sts.{ADDITIONAL}")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "additional host serves a policy: {body}"
    );
    assert!(
        body.contains(&format!("mx: {MX_SNI}")) && !body.contains("mx: mail.community.test"),
        "every domain advertises the one shared mail.<primary> MX: {body}"
    );
    assert!(
        body.contains("mode: testing"),
        "the additional domain's enforce couples to the primary's floor MX: {body}"
    );
}

/// A stored `testing` stays `testing` on a trusted MX (the mode is a ceiling;
/// cert reality never upgrades it — only the nest's advance does).
#[tokio::test]
async fn stored_testing_stays_testing_on_a_trusted_mx() {
    let state = nest(Some(common::trusted_resolver(PRIMARY, MX_SNI)));
    seed_primary(&state, PRIMARY, "testing").await;

    let (status, body) = serve(&state, &format!("mta-sts.{PRIMARY}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("mode: testing") && !body.contains("mode: enforce"),
        "a trusted certificate alone never raises the mode: {body}"
    );
}

/// There is no `none` on our own side: a row that somehow holds it (no writer
/// produces one) still publishes a policy, as `testing`.
#[tokio::test]
async fn stored_none_is_served_as_testing() {
    let state = nest(Some(common::trusted_resolver(PRIMARY, MX_SNI)));
    seed_primary(&state, PRIMARY, "none").await;

    let (status, body) = serve(&state, &format!("mta-sts.{PRIMARY}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("mode: testing") && !body.contains("mode: none"),
        "a domain this nest serves mail for always publishes a policy: {body}"
    );
}

/// **The witness for the nest's own advance** (`mail-multidomain.md` § The
/// advance): a domain added over the wire — the request names no mode — is
/// stored and served `testing`; once the mail name is on a trusted certificate
/// and the 7-day window has passed, the nest stores `enforce` with no human
/// action, and both `fauna.bridges.list_local_domains` and the served policy
/// file say so.
///
/// **How the clock moves.** The nest has no clock seam and none is added for
/// this: the pass is called through its `now`-injectable core
/// `advance_mta_sts_modes_at(state, now_ms)` — the same function the hourly
/// maintenance tick calls with the wall clock — with `now_ms` set past the
/// window. Everything else is the real thing: the add and the list go through
/// the registered RPC handlers, the policy through `build_router`, the trust
/// reading through a real `MultiDomainCertResolver` serving a real leaf. A
/// binary nest cannot show this — the tier_3 e2e nest serves plain HTTP with no
/// certificate (never trusted), and the binary takes no clock knob.
#[tokio::test]
async fn nest_advances_an_added_domain_to_enforce() {
    let state = nest(Some(common::trusted_resolver(PRIMARY, MX_SNI)));
    let admin = common::admin_actor(&state).await;
    let router = {
        let mut b = fauna_nest::rpc_router::RpcRouter::builder();
        fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers(&mut b);
        b.build()
    };

    // Add over the wire: no mode on the request.
    let added: AddLocalDomainReply = common::call(
        &router,
        &state,
        admin,
        "fauna.bridges.add_local_domain",
        &AddLocalDomainRequest {
            domain: PRIMARY.into(),
            mta_sts_cert_mode: "expand_primary".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(added.domain.mta_sts_mode, "testing");
    let host = format!("mta-sts.{PRIMARY}");
    let (_, body) = serve(&state, &host).await;
    assert!(body.contains("mode: testing"), "fresh domain: {body}");

    // Inside the window: trusted, but nothing moves.
    let added_at = added.domain.added_at;
    let early = added_at + MTA_STS_TESTING_WINDOW_MS - 1;
    assert!(advance_mta_sts_modes_at(&state, early).await.is_empty());
    let (_, body) = serve(&state, &host).await;
    assert!(body.contains("mode: testing"), "inside the window: {body}");

    // Past the window: the nest advances the stored mode by itself.
    let due = added_at + MTA_STS_TESTING_WINDOW_MS;
    assert_eq!(
        advance_mta_sts_modes_at(&state, due).await,
        vec![PRIMARY.to_string()]
    );

    let listed: ListLocalDomainsReply = common::call(
        &router,
        &state,
        admin,
        "fauna.bridges.list_local_domains",
        &ListLocalDomainsRequest {},
    )
    .await
    .unwrap();
    assert_eq!(listed.active.len(), 1);
    assert_eq!(listed.active[0].mta_sts_mode, "enforce");

    let (status, body) = serve(&state, &host).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("mode: enforce"),
        "the advanced domain is published enforce on the trusted MX: {body}"
    );
}

/// The same add on a nest behind the self-signed floor never advances, however
/// long it waits — and keeps publishing `testing`.
#[tokio::test]
async fn nest_never_advances_on_the_floor() {
    let state = nest(Some(common::floor_resolver(PRIMARY, MX_SNI)));
    seed_primary(&state, PRIMARY, "testing").await;
    let row = state
        .db
        .lookup_active_mail_domain(PRIMARY)
        .await
        .unwrap()
        .unwrap();

    let late = row.added_at + 10 * MTA_STS_TESTING_WINDOW_MS;
    assert!(advance_mta_sts_modes_at(&state, late).await.is_empty());
    let (_, body) = serve(&state, &format!("mta-sts.{PRIMARY}")).await;
    assert!(body.contains("mode: testing"), "{body}");
}

/// A bare host (no `mta-sts.` prefix) and a host carrying a `:port` both resolve
/// to the same `mail_domains` row.
#[tokio::test]
async fn bare_host_and_port_suffix_resolve() {
    let state = nest(Some(common::trusted_resolver(PRIMARY, MX_SNI)));
    seed_primary(&state, PRIMARY, "enforce").await;

    let (status_bare, body_bare) = serve(&state, PRIMARY).await;
    assert_eq!(status_bare, StatusCode::OK, "{body_bare}");
    assert!(body_bare.contains("mode: enforce"), "{body_bare}");

    let (status_port, body_port) = serve(&state, &format!("mta-sts.{PRIMARY}:443")).await;
    assert_eq!(status_port, StatusCode::OK, "{body_port}");
    assert!(body_port.contains("mode: enforce"), "{body_port}");
}

/// A host that is not an active local domain ⇒ 404 (peer treats MTA-STS as
/// unavailable).
#[tokio::test]
async fn unknown_host_404s() {
    let state = nest(Some(common::floor_resolver(PRIMARY, MX_SNI)));
    seed_primary(&state, PRIMARY, "enforce").await;

    let (status, _) = serve(&state, "mta-sts.not-a-local-domain.test").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A plain-HTTP nest (`served_cert_spki == None`) reads as on-floor for the MX →
/// never advertises enforce (the dev/tier_3 e2e reality).
#[tokio::test]
async fn no_resolver_never_enforces() {
    let state = nest(None);
    seed_primary(&state, PRIMARY, "enforce").await;

    let (status, body) = serve(&state, &format!("mta-sts.{PRIMARY}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("mode: testing"),
        "no served cert reads as floor → never enforce: {body}"
    );
}
