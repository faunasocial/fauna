//! End-to-end proof that the residual HTTP content-API reqwest leg enforces the
//! WS handshake's pinned SPKI — the security property that retires
//! `FAUNA_INSECURE_TLS` (security.md § Cross-connection binding).
//!
//! A real `reqwest::Client` built with `use_preconfigured_tls(<pinning config>)`
//! is driven against a genuine self-signed TLS server. We assert all three arms
//! of the verifier through reqwest itself (not just the verifier unit tests):
//!
//! 1. **no pin** (`RequireWebPki`) → the self-signed cert is **refused** (this is
//!    exactly what the old accept-any `FAUNA_INSECURE_TLS` path allowed),
//! 2. **wrong pin** → refused,
//! 3. **matching pin** → accepted, the request completes (self-signed nests stay
//!    usable once the WS handshake graduates the pin).
//!
//! The verifier is the same one the production reqwest leg uses
//! (`fauna_anon_client::tls_verify::dynamic_pinned_client_config`, which
//! `trust::store_pinned_reqwest_tls` wraps with `move || pinned_spki(authority)`);
//! the explicit resolvers here keep the test isolated from the process-global pin
//! store.

mod common;

use std::sync::Arc;

use fauna_anon_client::tls_verify::{NoPinPolicy, dynamic_pinned_client_config};

/// The shared self-signed fixture server answering every request with `200 ok`.
async fn spawn_self_signed_server() -> (u16, [u8; 32]) {
    common::spawn_self_signed_server(|_head| {
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec()
    })
    .await
}

/// Build a reqwest client whose TLS verification is our per-handshake pinning
/// verifier — exactly how `AuthClient::new` / the linux+windows
/// `build_http_client` wire it in production.
fn pinned_client(resolver: fauna_anon_client::tls_verify::PinResolver) -> reqwest::Client {
    reqwest::Client::builder()
        .use_preconfigured_tls(dynamic_pinned_client_config(
            resolver,
            NoPinPolicy::RequireWebPki,
        ))
        .build()
        .expect("reqwest client builds with the pinning TLS config")
}

#[tokio::test]
async fn no_pin_refuses_self_signed_cert() {
    let (port, _spki) = spawn_self_signed_server().await;
    // `RequireWebPki` with no graduated pin: a self-signed cert is the MITM-open
    // case the old `FAUNA_INSECURE_TLS` accepted — it must now be refused.
    let client = pinned_client(Arc::new(|| None));
    let res = client
        .get(format!("https://127.0.0.1:{port}/"))
        .send()
        .await;
    assert!(
        res.is_err(),
        "an un-pinned self-signed cert must be refused (no accept-any)"
    );
}

#[tokio::test]
async fn wrong_pin_refuses_connection() {
    let (port, _spki) = spawn_self_signed_server().await;
    let client = pinned_client(Arc::new(|| Some([0xAAu8; 32])));
    let res = client
        .get(format!("https://127.0.0.1:{port}/"))
        .send()
        .await;
    assert!(res.is_err(), "a cert whose SPKI != the pin must be refused");
}

#[tokio::test]
async fn matching_pin_accepts_and_completes() {
    let (port, spki) = spawn_self_signed_server().await;
    // The pin the WS handshake would have graduated for this host: the SPKI of the
    // cert the server actually serves. The request must now complete.
    let client = pinned_client(Arc::new(move || Some(spki)));
    let resp = client
        .get(format!("https://127.0.0.1:{port}/"))
        .send()
        .await
        .expect("matching pinned SPKI lets the request through");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn accept_provisional_reaches_self_signed_cert() {
    // The OTHER no-pin policy. The onboarding handle-check nest-health PROBE
    // (`OnboardingMachine::nest_probe_client`) and the pre-identity
    // anonymous WS connection (`fauna_anon_client::ws::connect_anonymous`) both
    // use `NoPinPolicy::AcceptProvisional`: a self-signed *local* nest (bare IP /
    // `localhost` / `.local` floor, whose SANs never cover the reached address)
    // must be REACHABLE. The probe is a pre-auth reachability check; real auth is
    // the downstream `fauna.auth` channel-binding ceremony. Without this, the
    // strict-WebPKI `http` client mis-reports `RegisteredNoNest` for every
    // reachable bare-IP nest (the `test@10.1.8.51` regression). This is the
    // complement of `no_pin_refuses_self_signed_cert` (the bearer-carrying path,
    // which must instead refuse).
    let (port, _spki) = spawn_self_signed_server().await;
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(dynamic_pinned_client_config(
            Arc::new(|| None),
            NoPinPolicy::AcceptProvisional,
        ))
        .build()
        .expect("reqwest client builds with the AcceptProvisional TLS config");
    let resp = client
        .get(format!("https://127.0.0.1:{port}/"))
        .send()
        .await
        .expect(
            "AcceptProvisional must reach a self-signed nest (the bare-IP onboarding probe path)",
        );
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn a_re_pin_bites_on_the_next_request_even_when_the_server_offers_resumption() {
    // A TLS 1.3 session resumed from a cached ticket never runs the client's
    // certificate verifier. With rustls' default client-side session cache, a
    // re-pin of the host to a NEW identity (a replaced box) would therefore not
    // bite until the cached session aged out: the process keeps talking to the
    // OLD box on every reqwest leg for as long as its ticket lives, the pin store
    // consulted only on full handshakes. The pinned configs never resume
    // (security.md § Cross-connection binding → *No session resumption on a
    // pinned config*), so against a server that OFFERS resumption the request
    // after a re-pin is a full handshake and is refused. The default fixture
    // disables resumption server-side and so could not see this.
    let (port, spki) =
        common::spawn_self_signed_server_with(common::ServerSessions::Resumable, |_head| {
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec()
        })
        .await;
    let pin = Arc::new(std::sync::Mutex::new(Some(spki)));
    let client = pinned_client(Arc::new({
        let pin = Arc::clone(&pin);
        move || *pin.lock().unwrap()
    }));
    let url = format!("https://127.0.0.1:{port}/");

    // Full handshake under the matching pin; the server hands out a session.
    let resp = client
        .get(&url)
        .send()
        .await
        .expect("matching pinned SPKI: the first request completes");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");

    // The host is re-pinned to a different identity. `Connection: close` means
    // the next request opens a new TLS connection — the one the client would
    // resume from its cache if it cached sessions.
    *pin.lock().unwrap() = Some([0xBBu8; 32]);
    let res = client.get(&url).send().await;
    assert!(
        res.is_err(),
        "a re-pinned host must refuse the old cert on the very next request, \
         whether or not the server offers session resumption"
    );
}
