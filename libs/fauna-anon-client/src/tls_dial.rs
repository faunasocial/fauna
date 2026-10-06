//! The one shared **trusted WS dial** — the client-side § Cross-connection
//! binding policy (`docs/goal/architecture/security.md` § Transport trust) as a
//! single implementation instead of a per-caller copy.
//!
//! Every *bearer-carrying* native WS dial follows the same three-step policy:
//!
//! 1. Over `wss://`, if a prior handshake graduated a bound SPKI for this
//!    authority ([`crate::pinned_spki`]), require the served leaf to present
//!    exactly that SPKI ([`crate::tls_verify::spki_pinned_client_config`]) — a
//!    MITM cannot complete TLS as a cert whose key it lacks.
//! 2. Otherwise fall through to strict WebPKI: a public-CA nest is
//!    authenticated the boring way, and a non-WebPKI cert with no pin is
//!    **rejected**, never accept-any (what retires `FAUNA_INSECURE_TLS`).
//! 3. ([`dial_ws_trusted_with_graduate_retry`] only.) A failed `wss://` dial
//!    with no pin runs one pre-identity `fauna.auth.nest_handshake` graduation
//!    ([`crate::AnonymousNestClient::graduate_transport_trust`]) and retries
//!    once with the SPKI it cached — the § Pin custody "consumer's dial"
//!    fallback, also the heal for an identity-holding process whose SPKI has
//!    yet to be (re-)graduated. The graduation **never mints a pin**: it
//!    verifies a pin the interactive path already holds (or a DNS `self=`
//!    root) and refuses otherwise, whatever store the process installed.
//!
//! Consumers: `fauna-client::ws_adapter` (the bearer WS-RPC channel),
//! [`crate::ws`]'s authenticated connect (the leaf-crate bearer twin). A
//! since-removed data-plane caller once shipped for weeks dialing
//! plain WebPKI because this policy lived only as per-caller copies, so that
//! caller simply never got it. One implementation is the fix that keeps a
//! new caller from repeating that.
//!
//! The *anonymous* (pre-identity) dial is deliberately NOT a consumer: it uses
//! the capturing verifier (provisional accept + capture) because it is the
//! connection the channel binding authenticates in-band — see
//! [`crate::ws`]'s `connect_anonymous`.

use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::handshake::client::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// The stream every trusted dial yields (tungstenite over an optionally
/// TLS-wrapped TCP socket).
pub type TrustedWsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The process-default rustls `CryptoProvider` install, once — canonical home
/// [`fauna_ws_substrate`]; re-exported so every trusted-dial consumer reaches
/// it beside the dial itself.
pub use fauna_ws_substrate::ensure_tls_provider;

fn is_wss(request: &Request) -> bool {
    request.uri().scheme_str() == Some("wss")
}

/// The graduated cross-connection pin for this request's authority, if any.
/// Only meaningful over TLS — a plain `ws://` dev nest has no cert to pin, so
/// non-`wss` requests always resolve `None`.
pub fn resolve_graduated_pin(request: &Request) -> Option<[u8; 32]> {
    if !is_wss(request) {
        return None;
    }
    let authority = request.uri().authority()?.as_str();
    crate::pinned_spki(&crate::trust::authority_of(authority))
}

/// `http::Request<()>` is not `Clone`; rebuild an identical request so the
/// graduate-retry arm can dial twice from one caller-built request.
fn clone_request(request: &Request) -> Request {
    let mut cloned = Request::builder()
        .method(request.method().clone())
        .uri(request.uri().clone())
        .version(request.version())
        .body(())
        .expect("re-building an already-valid request cannot fail");
    *cloned.headers_mut() = request.headers().clone();
    cloned
}

/// One dial under the graduated-pin policy (steps 1–2 above; no retry):
/// SPKI-pinned when `pin` is `Some`, strict WebPKI (or plain `ws://`)
/// otherwise. No deadline is imposed here — callers own their connect bounds.
///
/// `Box::pin`: the deep TLS + WS-upgrade handshake future goes on the heap so
/// the enclosing future stays small — the native UniFFI apps poll connects
/// on small foreign-executor stacks (see `connect_anonymous`'s comment and
/// `apps/native-async-execution.md`).
// result_large_err: the error is tungstenite's own (the same shape its
// `connect_async` returns); each of the three consumers maps it to its own
// error type immediately, so boxing would only force a deref-remap at every
// call site (the `bins/fauna-nest` crate-level rationale, applied narrowly).
#[allow(clippy::result_large_err)]
pub async fn dial_ws_trusted(
    request: Request,
    ws_config: WebSocketConfig,
    pin: Option<[u8; 32]>,
) -> Result<(TrustedWsStream, Response), tokio_tungstenite::tungstenite::Error> {
    ensure_tls_provider();
    // The process-wide dial budget (`fauna_ws_substrate::dial_budget`): every
    // bearer dial to a nest waits its turn here, and a `429` it earns holds the
    // gate for every other dial to that nest.
    let budget_key = fauna_ws_substrate::dial_budget::nest_key(request.uri());
    fauna_ws_substrate::dial_budget::acquire(&budget_key).await;
    let dialed = if let Some(pin) = pin {
        let (config, _capture) = crate::tls_verify::spki_pinned_client_config(pin);
        let connector = tokio_tungstenite::Connector::Rustls(config);
        Box::pin(tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(ws_config),
            false,
            Some(connector),
        ))
        .await
    } else {
        // `wss://` → strict WebPKI; `ws://` → plain (tungstenite `uri_mode`
        // dispatches on the URL scheme).
        Box::pin(tokio_tungstenite::connect_async_with_config(
            request,
            Some(ws_config),
            false,
        ))
        .await
    };
    if let Err(e) = &dialed {
        fauna_ws_substrate::dial_budget::note_dial_error(&budget_key, e);
    }
    dialed
}

/// The full bearer-dial policy (steps 1–3): resolve the graduated pin, dial,
/// and on a failed `wss://` dial with **no** pin, run one pre-identity
/// graduation and retry once with the SPKI it cached.
///
/// In a process that holds a bearer but no identity key — the apple File
/// Provider extension, a background agent (security.md § Pin custody) —
/// NOTHING ever runs the signed handshake that would graduate a SPKI, so a
/// self-signed nest fails strict WebPKI here forever without the fallback; the
/// graduation verifies the pin the interactive path holds for the host and
/// **never mints one** ([`crate::PinMinting::Never`]), whatever store this
/// process installed. That is not only the consumer's shape: a public-CA
/// nest's bearer mint pins nothing (the WebPKI waiver), so an interactive app
/// or the sync agent reaches this arm with no pin on ANY dial error against such a
/// nest — an on-path box that makes the first dial fail, serves its own
/// self-signed cert and answers the handshake with its own key used to get
/// itself TOFU-minted here and then be re-dialed with the real
/// bearer. With no pin and no `self=` root the
/// graduation refuses, no retry is sent and the original error surfaces; an
/// unreachable nest fails the anonymous connect just as fast as the dial did.
#[allow(clippy::result_large_err)] // same rationale as `dial_ws_trusted`
pub async fn dial_ws_trusted_with_graduate_retry(
    request: Request,
    ws_config: WebSocketConfig,
) -> Result<(TrustedWsStream, Response), tokio_tungstenite::tungstenite::Error> {
    let pin = resolve_graduated_pin(&request);
    let retry_arm = is_wss(&request) && pin.is_none();
    let retry_request = retry_arm.then(|| clone_request(&request));
    match dial_ws_trusted(request, ws_config, pin).await {
        Ok(ok) => Ok(ok),
        Err(first_err) => {
            let Some(retry_request) = retry_request else {
                return Err(first_err);
            };
            // Graduation wants the nest's HTTPS base; the retry arm only runs
            // for `wss://`, whose authority is the nest's.
            let authority = retry_request
                .uri()
                .authority()
                .map(|a| a.as_str().to_string());
            let Some(authority) = authority else {
                return Err(first_err);
            };
            let nest_url = format!("https://{authority}");
            if crate::AnonymousNestClient::graduate_transport_trust(&nest_url)
                .await
                .is_ok()
                && let Some(pin) = crate::pinned_spki(&crate::trust::authority_of(&authority))
            {
                dial_ws_trusted(retry_request, ws_config, Some(pin)).await
            } else {
                Err(first_err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[test]
    fn a_plain_ws_request_never_resolves_a_pin() {
        // Seed a pin for the host; a `ws://` request to it must not pick it up
        // (nothing to pin without TLS).
        let request: Request = "ws://pin-less.example:81/api/v1/ws"
            .into_client_request()
            .unwrap();
        assert_eq!(resolve_graduated_pin(&request), None);
    }

    #[test]
    fn clone_request_preserves_uri_and_headers() {
        let mut request: Request = "wss://nest.example:8443/api/v1/ws?device_id=aa"
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "Authorization",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static("Bearer tok"),
        );
        let cloned = clone_request(&request);
        assert_eq!(cloned.uri(), request.uri());
        assert_eq!(cloned.method(), request.method());
        assert_eq!(cloned.headers(), request.headers());
    }
}
