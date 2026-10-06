//! tier_3 — the self-signed-TLS channel binding end-to-end against a **real nest
//! binary serving HTTPS with a self-signed cert** (no public CA), over the real
//! `fauna_nest::serve_tls` TLS listener and the real `fauna_client` connect path
//! (the capturing verifier + `WsChallengeBearer` + cross-connection SPKI pin).
//! This is what S3 step B (e) exists to prove (tracked internally;
//! security.md § Transport trust): a genuine self-signed nest authenticates
//! and connects, while a
//! substituted cert is rejected **before any bearer is trusted** — the case the
//! deleted `FAUNA_INSECURE_TLS=1 → accept any cert` path silently allowed.
//!
//! "Substituted cert" is staged without a live MITM proxy: the nest signs an SPKI
//! that differs from the cert it actually serves (`served_cert_spki` ≠ the served
//! leaf). That is exactly the wire state a middlebox produces — the nest's
//! signature is over a different SPKI than the client received — so the client's
//! `verify_cert_binding` must reject it with `SpkiMismatch`.
//!
//! Also covers `AnonymousNestClient::connect_resolving`'s DNS override (the
//! production wizard's claim connect reaching a freshly-provisioned box by its
//! captured IP before its DNS record has propagated): the same `start_tls`
//! harness proves the override reaches the real self-signed nest even when the
//! URL's hostname can never resolve via system DNS.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::FixedSpki;
use fauna_client::NestClient;
use fauna_client::ws_challenge_bearer::WsChallengeBearer;
use fauna_core::identity::ActorKeypair;
use fauna_nest::acme::spki_sha256_of_cert_der;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest_http::BearerSource;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// Spin a real nest serving HTTPS with a fresh self-signed cert. The nest signs
/// `signed_spki` in the channel binding; pass the served leaf's real SPKI for a
/// genuine nest, or any other value to stage a substituted cert. Returns the
/// `https://` base URL.
/// Returns the base URL **and** the nest's db: an actor must be admitted before it
/// can reach the channel-binding leg at all (a handshake no longer auto-provisions).
async fn start_tls(signed_spki: impl FnOnce([u8; 32]) -> [u8; 32]) -> (String, Arc<CacheDb>) {
    start_tls_with_router(signed_spki, |b| {
        fauna_nest::auth_handlers::register_auth_handlers(b)
    })
    .await
}

/// Give `kp` a real `users` row. Without this the auth path short-circuits on
/// `not_registered` and never evaluates the cert binding — which would turn the
/// substituted-cert test into a false pass (it would "reject" for the wrong reason).
async fn admit(db: &CacheDb, kp: &ActorKeypair) {
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
}

/// As [`start_tls`], with a caller-chosen handler registration — lets a test
/// stage a nest (or a MITM) that refuses `fauna.auth.nest_handshake` alongside
/// the current one.
async fn start_tls_with_router(
    signed_spki: impl FnOnce([u8; 32]) -> [u8; 32],
    register: impl FnOnce(&mut fauna_nest::rpc_router::RpcRouterBuilder),
) -> (String, Arc<CacheDb>) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Self-signed leaf for `localhost` (SAN is irrelevant — the client's
    // capturing verifier provisionally accepts any cert and authenticates via
    // the in-band binding, not WebPKI).
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let real_spki = spki_sha256_of_cert_der(cert_der.as_ref()).expect("served leaf SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

    // The nest's stable Ed25519 deployment identity that signs the binding.
    let nest_signing_key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            register(&mut b);
            b.build()
        }),
        nest_signing_key: Some(nest_signing_key),
        served_cert_spki: Some(Arc::new(FixedSpki(signed_spki(real_spki)))),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);

    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("server tls config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));

    let (listener, addr) = common::nest_listener().await;
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        app.into_make_service(),
        // Loopback client → bounded by the loopback ceiling (1024), so any admin cap works.
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));
    (format!("https://{addr}"), db)
}

/// Genuine self-signed nest: the binding verifies, the bearer mints, the
/// identity is TOFU-pinned, and the authenticated WS connects with the
/// cross-connection SPKI pin — all over `wss://` with no public CA and no
/// `FAUNA_INSECURE_TLS`.
#[tokio::test]
async fn genuine_self_signed_nest_authenticates_and_connects() {
    // Nest signs the SPKI of the cert it actually serves.
    let (base, db) = start_tls(|real| real).await;
    let authority = fauna_anon_client::authority_of(&base);
    let kp = ActorKeypair::generate();
    admit(&db, &kp).await;

    // Bearer mint over the bound handshake succeeds (binding verifies).
    let bearer = WsChallengeBearer::new(base.clone(), kp.actor_id().0, kp.signing_key().clone());
    bearer
        .bearer()
        .await
        .expect("genuine self-signed nest: channel binding verifies, bearer mints over wss");
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_sha256_of_cert_der_for(&base).await),
        "the bound SPKI is pinned for the bearer connection",
    );

    // End-to-end: NestClient mints over the bound WS, then opens the
    // authenticated WS — which requires the pinned SPKI (cross-connection
    // binding) — and reaches Connected.
    let nest = NestClient::new(base, kp);
    nest.connect().await.expect("connect over self-signed wss");
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
    .expect("client reached Connected over the self-signed-TLS channel binding");
}

/// The direct handshake's own mint (`fauna_anon_client::mint_bearer_over_handshake`,
/// the tests/scripts/M2M ceremony) over a genuine self-signed nest: the shared
/// login-binding reader learns the nest's identity **SPKI-compared** against
/// the cert this connection received (`login.md` § Binding the nest), the
/// nest-bound handshake mints, and the served SPKI is pinned. Since the nest
/// verifies only the bound form, the green mint is the witness that the
/// native reader bound the right identity.
#[tokio::test]
async fn the_bound_handshake_mints_over_a_genuine_self_signed_nest() {
    let (base, db) = start_tls(|real| real).await;
    let authority = fauna_anon_client::authority_of(&base);
    let kp = ActorKeypair::generate();
    admit(&db, &kp).await;
    let minted =
        fauna_anon_client::mint_bearer_over_handshake(&base, kp.actor_id().0, kp.signing_key())
            .await
            .expect("the nest-bound handshake mints over wss");
    assert!(!minted.token.is_empty());
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_sha256_of_cert_der_for(&base).await),
        "the bound SPKI is pinned for the bearer connection",
    );
}

/// A substituted cert is caught at the very first leg now — the login-binding
/// read — before any login signature exists: the nest's identity binding names
/// an SPKI this connection did not receive, so no login is signed over it.
#[tokio::test]
async fn substituted_cert_is_rejected_before_any_login_is_signed() {
    let (base, db) = start_tls(|_real| [0xAAu8; 32]).await;
    let kp = ActorKeypair::generate();
    admit(&db, &kp).await;
    let Err(err) =
        fauna_anon_client::mint_bearer_over_handshake(&base, kp.actor_id().0, kp.signing_key())
            .await
    else {
        panic!("a substituted cert must refuse before the login is signed")
    };
    assert!(
        matches!(
            err,
            fauna_anon_client::AnonClientError::Trust(fauna_anon_client::TrustError::Binding(
                fauna_anon_client::BindingError::SpkiMismatch
            ))
        ),
        "the identity read refuses on the SPKI compare: {err}"
    );
}

/// Substituted cert: the nest signs an SPKI that differs from the leaf it serves
/// (the wire state a MITM produces). The client must reject the binding with
/// `SpkiMismatch` and refuse to mint/trust the bearer — never connecting.
#[tokio::test]
async fn substituted_cert_is_rejected_before_bearer() {
    // Nest signs a DIFFERENT SPKI than the cert it serves.
    let (base, db) = start_tls(|_real| [0xAAu8; 32]).await;
    let kp = ActorKeypair::generate();
    admit(&db, &kp).await;

    let bearer = WsChallengeBearer::new(base, kp.actor_id().0, kp.signing_key().clone());
    let err = bearer
        .bearer()
        .await
        .expect_err("a substituted cert must be rejected before the bearer is trusted");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("channel binding") || msg.to_lowercase().contains("spki"),
        "error should name the channel-binding/SPKI failure, got: {msg}"
    );
}

/// Pre-identity nest-identity handshake (Track 2 — deployment-seed first-contact
/// trust; design tracked internally):
/// `fauna.auth.nest_handshake` returns a channel binding **without any registered
/// actor** (no claim has happened — the DB is empty), and the client graduates it
/// against a pre-resolved identity root — the deployment seed it injected at
/// provision. A wrong expected root (a box presenting a different identity than
/// the one we provisioned) must hard-fail.
#[tokio::test]
async fn nest_handshake_serves_binding_pre_claim_and_graduates_against_injected_root() {
    use fauna_protocol::auth::{NEST_HANDSHAKE_KIND, NestHandshakeReply, NestHandshakeRequest};

    let (base, _db) = start_tls(|real| real).await;
    let client = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("anon connect");

    let nonce = fauna_anon_client::fresh_nonce();
    let reply: NestHandshakeReply = client
        .request(
            NEST_HANDSHAKE_KIND,
            NestHandshakeRequest {
                client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .expect("nest_handshake answers on the anonymous connection, no actor registered");
    let binding = reply
        .cert_binding
        .expect("a TLS nest with a deployment key serves a binding");

    // The root the provisioning client holds a priori: the deployment seed it
    // injected ([5u8; 32] is `start_tls`'s nest identity).
    let injected_root = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32])
        .verifying_key()
        .to_bytes();
    let captured = client.captured_cert();
    fauna_anon_client::graduate_handshake_with_root(
        "nest-handshake-genuine.test",
        &captured,
        &nonce,
        Some(&binding),
        Some(injected_root),
    )
    .expect("first-contact graduation verifies the box against the injected-seed root");

    // A box presenting an identity OTHER than the seed we injected is MITM/bug —
    // hard-fail, nothing pinned (security.md § Connection-teardown rule).
    let wrong_root = ed25519_dalek::SigningKey::from_bytes(&[6u8; 32])
        .verifying_key()
        .to_bytes();
    let err = fauna_anon_client::graduate_handshake_with_root(
        "nest-handshake-wrong-root.test",
        &captured,
        &nonce,
        Some(&binding),
        Some(wrong_root),
    )
    .expect_err("a mismatched injected root must hard-fail");
    assert_eq!(
        err,
        fauna_anon_client::TrustError::Binding(fauna_anon_client::BindingError::IdentityMismatch)
    );
    assert_eq!(
        fauna_anon_client::pinned_spki("nest-handshake-wrong-root.test"),
        None,
        "a failed graduation must pin nothing"
    );
}

/// **Item 1c — the cross-nest byte plane over a SELF-SIGNED home nest.** A
/// cross-nest folder member dials the set's home nest's HTTPS byte plane
/// directly, holding **no** account there, so no ordinary handshake ever
/// graduates a pin for it — and `store_pinned_reqwest_tls`'s `RequireWebPki`
/// floor refuses a self-signed home (the gap the cross-nest agent capstone
/// flagged and deliberately left plain-HTTP rather than paper over). This proves
/// the fix end-to-end at the byte plane: the pre-identity `fauna.auth.nest_handshake`
/// graduated against the grant-delivered `home_nest_actor_id` (`PreResolved`)
/// pins the SPKI, after which the SAME reqwest client — built exactly as the
/// sync agent's `build_http_client` builds it (`store_pinned_reqwest_tls`) —
/// completes an HTTPS request the un-graduated client is refused. The agent's
/// `graduate_home_nest_pin` is the hex-decode wrapper over the
/// `graduate_first_contact(Some(root))` proven here (its decode guard is unit-
/// pinned in `fauna-sync-agent`'s `bridge` tests). `security.md` § Transport
/// trust, the federation-granted Axis-2 row.
#[tokio::test]
async fn cross_nest_byte_plane_reqwest_accepts_a_self_signed_home_only_after_graduation() {
    let (base, _db) = start_tls(|real| real).await;
    let authority = fauna_anon_client::authority_of(&base);
    // The home nest's deployment identity the grant carries — `start_tls`'s nest
    // signs with `[5u8; 32]`, so its `nest_actor_id` is that seed's public key.
    let home_nest_actor_id = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32])
        .verifying_key()
        .to_bytes();

    // The byte-plane reqwest client, built exactly as the agent's `build_http_client`:
    // per-handshake SPKI pinning with a `RequireWebPki` floor.
    let byte_plane = reqwest::Client::builder()
        .use_preconfigured_tls(fauna_anon_client::store_pinned_reqwest_tls(&base))
        .timeout(Duration::from_secs(10))
        .build()
        .expect("byte-plane reqwest client builds with the pinning TLS config");

    // RED: no pin yet → `RequireWebPki` refuses the self-signed home (the exact
    // state the capstone hit — a self-signed home byte plane simply does not work).
    assert!(
        fauna_anon_client::pinned_spki(&authority).is_none(),
        "no pin should exist for the home nest before graduation"
    );
    let refused = byte_plane.get(format!("{base}/")).send().await;
    assert!(
        refused.is_err(),
        "an un-graduated self-signed home byte plane must be refused, got {refused:?}"
    );

    // Graduate the pin from the grant-delivered identity — the pre-identity
    // handshake the agent runs before dialing the byte plane.
    let anon = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("anon connect to the self-signed home nest");
    anon.graduate_first_contact(&base, Some(home_nest_actor_id))
        .await
        .expect("graduate the home-nest SPKI pin against the grant-delivered identity");
    assert_eq!(
        fauna_anon_client::pinned_spki(&authority),
        Some(spki_sha256_of_cert_der_for(&base).await),
        "graduation pins the served SPKI for the byte-plane authority",
    );

    // GREEN: the SAME reqwest client now completes a request over the self-signed
    // home — the pin resolver is read per-handshake, so no rebuild is needed.
    let accepted = byte_plane.get(format!("{base}/")).send().await;
    assert!(
        accepted.is_ok(),
        "after graduation the self-signed home byte plane is accepted, got {accepted:?}"
    );
}

/// The substituted-cert (MITM) wire state over the pre-identity handshake: the
/// nest's binding signs a different SPKI than the leaf the client received —
/// graduation must reject with `SpkiMismatch` even when the identity root
/// matches (the MITM relays the genuine nest's identity but terminates TLS with
/// its own cert).
#[tokio::test]
async fn nest_handshake_substituted_cert_is_rejected() {
    use fauna_protocol::auth::{NEST_HANDSHAKE_KIND, NestHandshakeReply, NestHandshakeRequest};

    let (base, _db) = start_tls(|_real| [0xAAu8; 32]).await;
    let client = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("anon connect");

    let nonce = fauna_anon_client::fresh_nonce();
    let reply: NestHandshakeReply = client
        .request(
            NEST_HANDSHAKE_KIND,
            NestHandshakeRequest {
                client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .expect("the substituted-SPKI nest still answers; rejection is client-side");
    let binding = reply.cert_binding.expect("binding present");

    let injected_root = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32])
        .verifying_key()
        .to_bytes();
    let err = fauna_anon_client::graduate_handshake_with_root(
        "nest-handshake-substituted.test",
        &client.captured_cert(),
        &nonce,
        Some(&binding),
        Some(injected_root),
    )
    .expect_err("a binding over a different SPKI than received must be rejected");
    assert_eq!(
        err,
        fauna_anon_client::TrustError::Binding(fauna_anon_client::BindingError::SpkiMismatch)
    );
}

/// The production entrypoint the onboarding `WsNestApi` calls on every fresh
/// pre-identity connection: `graduate_first_contact` runs the nest-identity
/// handshake and graduates in one step. Genuine box + matching injected root →
/// `Graduated`; mismatched root → hard `Trust` error; a nest with no
/// `nest_handshake` route → the same hard failure (below).
#[tokio::test]
async fn graduate_first_contact_verifies_matching_root_and_rejects_mismatch() {
    use fauna_anon_client::FirstContactOutcome;

    let (base, _db) = start_tls(|real| real).await;
    let injected_root = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32])
        .verifying_key()
        .to_bytes();

    let client = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("anon connect");
    assert_eq!(
        client
            .graduate_first_contact(&base, Some(injected_root))
            .await
            .expect("matching injected root graduates"),
        FirstContactOutcome::Graduated
    );

    // A different expected root (a box presenting an identity we did not
    // provision) hard-fails — on a fresh connection, before anything is sent.
    let wrong_root = ed25519_dalek::SigningKey::from_bytes(&[6u8; 32])
        .verifying_key()
        .to_bytes();
    let client2 = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("anon connect");
    match client2
        .graduate_first_contact(&base, Some(wrong_root))
        .await
    {
        Err(fauna_anon_client::AnonClientError::Trust(fauna_anon_client::TrustError::Binding(
            fauna_anon_client::BindingError::IdentityMismatch,
        ))) => {}
        other => panic!("expected Trust(Binding(IdentityMismatch)), got {other:?}"),
    }
}

/// A nest that does not route `fauna.auth.nest_handshake` hard-fails first
/// contact on the DNS/TOFU ladder too (`expected_root = None`). Flipped
/// 2026-09-24 from `graduate_first_contact_skips_on_a_legacy_nest_without_injected_root`,
/// which asserted the pre-Track-2 ungraduated fallback the compat-remnant
/// sweep removed (`version-compatibility.md` § Dimension 2) — with it went
/// the MITM-mimics-the-rejection residual that fallback carried.
#[tokio::test]
async fn graduate_first_contact_hard_fails_a_kind_rejection_without_injected_root() {
    // A nest with NO handlers at all — `nest_handshake` is allowlisted by the
    // dispatcher gate but unrouted, the honest stand-in for a box refusing
    // the kind.
    let (base, _db) = start_tls_with_router(|real| real, |_b| {}).await;
    let client = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("anon connect");
    match client.graduate_first_contact(&base, None).await {
        Err(fauna_anon_client::AnonClientError::Trust(
            fauna_anon_client::TrustError::BindingRequired,
        )) => {}
        other => panic!("expected Trust(BindingRequired) hard-fail, got {other:?}"),
    }
}

/// With an injected root held, the client KNOWS the box it just provisioned
/// answers the kind — a rejection is a first-contact MITM's cheapest move
/// (terminate TLS, refuse the kind, ride the downgrade), so it must hard-fail
/// with nothing sent, never silently proceed ungraduated.
#[tokio::test]
async fn graduate_first_contact_hard_fails_a_kind_rejection_with_injected_root() {
    let (base, _db) = start_tls_with_router(|real| real, |_b| {}).await;
    let injected_root = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32])
        .verifying_key()
        .to_bytes();

    let client = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("anon connect");
    match client
        .graduate_first_contact(&base, Some(injected_root))
        .await
    {
        Err(fauna_anon_client::AnonClientError::Trust(
            fauna_anon_client::TrustError::BindingRequired,
        )) => {}
        other => panic!("expected Trust(BindingRequired) hard-fail, got {other:?}"),
    }
}

/// …and a pinned self-signed host is no exception: the rejection hard-fails
/// there too. Flipped 2026-09-24 from
/// `graduate_first_contact_keeps_the_fallback_for_a_pinned_self_signed_legacy_nest`: with the fallback gone the predicate has one arm.
#[tokio::test]
async fn graduate_first_contact_hard_fails_a_kind_rejection_on_a_pinned_self_signed_host() {
    let (base, _db) = start_tls_with_router(|real| real, |_b| {}).await;
    // A TOFU pin for this authority, as a prior bearer handshake with this
    // self-signed host left behind.
    fauna_anon_client::trust::pin_identity_for_test(&base, [0x5au8; 32]);
    assert!(
        fauna_anon_client::pinned_identity(&fauna_anon_client::authority_of(&base)).is_some(),
        "the pin must be in the installed store for this test to mean anything"
    );

    let client = fauna_anon_client::AnonymousNestClient::connect(&base)
        .await
        .expect("anon connect");
    match client.graduate_first_contact(&base, None).await {
        Err(fauna_anon_client::AnonClientError::Trust(
            fauna_anon_client::TrustError::BindingRequired,
        )) => {}
        other => panic!("expected Trust(BindingRequired) hard-fail, got {other:?}"),
    }
}

/// Re-derive the served leaf SPKI for the genuine-case assertion by opening a
/// throwaway anonymous connection and reading the captured cert. Kept tiny so
/// the genuine test can assert the *exact* pinned value without threading the
/// cert out of `start_tls`.
async fn spki_sha256_of_cert_der_for(base: &str) -> [u8; 32] {
    let c = fauna_anon_client::AnonymousNestClient::connect(base)
        .await
        .expect("anon connect");
    c.captured_cert().spki.expect("captured served SPKI")
}

/// Reach-by-IP override (Track 1 — the production wizard's own claim connect):
/// `connect_resolving` dials a caller-supplied `SocketAddr` directly instead of
/// resolving the URL's host via system DNS, while SNI/`Host`/cert-identity still
/// come from the URL. Proven here with a hostname that can never resolve —
/// `connect` (no override) must fail, while `connect_resolving` reaches the
/// exact same real self-signed nest by IP. The capturing verifier doesn't check
/// hostname/WebPKI at all (it authenticates via in-band channel binding), so the
/// fake hostname in SNI/Host doesn't stop the handshake from completing.
#[tokio::test]
async fn resolve_override_reaches_the_box_despite_an_unresolvable_hostname() {
    let (base, _db) = start_tls(|real| real).await;
    let addr: std::net::SocketAddr = base
        .trim_start_matches("https://")
        .parse()
        .expect("start_tls returns https://<ip>:<port>");

    // RFC 2606 reserved TLD — guaranteed to never resolve via real DNS.
    let fake_url = format!(
        "https://nest-hetzner-test-does-not-resolve.invalid:{}",
        addr.port()
    );

    assert!(
        fauna_anon_client::AnonymousNestClient::connect(&fake_url)
            .await
            .is_err(),
        "an unresolvable hostname must fail without a resolve override"
    );

    let client = fauna_anon_client::AnonymousNestClient::connect_resolving(&fake_url, Some(addr))
        .await
        .expect("resolve override reaches the box directly by IP, bypassing DNS");
    assert!(
        client.captured_cert().spki.is_some(),
        "the override connection completed a real TLS handshake and captured a cert"
    );
}
