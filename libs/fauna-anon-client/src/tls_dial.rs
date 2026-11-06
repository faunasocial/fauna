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
//!    with no pin — or a pinned dial the verifier refused because the served
//!    leaf's SPKI is not the pinned one (a **stale pin**: the nest's served
//!    cert moved under a client that pinned the old one) — runs one
//!    pre-identity `fauna.auth.nest_handshake` graduation
//!    ([`crate::AnonymousNestClient::graduate_transport_trust`]) and retries
//!    once under the pin that graduation left — the § Pin custody "consumer's
//!    dial" fallback, also the heal for an identity-holding process whose SPKI
//!    has yet to be (re-)graduated, and the re-bind § Cross-connection binding
//!    rule 3 promises on a served-cert rotation. The graduation **never mints
//!    a pin**: it verifies a pin the interactive path already holds (or a DNS
//!    `self=` root) and refuses otherwise, whatever store the process
//!    installed — so a moved SPKI is accepted only when the pinned nest
//!    identity signs it over the fresh connection, in every process kind, and
//!    a box that cannot sign as that identity is refused — with the typed
//!    identity verdict ([`TrustedDialError::IdentityChanged`]) when that is
//!    what the graduation reached, else the original dial error — and never
//!    sees the bearer.
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
    dial_ws_trusted_observed(request, ws_config, pin).await.0
}

/// [`dial_ws_trusted`], also returning the SPKI the served leaf presented on
/// the last TLS handshake a pinned dial ran (`None` when no pin was given —
/// tungstenite's own connector captures nothing — or when the dial failed
/// before any certificate reached the verifier). The graduate-retry arm reads
/// it to tell a **stale pin** (the nest served a leaf whose SPKI is not the
/// pinned one) from every other failure a pinned dial can meet.
#[allow(clippy::result_large_err)] // same rationale as `dial_ws_trusted`
async fn dial_ws_trusted_observed(
    request: Request,
    ws_config: WebSocketConfig,
    pin: Option<[u8; 32]>,
) -> (
    Result<(TrustedWsStream, Response), tokio_tungstenite::tungstenite::Error>,
    Option<[u8; 32]>,
) {
    ensure_tls_provider();
    // The process-wide dial budget (`fauna_ws_substrate::dial_budget`): every
    // bearer dial to a nest waits its turn here, and a `429` it earns holds the
    // gate for every other dial to that nest. Taken once for the domain dial
    // and its hint fallback together: they are one logical dial of one nest.
    let budget_key = fauna_ws_substrate::dial_budget::nest_key(request.uri());
    fauna_ws_substrate::dial_budget::acquire(&budget_key).await;
    // The reach hint's dial rule, session side (`fauna_ws_substrate::reach_hint`):
    // the domain first, always; the hint only when the domain failed to reach
    // anything; and a domain success spends the hint. The hint request is the
    // same request — SNI, `Host`, the demanded cert and the pin all stay the
    // domain's, so a stale hint at a recycled address is refused by the pin
    // exactly as on the launch's hint dial.
    let host = fauna_ws_substrate::reach_hint::host_key_of_uri(request.uri());
    let hint = fauna_ws_substrate::reach_hint::hint_for(&host);
    let hint_request = hint.map(|_| clone_request(&request));
    let (dialed, served) = dial_ws_once(request, ws_config, pin, None).await;
    let (dialed, served) = match (dialed, hint, hint_request) {
        (Err(domain_err), Some(addr), Some(hint_request))
            if fauna_ws_substrate::reach_hint::is_reachability_failure(&domain_err) =>
        {
            tracing::info!(
                host,
                %addr,
                error = %domain_err,
                "the domain did not reach the nest; dialling its reach hint"
            );
            dial_ws_once(hint_request, ws_config, pin, Some(addr)).await
        }
        (Ok(ok), Some(_), _) => {
            fauna_ws_substrate::reach_hint::domain_reached(&host);
            (Ok(ok), served)
        }
        (other, _, _) => (other, served),
    };
    if let Err(e) = &dialed {
        fauna_ws_substrate::dial_budget::note_dial_error(&budget_key, e);
    }
    (dialed, served)
}

/// One socket under the trusted policy: SPKI-pinned when `pin` is `Some`,
/// strict WebPKI (or plain `ws://`) otherwise; over a TCP socket to `via` when
/// given, else resolving the request's own host — the request is untouched
/// either way, so the name every trust decision keys on is the request's.
///
/// The second value is the served leaf's SPKI as the pinned verifier captured
/// it on this socket's handshake (`None` without a pin, or before any
/// certificate arrived) — written by the verifier whether it then accepted or
/// refused the leaf, so a refusal still says which SPKI was met.
#[allow(clippy::result_large_err)] // same rationale as `dial_ws_trusted`
async fn dial_ws_once(
    request: Request,
    ws_config: WebSocketConfig,
    pin: Option<[u8; 32]>,
    via: Option<std::net::SocketAddr>,
) -> (
    Result<(TrustedWsStream, Response), tokio_tungstenite::tungstenite::Error>,
    Option<[u8; 32]>,
) {
    // A pinned config, or `None` for tungstenite's own strict-WebPKI connector
    // (`connect_async_with_config` builds exactly that one; `ws://` wraps the
    // socket plain whichever is passed — `tungstenite::client::uri_mode`).
    let (connector, capture) = match pin {
        Some(pin) => {
            let (config, capture) = crate::tls_verify::spki_pinned_client_config(pin);
            (
                Some(tokio_tungstenite::Connector::Rustls(config)),
                Some(capture),
            )
        }
        None => (None, None),
    };
    let dialed = if let Some(addr) = via {
        match TcpStream::connect(addr).await {
            Ok(tcp) => {
                Box::pin(tokio_tungstenite::client_async_tls_with_config(
                    request,
                    tcp,
                    Some(ws_config),
                    connector,
                ))
                .await
            }
            Err(e) => Err(e.into()),
        }
    } else {
        Box::pin(tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(ws_config),
            false,
            connector,
        ))
        .await
    };
    let served = capture.and_then(|c| c.lock().unwrap().spki);
    (dialed, served)
}

/// Why [`dial_ws_trusted_with_graduate_retry`] failed: tungstenite's own
/// error, or the **identity verdict** its re-bind reached.
///
/// The second arm exists because the first cannot carry it: a dial refused
/// for a moved served SPKI runs the never-minting re-bind, and when the pinned
/// nest identity does not sign the new leaf that refusal is the `known_hosts`
/// verdict (`transport-trust.md` § Post-auth surfacing), not a transport
/// fault. Every consumer maps it to its own identity-changed error — the
/// bearer channel's `NestClientError::NestIdentityChanged`, which stops the
/// reconnect supervisor — exactly as each already maps a `401` or `426` out
/// of [`Self::Ws`].
#[derive(Debug)]
pub enum TrustedDialError {
    /// The dial's own failure (TLS, TCP, an HTTP upgrade refusal, …).
    Ws(tokio_tungstenite::tungstenite::Error),
    /// The served cert moved and the pinned identity did not sign it (or a
    /// pinned nest proved no identity at all): the pin stays, no retry was
    /// sent, and the bearer never left this process.
    IdentityChanged {
        /// The authority the pin is keyed by ([`crate::trust::authority_of`]).
        host: String,
        /// What was pinned against what the box proved.
        verdict: crate::trust::IdentityChangedVerdict,
    },
}

impl std::fmt::Display for TrustedDialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ws(e) => write!(f, "{e}"),
            Self::IdentityChanged { host, .. } => {
                write!(
                    f,
                    "the nest identity pinned for {host} did not sign the served cert"
                )
            }
        }
    }
}

impl std::error::Error for TrustedDialError {}

impl From<tokio_tungstenite::tungstenite::Error> for TrustedDialError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Ws(e)
    }
}

/// The full bearer-dial policy (steps 1–3): resolve the graduated pin, dial,
/// and on a failed `wss://` dial with **no** pin — or one the pinned verifier
/// refused because the served SPKI is not the pinned one — run one
/// pre-identity graduation and retry once under the pin it left.
///
/// **The stale-pin arm (2026-11-05).** The SPKI pin is a
/// per-process cache of the leaf a binding last proved, and the nest's served
/// leaf moves legitimately — the floor's first ACME cert, an IP bridge cert
/// installed or lapsing, a floor-key rotation — so a pinned dial refused for
/// a *different* served SPKI is the re-bind `transport-trust.md` § Cross-
/// connection binding rule 3 promises, not a stop: the same never-minting
/// graduation runs, and the retry goes out only if it left a pin other than
/// the one the refused dial used — the new SPKI the pinned identity signed
/// over the fresh connection, or none at all when the served cert graduated
/// WebPKI-valid with no root held (the clearing rule, same section). A box
/// whose binding another identity signed leaves the pin untouched, so no
/// retry is sent and the dial fails with that verdict typed
/// ([`TrustedDialError::IdentityChanged`]). The arm keys on the SPKI
/// the verifier captured, never on the error's text: a pinned dial that
/// failed before any certificate arrived, or met the pinned leaf and failed
/// later, is not a stale pin and runs no graduation. Before this arm a held
/// pin disarmed the retry outright, and a sync agent whose nest re-keyed its
/// served cert refused every bearer dial for the life of the process.
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
) -> Result<(TrustedWsStream, Response), TrustedDialError> {
    let pin = resolve_graduated_pin(&request);
    let retry_request = is_wss(&request).then(|| clone_request(&request));
    let (dialed, served) = dial_ws_trusted_observed(request, ws_config, pin).await;
    match dialed {
        Ok(ok) => Ok(ok),
        Err(first_err) => {
            let Some(retry_request) = retry_request else {
                return Err(first_err.into());
            };
            // No pin: the consumer's-dial fallback. A pin the served leaf
            // did not match: the stale-pin heal. Anything else a pinned dial
            // met is not the pin's doing.
            let stale_pin = match (pin, served) {
                (None, _) => false,
                (Some(pin), Some(served)) => served != pin,
                (Some(_), None) => return Err(first_err.into()),
            };
            if pin.is_some() && !stale_pin {
                return Err(first_err.into());
            }
            // Graduation wants the nest's HTTPS base; the retry arm only runs
            // for `wss://`, whose authority is the nest's.
            let authority = retry_request
                .uri()
                .authority()
                .map(|a| a.as_str().to_string());
            let Some(authority) = authority else {
                return Err(first_err.into());
            };
            let nest_url = format!("https://{authority}");
            if stale_pin {
                tracing::info!(
                    authority,
                    "the served cert's SPKI is not the pinned one; re-binding against the pinned nest identity"
                );
            }
            let host = crate::trust::authority_of(&authority);
            let graduated = crate::AnonymousNestClient::graduate_transport_trust(&nest_url).await;
            // The graduation's refusal, when it IS the identity verdict (the
            // pinned identity did not sign this box's binding, or a pinned nest
            // proved none), leaves the dial typed: flattened into the dial's own
            // TLS error it reached the reconnect supervisor as a retryable
            // transport fault, and the supervisor redialled a box the pin
            // refused for the life of the process (`transport-trust.md`
            // § Post-auth surfacing, the plumbing rule). Nothing else changes:
            // the pin stays, no retry is sent and the bearer never rides.
            if let Err(crate::AnonClientError::Trust(trust_err)) = &graduated
                && let Some(verdict) = crate::trust::classify_identity_changed(trust_err, &host)
            {
                // Logged here, at the classification, like the bearer mint's
                // (`fauna_client::ws_challenge_bearer::map_anon_err`): one
                // record of what was pinned against what was seen.
                tracing::error!(
                    "[identity] nest identity changed for {host} (pinned {}, seen {:?}, fork \
                     evidence: {}) — the served cert moved and the pinned identity did not sign \
                     it; refusing the dial, the bearer stays here",
                    verdict.pinned_hex,
                    verdict.seen_hex,
                    verdict.fork
                );
                return Err(TrustedDialError::IdentityChanged { host, verdict });
            }
            let healed = crate::pinned_spki(&host);
            if graduated.is_ok() && healed != pin {
                dial_ws_trusted(retry_request, ws_config, healed)
                    .await
                    .map_err(TrustedDialError::from)
            } else {
                Err(first_err.into())
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
