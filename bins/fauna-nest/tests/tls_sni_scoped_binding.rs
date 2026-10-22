//! tier_3 — the **SNI-scoped channel binding** over the real
//! `fauna_nest::serve_tls` listener behind the real `MultiDomainCertResolver`,
//! serving **two different certs**: the domain cert a dial **by name** is
//! served and the § B-IP bridge cert every **no-SNI** dial is served
//! (`nest/tls-certificates.md` § B-IP *Lifetime* — the state every freshly
//! provisioned box is in for up to 160 h). Every binding producer must sign the
//! SPKI of the cert *this connection* was served (`transport-trust.md` § Two
//! independent axes → Axis 1), which the nest reads from its own TLS layer: the
//! served SNI rides the connection into the handler (`served_name.rs`), and
//! the binding read is `served_cert_spki_sha256(sni)` for a named dial, the
//! no-SNI read for an IP dial.
//!
//! Before the fix every producer signed `cert_for_sni(None)` — the bridge — so a
//! native app dialling a fresh box by name refused the binding (`served SPKI
//! differs from received cert`) for the whole bridge window. The dial-by-name
//! tests below are red on that code and green on the scoped read; the dial-by-IP
//! test pins the no-SNI fallback.
//!
//! A real client dials by name through `connect_resolving` (the production
//! wizard's own reach override: the socket target is the loopback listener,
//! while SNI, `Host` and the demanded cert stay the name's), so the nest's TLS
//! layer sees exactly what a name dial of a provisioned box presents.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fauna_anon_client::AnonymousNestClient;
use fauna_core::identity::ActorKeypair;
use fauna_nest::acme::{
    MultiDomainCertResolver, ServedCertSpki, load_certified_key_from_pem, spki_sha256_of_cert_der,
};
use fauna_nest::db::CacheDb;
use fauna_nest::federation_channel::{
    FEDERATION_SUBPROTOCOL, FederationHelloReply, HELLO_KIND, build_hello,
};
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::auth::{HandshakeReply, HandshakeRequest, handshake_signed_message};
use fauna_protocol::dispatcher::RpcDispatcher;
use fauna_ws_substrate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, TungsteniteAdapter};

/// The name the domain cert covers — an RFC 2606 reserved TLD, so it can never
/// resolve: only the reach override reaches the listener.
const NAMED_HOST: &str = "nest-sni-scoped.test";
/// The nest's deployment identity (its `nest_signing_key`); the client holds
/// its public half a priori, as a provisioning client does.
const NEST_SEED: [u8; 32] = [5u8; 32];

struct TwoCertNest {
    addr: SocketAddr,
    /// SPKI of the domain cert — what a dial by name is served.
    named_spki: [u8; 32],
    /// SPKI of the IP bridge cert — what a no-SNI dial is served.
    bridge_spki: [u8; 32],
    /// The deployment identity's public key, the id every binding names.
    nest_actor_id: [u8; 32],
    /// `state.nest_identity`'s public key (hex) — the federation leg signs with
    /// *that* key, not the deployment key (`federation_channel::serve_listener`).
    listener_nest_id: String,
    db: Arc<CacheDb>,
}

/// Spin a real nest over `serve_tls` behind a real `MultiDomainCertResolver`
/// holding the domain cert as its default and a bridge cert (another key, an IP
/// SAN) installed with `set_ip_cert`. The resolver is both the acceptor's cert
/// source and the state's `served_cert_spki`, exactly as `main.rs` wires it.
async fn start_two_cert_nest() -> TwoCertNest {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let named =
        rcgen::generate_simple_self_signed(vec![NAMED_HOST.to_string()]).expect("domain cert");
    let named_spki = spki_sha256_of_cert_der(named.cert.der().as_ref()).expect("domain SPKI");
    let named_key = load_certified_key_from_pem(
        named.cert.pem().as_bytes(),
        named.key_pair.serialize_pem().as_bytes(),
    )
    .expect("domain certified key");

    let bridge =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).expect("bridge cert");
    let bridge_spki = spki_sha256_of_cert_der(bridge.cert.der().as_ref()).expect("bridge SPKI");
    let bridge_key = load_certified_key_from_pem(
        bridge.cert.pem().as_bytes(),
        bridge.key_pair.serialize_pem().as_bytes(),
    )
    .expect("bridge certified key");
    assert_ne!(
        named_spki, bridge_spki,
        "two distinct keys, or the test proves nothing"
    );

    let resolver = Arc::new(MultiDomainCertResolver::new(named_key));
    resolver.set_ip_cert(bridge_key);
    // The resolver's own two reads, pinned so a regression there cannot be
    // mistaken for one in the binding producers.
    assert_eq!(
        resolver.served_cert_spki_sha256(NAMED_HOST),
        Some(named_spki),
        "a named SNI is served the domain cert"
    );
    assert_eq!(
        resolver.current_spki_sha256(),
        Some(bridge_spki),
        "the no-SNI read is the bridge while it is installed"
    );

    let nest_signing_key = ed25519_dalek::SigningKey::from_bytes(&NEST_SEED);
    let nest_actor_id = nest_signing_key.verifying_key().to_bytes();
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            b.build()
        }),
        nest_signing_key: Some(nest_signing_key),
        served_cert_spki: Some(Arc::clone(&resolver) as Arc<dyn ServedCertSpki>),
        ..AppState::for_test(db.clone())
    });
    let listener_nest_id = hex::encode(state.nest_identity.public_key_bytes());
    let app = fauna_nest::build_router(state);

    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));

    let (listener, addr) = common::nest_listener().await;
    let addr: SocketAddr = addr.parse().expect("nest_listener returns ip:port");
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        app.into_make_service(),
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));
    TwoCertNest {
        addr,
        named_spki,
        bridge_spki,
        nest_actor_id,
        listener_nest_id,
        db,
    }
}

fn named_url(nest: &TwoCertNest) -> String {
    format!("https://{NAMED_HOST}:{}", nest.addr.port())
}

/// Dial the nest **by name** (SNI = `NAMED_HOST`, socket = the listener) and
/// prove the TLS layer served the domain cert, so a binding over anything else
/// is the nest's fault, not the dial's.
async fn connect_by_name(nest: &TwoCertNest) -> AnonymousNestClient {
    let client = AnonymousNestClient::connect_resolving(&named_url(nest), Some(nest.addr))
        .await
        .expect("a dial by name reaches the two-cert nest through the reach override");
    assert_eq!(
        client.captured_cert().spki,
        Some(nest.named_spki),
        "a dial by name is served the domain cert"
    );
    client
}

async fn admit(db: &CacheDb, kp: &ActorKeypair) {
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .expect("admit the actor");
}

/// `build_identity_binding` (`fauna.auth.nest_handshake`, the login-binding
/// read every native sign-in starts with): a dial by name gets a binding over
/// the domain cert it received, so the SPKI-compared read succeeds.
#[tokio::test]
async fn identity_binding_names_the_cert_a_dial_by_name_received() {
    let nest = start_two_cert_nest().await;
    let client = connect_by_name(&nest).await;
    let id = fauna_anon_client::read_login_binding(&client)
        .await
        .expect("the identity binding is over the cert this connection received");
    assert_eq!(id, nest.nest_actor_id);
}

/// `build_cert_binding` on the bearer-minting `fauna.auth.handshake`: the
/// client signs over the identity it holds a priori and verifies the reply's
/// binding against the SPKI it received for the name.
#[tokio::test]
async fn handshake_binding_names_the_cert_a_dial_by_name_received() {
    let nest = start_two_cert_nest().await;
    let kp = ActorKeypair::generate();
    admit(&nest.db, &kp).await;
    let client = connect_by_name(&nest).await;

    let nonce = fauna_anon_client::fresh_nonce();
    let timestamp = fauna_core::data::Timestamp::now_millis();
    let actor_id = kp.actor_id().0;
    let msg = handshake_signed_message(&actor_id, timestamp, &nest.nest_actor_id, &nonce);
    let signature = ed25519_dalek::Signer::sign(kp.signing_key(), &msg);
    let reply: HandshakeReply = client
        .request(
            "fauna.auth.handshake",
            HandshakeRequest {
                actor_id: fauna_core::hex32::encode(&actor_id),
                timestamp,
                signature: hex::encode(signature.to_bytes()),
                client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
                nest_id: fauna_core::hex32::encode(&nest.nest_actor_id),
                extra: Default::default(),
            },
        )
        .await
        .expect("the bound handshake mints");
    let binding = reply.cert_binding.expect("a TLS nest serves a binding");
    fauna_anon_client::verify_cert_binding(
        &nest.named_spki,
        &nonce,
        &binding,
        Some(&nest.nest_actor_id),
    )
    .expect("the handshake binding is over the domain cert this connection received");
}

/// `build_cert_binding` on the verify path (`BindingKind::VerifyFold`, the
/// silent-challenge mint): the fold `challenge_nonce ‖ client_nonce` is signed
/// over the domain cert a dial by name received.
#[tokio::test]
async fn verify_fold_binding_names_the_cert_a_dial_by_name_received() {
    let nest = start_two_cert_nest().await;
    let kp = ActorKeypair::generate();
    admit(&nest.db, &kp).await;
    let client = connect_by_name(&nest).await;

    let client_nonce = fauna_anon_client::fresh_nonce();
    let (reply, challenge_nonce) = fauna_protocol::auth::challenge_verify(
        &client,
        kp.signing_key(),
        Some(&client_nonce),
        &nest.nest_actor_id,
    )
    .await
    .unwrap_or_else(|e| panic!("the silent challenge mints: {e:?}"));
    let binding = reply.cert_binding.expect("a TLS nest serves a binding");
    let mut fold = challenge_nonce.to_vec();
    fold.extend_from_slice(&client_nonce);
    fauna_anon_client::verify_cert_binding(
        &nest.named_spki,
        &fold,
        &binding,
        Some(&nest.nest_actor_id),
    )
    .expect("the verify-path binding is over the domain cert this connection received");
}

/// The no-SNI fallback: a dial by IP (RFC 6066 forbids a literal address in
/// SNI) is served the bridge and gets a binding over the bridge — today's read,
/// kept for exactly this dial.
#[tokio::test]
async fn a_dial_by_ip_keeps_the_bridge_binding() {
    let nest = start_two_cert_nest().await;
    let client = AnonymousNestClient::connect(&format!("https://{}", nest.addr))
        .await
        .expect("a dial by IP reaches the two-cert nest");
    assert_eq!(
        client.captured_cert().spki,
        Some(nest.bridge_spki),
        "a no-SNI dial is served the bridge cert"
    );
    let id = fauna_anon_client::read_login_binding(&client)
        .await
        .expect("the identity binding is over the bridge this connection received");
    assert_eq!(id, nest.nest_actor_id);
}

/// The nest↔nest leg: a peer dials the listener **by name** and carries the
/// SPKI it observed in `fauna.federation.hello`; the listener compares it with
/// the cert it served *that* connection — the domain cert, not the bridge.
#[tokio::test]
async fn federation_hello_by_name_binds_the_domain_cert() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    let nest = start_two_cert_nest().await;

    // A hand-rolled dial by name: the production dialer resolves the URL host
    // itself, which a `.test` name never does, so the TLS stream is built on a
    // loopback socket with the name as SNI — what a peer's dial of the box's
    // domain presents.
    fauna_ws_substrate::ensure_tls_provider();
    let (tls_config, capture) = fauna_ws_substrate::capturing_client_config();
    let connector = tokio_rustls::TlsConnector::from(tls_config);
    let tcp = tokio::net::TcpStream::connect(nest.addr)
        .await
        .expect("tcp");
    let tls = connector
        .connect(
            rustls::pki_types::ServerName::try_from(NAMED_HOST).unwrap(),
            tcp,
        )
        .await
        .expect("TLS handshake by name");
    let observed_spki = capture
        .lock()
        .ok()
        .and_then(|c| c.spki)
        .expect("the capturing verifier saw a cert");
    assert_eq!(
        observed_spki, nest.named_spki,
        "a dial by name is served the domain cert"
    );

    let mut request = format!(
        "wss://{NAMED_HOST}:{}/api/v1/federation/ws",
        nest.addr.port()
    )
    .into_client_request()
    .expect("ws url");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(FEDERATION_SUBPROTOCOL),
    );
    let (ws, _resp) = tokio_tungstenite::client_async_with_config(
        request,
        tls,
        Some(fauna_ws_substrate::rpc_ws_config()),
    )
    .await
    .expect("federation WS upgrade over TLS");
    let adapter = TungsteniteAdapter::new(ws, KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT);
    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    tokio::spawn(driver);

    let initiator_sk = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    let initiator_nest_id = hex::encode(initiator_sk.verifying_key().to_bytes());
    let hello = build_hello(
        &initiator_sk,
        &initiator_nest_id,
        &nest.listener_nest_id,
        &fauna_core::identity::random_hex(32),
        &hex::encode(observed_spki),
    )
    .expect("hello builds");
    let reply: FederationHelloReply = dispatcher
        .request_typed(
            HELLO_KIND,
            [1u8; 16],
            hello,
            Duration::from_secs(10),
            tokio::time::sleep(Duration::from_secs(10)),
        )
        .await
        .unwrap_or_else(|e| panic!("the listener binds the cert it served this name dial: {e:?}"));
    assert_eq!(reply.listener_nest_id, nest.listener_nest_id);
}
