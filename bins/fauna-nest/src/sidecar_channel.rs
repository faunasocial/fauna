//! The internal nest↔sidecar WS-RPC channel.
//!
//! The **fourth nest WS connection class**, after the per-actor plane
//! (`crate::ws`), the anonymous pre-identity plane, and the nest↔nest federation
//! channel (`crate::federation_channel`). A co-located, sandboxed sidecar process
//! dials the nest over loopback and both ends speak WS-RPC over the one duplex.
//! Its one rider today is the iroh relay sidecar (`bins/fauna-iroh-relay`), at
//! `GET /internal/relay/ws`.
//!
//! **Auth model — one-way token handshake.** Unlike federation's mutual
//! sign-over-CID, the sidecar is a co-located process the nest minted a bearer
//! token for (`crate::sidecar_tokens`); the nest is the loopback **listener** the
//! sidecar dials. The sidecar proves possession of its token in the FIRST frame
//! (`fauna.sidecar.hello`), the nest verifies it against the `sidecar-token-*`
//! map and binds the channel to a `SidecarScope`. No counter-proof from the nest
//! (the sidecar trusts the loopback address it was told to dial).
//!
//! Design authority: `docs/goal/architecture/transport.md` § Future directions.
//!
//! This module is the **nest listener side**: the token handshake
//! ([`verify_sidecar_hello`]), the loopback-only upgrade gate
//! ([`internal_ws_allowlist_gate`], shared with the nest-link worker channel), and
//! the relay listener ([`relay_ws_handler`]) with its lean serve loop — the
//! relay-origin kinds (`fauna.relay.fetch_tls_cert`, `fauna.relay.admit`), the
//! nest-origin `fauna.relay.cert_changed` push, plus the channel-generic
//! log-plane kind. The dialer side is the shared [`fauna_sidecar_client`] crate,
//! which the relay binary and this module's tests both drive.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRequest;
use axum::extract::ws::WebSocket;
use axum::response::Response;
use tokio::sync::mpsc;

use fauna_protocol::log_plane::{
    KIND_SIDECAR_LOG_EVENTS, ReportLogEventsReply, ReportLogEventsRequest,
};
use fauna_protocol::relay::{
    HELLO_EXTRA_X25519, KIND_RELAY_ADMIT, KIND_RELAY_CERT_CHANGED, KIND_RELAY_FETCH_TLS_CERT,
    RelayAdmitReply, RelayAdmitRequest, RelayCertChangedReply, RelayCertChangedRequest,
    RelayFetchTlsCertReply,
};
use fauna_protocol::sidecar::{KIND_SIDECAR_HELLO, SidecarHello, SidecarHelloReply};
use fauna_protocol::{ByteBuf, Request, RpcDispatcher, RpcError, Value};

use crate::federation_channel::{WsMessageAdapter, send_error, to_value, value_to};
use crate::routes::AppState;
use crate::sidecar_tokens::SidecarScope;

/// How long the listener waits for the inbound `fauna.sidecar.hello` before
/// tearing the connection down (mirrors federation's `HANDSHAKE_DEADLINE`).
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

// ── Token handshake ────────────────────────────────────────────────────────────

/// Why a `fauna.sidecar.hello` is rejected. Any rejection tears the connection
/// down — none are recoverable in-band.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SidecarHelloError {
    #[error("unknown scope string in hello")]
    UnknownScope,
    #[error("declared scope does not match this listener's required scope")]
    ScopeMismatch,
    #[error("token is valid but does not grant the required scope")]
    ScopeNotGranted,
    #[error("unknown sidecar token")]
    UnknownToken,
}

/// Verify a `fauna.sidecar.hello` against the nest's sidecar-token map and the
/// scope this listener requires. Pure (no I/O, no `AppState`) so it is
/// unit-testable directly; the listener wraps it with the dispatcher reply.
///
/// Checks, in order: (1) the declared scope string parses; (2) it matches the
/// listener's `required_scope`; (3) the token is known AND grants that scope.
/// Returns the bound scope on success.
pub fn verify_sidecar_hello(
    hello: &SidecarHello,
    tokens: &HashMap<String, Vec<SidecarScope>>,
    required_scope: SidecarScope,
) -> Result<SidecarScope, SidecarHelloError> {
    let declared = SidecarScope::from_wire(&hello.scope).ok_or(SidecarHelloError::UnknownScope)?;
    if declared != required_scope {
        return Err(SidecarHelloError::ScopeMismatch);
    }
    match tokens.get(&hello.token) {
        Some(scopes) if scopes.contains(&required_scope) => Ok(required_scope),
        Some(_) => Err(SidecarHelloError::ScopeNotGranted),
        None => Err(SidecarHelloError::UnknownToken),
    }
}

/// Listener side: verify the inbound `fauna.sidecar.hello` and reply. On success
/// returns the bound scope **and the verified hello** (the relay listener reads
/// its `extra` X25519 attestation); on any failure replies `unauthenticated` (or
/// `malformed`) and returns `Err(())` so the caller tears the connection down.
/// Takes the token map + required scope explicitly (decoupled from `AppState`) so
/// it is unit-testable over an in-memory duplex, like federation's
/// `listener_handshake`.
async fn listener_handshake(
    dispatcher: &Arc<RpcDispatcher>,
    tokens: &HashMap<String, Vec<SidecarScope>>,
    required_scope: SidecarScope,
    req: Request,
) -> Result<(SidecarScope, SidecarHello), ()> {
    if req.kind != KIND_SIDECAR_HELLO {
        tracing::warn!(kind = %req.kind, "sidecar: first frame was not hello; rejecting");
        send_error(dispatcher, req.correlation_id, unauthenticated()).await;
        return Err(());
    }

    let hello: SidecarHello = match value_to(&req.payload) {
        Ok(h) => h,
        Err(()) => {
            send_error(dispatcher, req.correlation_id, malformed("hello payload")).await;
            return Err(());
        }
    };

    match verify_sidecar_hello(&hello, tokens, required_scope) {
        Ok(scope) => {
            let _ = fauna_peer_channel::send_reply_bounded(
                dispatcher,
                req.correlation_id,
                to_value(&SidecarHelloReply::default()),
                true,
            )
            .await;
            Ok((scope, hello))
        }
        Err(e) => {
            tracing::warn!(error = %e, "sidecar hello rejected");
            send_error(dispatcher, req.correlation_id, unauthenticated()).await;
            Err(())
        }
    }
}

// ── Listener: GET /internal/relay/ws (the iroh relay sidecar) ────────────────────

/// `GET /internal/relay/ws` — the iroh relay sidecar's WS connection class.
/// **Loopback-only** (mirrors `/internal/worker/ws`): the relay
/// is co-located and dials nest over loopback, so a non-loopback
/// source is rejected at upgrade time. **No bearer at upgrade**:
/// the connection authenticates at the L3 layer via the `fauna.sidecar.hello`
/// token handshake (the FIRST frame), bound to [`SidecarScope::Relay`], and is
/// driven by [`serve_relay_listener`] (the relay attests its X25519 in the hello
/// and fetches its sealed TLS cert).
pub async fn relay_ws_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
) -> Response {
    let ws = match sidecar_ws_upgrade(&state, req).await {
        Ok(ws) => ws,
        Err(resp) => return resp,
    };
    // Generation-scoped connection future — see the federation twin. A
    // sidecar channel is neither a `state.ws` client (no 1001-drain) nor plain
    // HTTP (no idle timeout), so nothing else at teardown reaches it.
    ws.on_upgrade(move |socket| {
        let scope = Arc::clone(&state);
        async move {
            let _ = scope
                .spawn_scoped(serve_relay_listener(state, socket))
                .await;
        }
    })
}

/// Shared loopback-only gate for the internal WS listeners: the
/// sidecar's `/internal/relay/ws`, and
/// [`crate::nest_link::proxy::worker_ws_handler`] — both explicitly document
/// reusing this same allowlist, and had each hand-copied the identical check
/// under their own name before this lift. `label` names the caller for the
/// log line and rejection body (`"sidecar"`/`"worker"`).
///
/// Loopback only (no configurable allowlist). ConnectInfo
/// is injected on BOTH production serving paths — TLS via `serve_tls`'s
/// `WithConnectInfo` middleware (`lib.rs`), plain-HTTP via
/// `into_make_service_with_connect_info` — and both these routes are on the
/// public listener (the IP gate, not the route's absence, is what protects
/// them). So a missing ConnectInfo is never an expected state; it can only
/// mean a future middleware-ordering change dropped it. Fail CLOSED (reject)
/// rather than open, so such a regression can never silently open an
/// internal channel to a non-loopback source.
pub(crate) fn internal_ws_allowlist_gate(
    req: &axum::extract::Request,
    label: &str,
) -> Result<(), Response> {
    use axum::response::IntoResponse;
    let Some(axum::extract::ConnectInfo(addr)) = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
    else {
        tracing::warn!(
            "{label} WS upgrade with no ConnectInfo (middleware regression?); rejecting"
        );
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            format!("{label} WS requires a known peer address"),
        )
            .into_response());
    };
    let ip = addr.ip();
    if !ip.is_loopback() {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            format!("{label} WS not allowed from this IP"),
        )
            .into_response());
    }
    Ok(())
}

/// Shared loopback-only gate ([`internal_ws_allowlist_gate`]) + WS
/// upgrade for the internal sidecar connection class (`/internal/relay/ws`).
/// Returns the size-bounded [`WebSocketUpgrade`] on
/// success, or the rejection `Response` (403 for a non-loopback IP, or the
/// upgrade-extractor's own error).
async fn sidecar_ws_upgrade(
    state: &Arc<AppState>,
    req: axum::extract::Request,
) -> Result<axum::extract::ws::WebSocketUpgrade, Response> {
    use axum::response::IntoResponse;
    internal_ws_allowlist_gate(&req, "sidecar")?;

    let ws = axum::extract::ws::WebSocketUpgrade::from_request(req, state)
        .await
        .map_err(|e| e.into_response())?;
    Ok(ws
        .max_message_size(crate::routes::MAX_WS_MESSAGE_SIZE)
        .max_frame_size(crate::routes::MAX_WS_MESSAGE_SIZE))
}

/// The relay sidecar's single binding ID in the sealed-cert AAD
/// (`(role, bridge_id, domain)` — `fauna_mls::wrapped_blob`). There is exactly
/// one iroh relay per nest, so a fixed id suffices (the bridge path needs
/// per-bridge ids only because several bridges share that channel). The relay
/// never sends it: the nest binds it on seal, and the relay derives the same AAD
/// from the blob's own index on unseal.
const RELAY_BRIDGE_ID: &str = "iroh-relay";

/// The `domain` component of the relay cert's seal AAD: `relay.<apex>` when the
/// nest has a configured domain, else a bare `"relay"` (a domainless/LAN nest
/// serves the loopback floor; the AAD `domain` is only a binding label — the
/// served cert's real SANs come from the sealed PEM on disk, not this string).
///
/// **Deliberately reads the `node.domain` seed, NOT the claim-refreshed
/// `handle_domain()`** — the one site in the nest where following the live
/// domain would be wrong. This string is baked into a seal *binding*: a cert
/// sealed before claim was bound to the pre-claim label, so switching the label
/// at claim would make it undecryptable. Every *other* runtime domain read
/// follows the claim (`crate::state::ActivityPubState` docs,
/// `domains-and-tls-bootstrap.md` § Claim); this one must not.
fn relay_seal_domain(state: &AppState) -> String {
    // seed-read-ok(boot-binding): the ONE deliberate exception in the tree. This
    // string is a seal AAD *binding*, not a name — a cert sealed before claim was
    // bound to the pre-claim label, so following the live domain would make it
    // undecryptable. The docstring above states it; `domains-and-tls-bootstrap.md`
    // § Claim names this site as the exception.
    match state.config.nest.domain.as_deref() {
        Some(d) if !d.trim().is_empty() => format!("relay.{}", d.trim()),
        _ => "relay".to_string(),
    }
}

/// Parse the relay's hex-encoded X25519 **public** key out of its
/// `fauna.sidecar.hello` `extra` map (key [`HELLO_EXTRA_X25519`]). `None` if the
/// key is absent, not a string, not valid hex, or not exactly 32 bytes.
fn relay_x25519_from_hello(hello: &SidecarHello) -> Option<[u8; 32]> {
    let Value::String(hex_str) = hello.extra.get(HELLO_EXTRA_X25519)? else {
        return None;
    };
    let bytes = hex::decode(hex_str).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// Drive one inbound **relay** sidecar channel (we are the listener at
/// `/internal/relay/ws`). Runs the `fauna.sidecar.hello` handshake bound to
/// [`SidecarScope::Relay`], reads + validates the relay's attested X25519 public
/// key from the hello `extra`, then serves the one relay-origin kind
/// (`fauna.relay.fetch_tls_cert`): each fetch seals the on-disk `relay.<apex>`
/// cert to that connection-local X25519 and returns it.
///
/// The X25519 is captured from THIS connection's hello — never an `AppState`
/// slot, never a peer advertisement — so a cert can only ever be sealed to the
/// key the token-authenticated relay proved it holds (HPKE is the
/// proof-of-possession: only the matching secret opens the blob). A lean loop
/// (no idempotency cache, no router) keeps the channel's allowlist in one
/// `match`: a relay token can only reach this route (scope-bound at the
/// handshake) and only the kinds named there.
async fn serve_relay_listener(state: Arc<AppState>, socket: WebSocket) {
    let adapter = WsMessageAdapter::with_heartbeat(socket, state.ws.heartbeat());
    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    let dispatcher = Arc::new(dispatcher);
    // Generation-scoped — see the federation twin. The handle is
    // deliberately NOT retained: this listener never aborts its driver. A
    // handshake rejection *enqueues* its `unauthenticated` reply (`send_reply`
    // only puts it on `out_tx`), so aborting on the next line raced that frame
    // and usually won — the dialer then saw its oneshot dropped and read
    // `fauna.protocol.disconnected`, i.e. a refused credential arriving as "the
    // socket went away". Dropping the last dispatcher handle instead closes
    // `out_tx`, and the driver drains what is queued before exiting on its own
    // (`RpcDispatcher::new`; `transport.md` § Layers).
    state.spawn_scoped(driver);

    if let Some(inbound) = dispatcher.inbound_requests() {
        serve_relay_channel(&state, &dispatcher, inbound).await;
    }
    // `dispatcher` drops here — the driver drains and exits.
}

/// The relay listener's channel logic, decoupled from the WS transport setup
/// (handshake + serve loop) so it is unit-testable over an in-memory duplex.
/// Runs the Relay-bound
/// `fauna.sidecar.hello` handshake, captures the attested X25519, then serves
/// `fauna.relay.fetch_tls_cert` until the channel closes.
async fn serve_relay_channel(
    state: &Arc<AppState>,
    dispatcher: &Arc<RpcDispatcher>,
    mut inbound: mpsc::Receiver<Request>,
) {
    // First frame: the token handshake bound to Relay; pull the attested X25519.
    let relay_x25519 = match tokio::time::timeout(HANDSHAKE_DEADLINE, inbound.recv()).await {
        Ok(Some(req)) => {
            match listener_handshake(dispatcher, &state.sidecar_tokens, SidecarScope::Relay, req)
                .await
            {
                Ok((_scope, hello)) => match relay_x25519_from_hello(&hello) {
                    Some(pk) => pk,
                    None => {
                        tracing::warn!(
                            "relay sidecar: hello carried no valid x25519 attestation; dropping"
                        );
                        return;
                    }
                },
                Err(()) => return,
            }
        }
        Ok(None) => return,
        Err(_elapsed) => {
            tracing::warn!("relay sidecar: no hello within deadline; dropping connection");
            return;
        }
    };

    tracing::info!("relay sidecar channel established (listener side)");
    // Count this relay in for as long as its channel lives: the count is how the
    // nest knows its deployment runs a relay (`discovery_core::relay_sidecar_connected`).
    let _connected = RelayConnected::new(state);

    // The cert-changed signal, marked seen as of now: the relay fetches its cert
    // itself the moment its channel is up, so only a LATER change needs a push.
    let mut cert_changed = state.relay_cert_changed.subscribe();
    cert_changed.borrow_and_update();

    // Serve the relay-origin kinds until the channel closes. Re-sealing on a
    // retry is cheap and safe (seal-on-read) and the admission answer is a pure
    // read, so this lean loop needs no idempotency cache: a replayed request
    // just gets the same answer again.
    let mut watching = true;
    loop {
        let req = tokio::select! {
            req = inbound.recv() => match req {
                Some(req) => req,
                None => break,
            },
            changed = cert_changed.changed(), if watching => {
                match changed {
                    Ok(()) => push_relay_cert_changed(state, dispatcher),
                    // The sender is gone: nothing will change again. Stop
                    // watching (a closed `watch` resolves at once, for ever).
                    Err(_) => watching = false,
                }
                continue;
            }
        };
        let correlation_id = req.correlation_id;
        let payload = match req.kind.as_str() {
            KIND_RELAY_FETCH_TLS_CERT => {
                let blob = relay_fetch_tls_cert(state, &relay_x25519);
                to_value(&RelayFetchTlsCertReply {
                    blob: blob.map(ByteBuf::from),
                    extra: Default::default(),
                })
            }
            // Who may use the relay (`p2p.md` § The relay): the nest answers
            // from its own tables, the relay holds no list. A key that does
            // not parse is not a known key.
            KIND_RELAY_ADMIT => {
                let admitted = match value_to::<RelayAdmitRequest>(&req.payload) {
                    Ok(parsed) => match fauna_core::hex32::decode(&parsed.endpoint_key) {
                        Ok(key) => crate::relay_admission::key_is_known(&state.db, &key).await,
                        Err(_) => false,
                    },
                    Err(()) => false,
                };
                to_value(&RelayAdmitReply {
                    admitted,
                    extra: Default::default(),
                })
            }
            // The sidecar log plane's internal-duplex leg (`observability.md`
            // § The sidecar log plane). The relay channel is deliberately lean
            // — no idempotency cache, no router — so the kind is served
            // inline. Source is `relay` because *this channel* is the relay's:
            // attribution comes from the authenticated channel class, never
            // the payload.
            KIND_SIDECAR_LOG_EVENTS => {
                match value_to::<ReportLogEventsRequest>(&req.payload) {
                    Ok(parsed) => {
                        crate::log_plane::admit(crate::log_plane::LogSource::Relay, &parsed);
                    }
                    Err(()) => {
                        // Malformed report: drop it and keep the channel up —
                        // the plane must never disturb the relay's real work.
                        tracing::debug!("relay sidecar: malformed log-event batch; ignoring");
                    }
                }
                to_value(&ReportLogEventsReply::default())
            }
            _ => {
                tracing::debug!(
                    kind = %req.kind,
                    "relay sidecar: kind not allowed on this channel; rejecting unauthenticated"
                );
                send_error(dispatcher, correlation_id, unauthenticated()).await;
                continue;
            }
        };
        let _ =
            fauna_peer_channel::send_reply_bounded(dispatcher, correlation_id, payload, true).await;
    }

    tracing::info!("relay sidecar channel closed (listener side)");
}

/// One connected relay sidecar, counted in [`AppState::relay_channels`] from the
/// handshake until the channel's serve loop ends, however it ends.
struct RelayConnected(Arc<std::sync::atomic::AtomicUsize>);

impl RelayConnected {
    fn new(state: &AppState) -> Self {
        state
            .relay_channels
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(Arc::clone(&state.relay_channels))
    }
}

impl Drop for RelayConnected {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How long the nest waits for the relay to acknowledge a
/// `fauna.relay.cert_changed` push before giving up on it. A lost push costs
/// nothing but promptness: the relay re-fetches on every reconnect and on its
/// own backstop timer.
const CERT_CHANGED_PUSH_DEADLINE: Duration = Duration::from_secs(10);

/// Tell the relay on this channel that the cert on disk changed, so it fetches
/// it again now (`tls-certificates.md` § Keeping the cert alive). Spawned, never
/// awaited by the serve loop: the relay answers by originating a fetch on this
/// same channel, which only the loop can serve.
fn push_relay_cert_changed(state: &Arc<AppState>, dispatcher: &Arc<RpcDispatcher>) {
    let dispatcher = Arc::clone(dispatcher);
    state.spawn_scoped(async move {
        let mut idem = [0u8; 16];
        if getrandom::fill(&mut idem).is_err() {
            return;
        }
        let pushed: Result<RelayCertChangedReply, _> = dispatcher
            .request_typed(
                KIND_RELAY_CERT_CHANGED,
                idem,
                &RelayCertChangedRequest::default(),
                CERT_CHANGED_PUSH_DEADLINE,
                tokio::time::sleep(CERT_CHANGED_PUSH_DEADLINE),
            )
            .await;
        match pushed {
            Ok(_) => tracing::info!("relay sidecar: told the relay its cert changed"),
            Err(e) => tracing::debug!(
                error = ?e,
                "relay sidecar: cert-changed push not acknowledged (the relay re-fetches on reconnect)"
            ),
        }
    });
}

/// Seal the nest's current on-disk TLS cert to the relay's attested X25519,
/// returning the wrapped `TlsCertBlob` bytes (`None` when no cert is on disk yet,
/// or the seal failed — the relay retries on its refresh timer).
///
/// Seals DIRECTLY from `state.acme_dir`, NOT via `state.storage()`. The relay's
/// TLS cert is deployment infra (the on-disk ACME/floor PEM plus the relay's
/// attested x25519), never user data, so it MUST NOT ride the user-data storage
/// seam at all. Routing it through `state.storage()` coupled the two into the
/// same client-causable broken state the mail path hit
/// (`bridge_blob_handlers::fetch_tls_cert_blob_handler`, root-caused live
/// 2026-07-05): back when a fresh box's storage was the retired `UnconfiguredStorage` (whose
/// `seal_current_tls_cert_for_x25519` default returns `Ok(None)`) the relay could
/// never obtain a cert and served no `relay.<apex>` TLS indefinitely. That
/// unresolved state died with the storage-mode axis, but the decoupling stands
/// on its own merits and is pinned by
/// `relay_fetch_tls_cert_seals_even_with_storage_unconfigured`, which installs a
/// `Storage` impl that declines to seal. The `_impl` does no DB lookup (the relay
/// supplies its own attested x25519 on the live channel), so the direct call is
/// byte-identical to the storage override.
fn relay_fetch_tls_cert(state: &AppState, relay_x25519: &[u8; 32]) -> Option<Vec<u8>> {
    // The relay serves only on a nest with a public name of its own (`p2p.md`
    // § The relay). Until then it is handed nothing and stands by; the claim
    // that gives the nest its name bumps `relay_cert_changed`, and it asks again.
    if !crate::discovery_core::relay_wanted(&state.handle_domain()) {
        return None;
    }
    let domain = relay_seal_domain(state);
    match crate::storage::seal_current_tls_cert_for_x25519_impl(
        &state.acme_dir,
        SidecarScope::Relay.as_str(),
        RELAY_BRIDGE_ID,
        &domain,
        relay_x25519,
    ) {
        Ok(blob) => blob,
        Err(e) => {
            tracing::error!(error = %e, "relay sidecar: seal current tls cert failed");
            None
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────────────────

fn malformed(err: impl std::fmt::Display) -> RpcError {
    tracing::debug!("sidecar handshake malformed payload: {err}");
    RpcError::new("fauna.protocol.malformed", "error.protocol.malformed")
}

fn unauthenticated() -> RpcError {
    crate::rpc_errors::unauthenticated()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::test_transport::{MpscTransport, make_pair};
    use fauna_sidecar_client::SidecarDialError;

    fn tokens_with(token: &str, scopes: Vec<SidecarScope>) -> HashMap<String, Vec<SidecarScope>> {
        let mut m = HashMap::new();
        m.insert(token.to_string(), scopes);
        m
    }

    fn hello(token: &str, scope: &str) -> SidecarHello {
        SidecarHello {
            token: token.into(),
            scope: scope.into(),
            extra: Default::default(),
        }
    }

    /// The dialer half of the handshake, as the relay binary runs it: the
    /// shared [`fauna_sidecar_client::sidecar_hello`], declaring the Relay scope.
    async fn relay_hello(
        dispatcher: &Arc<RpcDispatcher>,
        token: &str,
        extra: std::collections::BTreeMap<String, Value>,
    ) -> Result<(), SidecarDialError> {
        fauna_sidecar_client::sidecar_hello(dispatcher, token, SidecarScope::Relay.as_str(), extra)
            .await
    }

    // ── verify_sidecar_hello (pure) ──────────────────────────────────────────

    #[test]
    fn hello_accepts_valid_token_and_scope() {
        let tokens = tokens_with("tok", vec![SidecarScope::Relay]);
        assert_eq!(
            verify_sidecar_hello(&hello("tok", "relay"), &tokens, SidecarScope::Relay),
            Ok(SidecarScope::Relay)
        );
    }

    #[test]
    fn hello_rejects_unknown_token() {
        let tokens = tokens_with("tok", vec![SidecarScope::Relay]);
        assert_eq!(
            verify_sidecar_hello(&hello("wrong", "relay"), &tokens, SidecarScope::Relay),
            Err(SidecarHelloError::UnknownToken)
        );
    }

    /// Scope isolation: a valid bridge token must not authenticate the relay
    /// channel, and a relay token must not satisfy a listener bound to another
    /// scope.
    #[test]
    fn hello_rejects_token_without_required_scope() {
        let bridge_tokens = tokens_with("tok", vec![SidecarScope::Bridge]);
        assert_eq!(
            verify_sidecar_hello(&hello("tok", "relay"), &bridge_tokens, SidecarScope::Relay),
            Err(SidecarHelloError::ScopeNotGranted)
        );
        let relay_tokens = tokens_with("rtok", vec![SidecarScope::Relay]);
        assert_eq!(
            verify_sidecar_hello(
                &hello("rtok", "bridge"),
                &relay_tokens,
                SidecarScope::Bridge
            ),
            Err(SidecarHelloError::ScopeNotGranted)
        );
    }

    #[test]
    fn hello_rejects_scope_mismatch_and_unknown_scope() {
        let tokens = tokens_with("tok", vec![SidecarScope::Relay, SidecarScope::Dns]);
        // Declared scope (dns) ≠ this listener's required scope (relay).
        assert_eq!(
            verify_sidecar_hello(&hello("tok", "dns"), &tokens, SidecarScope::Relay),
            Err(SidecarHelloError::ScopeMismatch)
        );
        // A scope string that doesn't parse.
        assert_eq!(
            verify_sidecar_hello(&hello("tok", "bogus"), &tokens, SidecarScope::Relay),
            Err(SidecarHelloError::UnknownScope)
        );
    }

    /// The sidecar log plane's internal-duplex leg: the relay originates
    /// `fauna.sidecar.log_events` and the entry lands in the remote ring
    /// attributed to the channel's own class, never the payload
    /// (`observability.md` § The sidecar log plane).
    #[tokio::test]
    async fn relay_channel_serves_the_log_plane_leg() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;

        let acme = tempfile::tempdir().unwrap();
        crate::self_signed_cert::write_self_signed_bootstrap(acme.path(), Some("example.com"))
            .unwrap();
        let (nest_t, relay_t) = make_pair();
        let state = relay_test_state("relay-token", acme.path().to_path_buf());
        spawn_relay_listener(state, nest_t);

        let (disp, drv) = RpcDispatcher::new(relay_t);
        // spawn-ok(test)
        tokio::spawn(drv);
        let disp = Arc::new(disp);
        let (_x_sk, x_pk) = generate_x25519_keypair();
        relay_hello(&disp, "relay-token", relay_hello_extra(&x_pk))
            .await
            .expect("relay handshake");

        fauna_log::clear();
        let _: ReportLogEventsReply = fauna_sidecar_client::request_typed(
            &disp,
            KIND_SIDECAR_LOG_EVENTS,
            &ReportLogEventsRequest {
                events: vec![fauna_protocol::log_plane::SidecarLogEvent {
                    timestamp_ms: 1_000,
                    level: "error".into(),
                    event: "cert_fetch_failed".into(),
                    message: "TLS cert fetch failed".into(),
                    ..Default::default()
                }],
                dropped: 0,
                ..Default::default()
            },
        )
        .await
        .expect("log_events reply on the relay channel");

        let ring = fauna_log::snapshot_remote();
        let entry = ring
            .iter()
            .find(|e| e.message == "TLS cert fetch failed")
            .expect("the reported event reached the remote ring");
        assert_eq!(entry.target, "relay:cert_fetch_failed");
        assert_eq!(entry.level, fauna_log::LogLevel::Error);
    }

    // ── relay sidecar channel ────────────────────────────────────────────────

    /// A relay-token test AppState whose `acme_dir` holds the floor cert. The
    /// relay TLS-fetch path seals directly from `state.acme_dir` (deployment
    /// infra, storage-mode-independent — see `relay_fetch_tls_cert`), so the
    /// storage mode is irrelevant here; point `acme_dir` at the caller's cert dir.
    fn relay_test_state(token: &str, acme_dir: std::path::PathBuf) -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let mut state = AppState::for_test(db.clone());
        state.acme_dir = acme_dir;
        // A claimed nest with a public name: the only kind the relay serves for.
        state
            .identity_domain
            .store(Some(Arc::new("nest.example.com".to_string())));
        state
            .sidecar_tokens
            .insert(token.to_string(), vec![SidecarScope::Relay]);
        Arc::new(state)
    }

    /// [`relay_test_state`] with a `Storage` impl that declines to seal the cert
    /// (every seal-on-read takes the trait default → `Ok(None)`), so the relay
    /// path must seal from `acme_dir` itself or serve nothing.
    fn relay_test_state_with_default_seals(
        token: &str,
        acme_dir: std::path::PathBuf,
    ) -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let mut state = AppState::for_test(db.clone());
        state.acme_dir = acme_dir;
        state
            .identity_domain
            .store(Some(Arc::new("nest.example.com".to_string())));
        state
            .sidecar_tokens
            .insert(token.to_string(), vec![SidecarScope::Relay]);
        state.install_storage_for_test(std::sync::Arc::new(crate::storage::DefaultSealsStorage)
            as crate::storage::SharedStorage);
        Arc::new(state)
    }

    /// Spawn the relay listener half over one duplex end (handshake + serve loop).
    fn spawn_relay_listener(state: Arc<AppState>, transport: MpscTransport) {
        let (dispatcher, driver) = RpcDispatcher::new(transport);
        let dispatcher = Arc::new(dispatcher);
        // spawn-ok(test)
        tokio::spawn(driver);
        let inbound = dispatcher.inbound_requests().unwrap();
        tokio::spawn(async move {
            serve_relay_channel(&state, &dispatcher, inbound).await;
        });
    }

    fn relay_hello_extra(x25519_pub: &[u8; 32]) -> std::collections::BTreeMap<String, Value> {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert(
            HELLO_EXTRA_X25519.to_string(),
            Value::String(hex::encode(x25519_pub)),
        );
        extra
    }

    /// The full in-process relay round-trip: the relay handshakes (attesting its
    /// X25519), originates `fauna.relay.fetch_tls_cert`, and unseals the reply with
    /// its own X25519 secret to recover the **exact on-disk cert** — proving the
    /// handshake binds Relay, the attested X25519 is captured, the nest seals the
    /// floor cert to it, and the relay can open it (the whole sealed-cert path).
    #[tokio::test]
    async fn relay_handshake_then_fetch_tls_cert_round_trips() {
        use fauna_mls::wrapped_blob::{TlsCertBlob, generate_x25519_keypair, unseal_tls_cert};
        use fauna_protocol::relay::{RelayFetchTlsCertReply, RelayFetchTlsCertRequest};

        let acme = tempfile::tempdir().unwrap();
        crate::self_signed_cert::write_self_signed_bootstrap(acme.path(), Some("example.com"))
            .unwrap();
        let cert_on_disk = std::fs::read(acme.path().join(crate::acme::CERT_FILENAME)).unwrap();

        let (nest_t, relay_t) = make_pair();
        let state = relay_test_state("relay-token", acme.path().to_path_buf());
        spawn_relay_listener(state, nest_t);

        let (disp, drv) = RpcDispatcher::new(relay_t);
        // spawn-ok(test)
        tokio::spawn(drv);
        let disp = Arc::new(disp);

        // The relay attests its X25519 public key in the hello.
        let (x_sk, x_pk) = generate_x25519_keypair();
        relay_hello(&disp, "relay-token", relay_hello_extra(&x_pk))
            .await
            .expect("relay handshake");

        // Fetch + unseal with our secret → the exact on-disk cert.
        let reply: RelayFetchTlsCertReply = fauna_sidecar_client::request_typed(
            &disp,
            KIND_RELAY_FETCH_TLS_CERT,
            &RelayFetchTlsCertRequest::default(),
        )
        .await
        .expect("fetch reply");
        let blob_bytes = reply.blob.expect("a sealed cert blob");
        let blob = TlsCertBlob::from_canonical_bytes(&blob_bytes).expect("decode blob");
        let bundle = unseal_tls_cert(&blob, &x_sk).expect("unseal with our x25519 secret");
        assert_eq!(
            bundle.cert_chain, cert_on_disk,
            "the relay unseals the exact on-disk floor cert"
        );

        // A different X25519 secret must NOT open it (the seal is bound to ours).
        let (other_sk, _other_pk) = generate_x25519_keypair();
        assert!(
            unseal_tls_cert(&blob, &other_sk).is_err(),
            "a non-recipient secret cannot open the relay's sealed cert"
        );
    }

    /// Regression guard (sibling of the mail-path
    /// `bridge_blob_handlers::fetch_tls_cert_seals_even_with_storage_unconfigured`,
    /// root-caused live 2026-07-05): the relay TLS fetch must deliver a sealed cert
    /// even when NO storage mode is committed. A fresh box can enable the relay
    /// before the admin commits a storage mode, so `state.storage()` is
    /// a `Storage` impl whose `seal_current_tls_cert_for_x25519` default
    /// returns `Ok(None)`. The cert is deployment infra, so `relay_fetch_tls_cert`
    /// seals directly from `state.acme_dir` and still succeeds. Before this
    /// storage decoupling the relay served no `relay.<apex>` TLS indefinitely.
    #[tokio::test]
    async fn relay_fetch_tls_cert_seals_even_with_storage_unconfigured() {
        use fauna_mls::wrapped_blob::{TlsCertBlob, generate_x25519_keypair, unseal_tls_cert};

        let acme = tempfile::tempdir().unwrap();
        crate::self_signed_cert::write_self_signed_bootstrap(acme.path(), Some("example.com"))
            .unwrap();
        let cert_on_disk = std::fs::read(acme.path().join(crate::acme::CERT_FILENAME)).unwrap();

        // A storage impl whose seal-on-read returns `Ok(None)` (the trait
        // default), so a cert that IS delivered proves the handler sealed it
        // itself rather than delegating to `state.storage()`.
        let state = relay_test_state_with_default_seals("relay-token", acme.path().to_path_buf());

        let (x_sk, x_pk) = generate_x25519_keypair();
        let blob_bytes = relay_fetch_tls_cert(&state, &x_pk)
            .expect("cert must seal even when the storage mode is uncommitted");
        let blob = TlsCertBlob::from_canonical_bytes(&blob_bytes).expect("decode blob");
        let bundle = unseal_tls_cert(&blob, &x_sk).expect("unseal with our x25519 secret");
        assert_eq!(
            bundle.cert_chain, cert_on_disk,
            "the relay unseals the exact on-disk floor cert with no storage mode committed"
        );
    }

    /// **A REFUSED hello reaches the dialer as `unauthenticated`, and the
    /// listener's driver still ends** — `transport.md` § Layers (L3 frame
    /// lifecycle).
    ///
    /// `listener_handshake` answers a bad token by *enqueuing* an
    /// `unauthenticated` Reply — `send_reply` only puts it on the dispatcher's
    /// bounded `out_tx`, and the driver task is what writes it. The listener
    /// then used to `abort()` that driver on the very next line, a race the
    /// abort usually won under load; the dialer's `await_reply` saw its oneshot
    /// dropped and synthesised `fauna.protocol.disconnected`. The same single
    /// defect was logged as `unauthenticated` on one run and `disconnected` on
    /// another, which read as two bugs on two sidecars.
    ///
    /// Both halves of the fix are asserted, because either alone is a trap: the
    /// dialer must read the code the listener actually sent, **and** the
    /// listener's driver must finish once the dispatcher is gone. Without the
    /// second, a listener could only stop aborting by detaching the driver —
    /// which leaves an unauthenticated connection's task alive for exactly as
    /// long as the refused peer chooses to hold the socket open.
    #[tokio::test]
    async fn a_refused_hello_reaches_the_dialer_as_unauthenticated() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;

        let acme = tempfile::tempdir().unwrap();
        crate::self_signed_cert::write_self_signed_bootstrap(acme.path(), Some("example.com"))
            .unwrap();
        let (nest_t, relay_t) = make_pair();
        let state = relay_test_state("relay-token", acme.path().to_path_buf());

        // The listener, in the shape `serve_relay_listener` runs it: spawn the
        // driver, serve, and tear down by DROPPING the dispatcher — no abort.
        let (dispatcher, driver) = RpcDispatcher::new(nest_t);
        let dispatcher = Arc::new(dispatcher);
        // spawn-ok(test)
        let driver_handle = tokio::spawn(driver);
        let inbound = dispatcher.inbound_requests().unwrap();
        // spawn-ok(test)
        tokio::spawn(async move {
            serve_relay_channel(&state, &dispatcher, inbound).await;
            // `dispatcher` drops here, closing `out_tx`.
        });

        let (disp, drv) = RpcDispatcher::new(relay_t);
        // spawn-ok(test)
        tokio::spawn(drv);
        let disp = Arc::new(disp);
        let (_x_sk, x_pk) = generate_x25519_keypair();

        let err = relay_hello(&disp, "not-the-relay-token", relay_hello_extra(&x_pk))
            .await
            .expect_err("a token the nest does not know must be refused");
        match err {
            SidecarDialError::Rejected(code) => assert_eq!(
                code, "fauna.protocol.unauthenticated",
                "a refused credential must arrive as the code the listener SENT, never as \
                 `fauna.protocol.disconnected` — telling the two apart is exactly what a \
                 human reading these logs could not do"
            ),
            other => panic!("expected a rejection carrying the listener's code, got {other:?}"),
        }

        // …and the listener's driver ends on its own, without the refused peer
        // having to close the socket. A generous ceiling on an already-complete
        // transition (`out_tx` is closed), not a settle-sleep.
        tokio::time::timeout(std::time::Duration::from_secs(10), driver_handle)
            .await
            .expect("the driver must exit once the listener has dropped its dispatcher")
            .expect("driver task did not panic");
    }

    /// The relay channel serves ONLY its own allowlist; any other kind (here a
    /// client-actor kind) is rejected `unauthenticated`, so a relay token can
    /// never reach another surface.
    #[tokio::test]
    async fn relay_channel_rejects_other_kinds() {
        use fauna_mls::wrapped_blob::generate_x25519_keypair;

        let acme = tempfile::tempdir().unwrap();
        crate::self_signed_cert::write_self_signed_bootstrap(acme.path(), Some("example.com"))
            .unwrap();
        let (nest_t, relay_t) = make_pair();
        let state = relay_test_state("relay-token", acme.path().to_path_buf());
        spawn_relay_listener(state, nest_t);

        let (disp, drv) = RpcDispatcher::new(relay_t);
        // spawn-ok(test)
        tokio::spawn(drv);
        let disp = Arc::new(disp);
        let (_x_sk, x_pk) = generate_x25519_keypair();
        relay_hello(&disp, "relay-token", relay_hello_extra(&x_pk))
            .await
            .expect("relay handshake");

        let call = disp
            .request_raw("fauna.conversations.send", [7u8; 16], Value::Null, None)
            .await
            .unwrap();
        let err = call.await_reply().await.unwrap_err();
        assert_eq!(err.code, "fauna.protocol.unauthenticated");
    }

    /// A relay that has completed its handshake against `state` over a fresh
    /// in-memory channel — the dialer end, as the relay binary holds it.
    async fn handshaken_relay(state: Arc<AppState>) -> Arc<RpcDispatcher> {
        let (nest_t, relay_t) = make_pair();
        spawn_relay_listener(state, nest_t);
        let (disp, drv) = RpcDispatcher::new(relay_t);
        // spawn-ok(test)
        tokio::spawn(drv);
        let disp = Arc::new(disp);
        let (_x_sk, x_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        relay_hello(&disp, "relay-token", relay_hello_extra(&x_pk))
            .await
            .expect("relay handshake");
        disp
    }

    /// The relay's question, as the relay binary asks it.
    async fn relay_asks(disp: &Arc<RpcDispatcher>, endpoint_key: String) -> bool {
        let reply: RelayAdmitReply = fauna_sidecar_client::request_typed(
            disp,
            KIND_RELAY_ADMIT,
            &RelayAdmitRequest {
                endpoint_key,
                extra: Default::default(),
            },
        )
        .await
        .expect("fauna.relay.admit reply");
        reply.admitted
    }

    /// Who may use the relay (`p2p.md` § The relay): a device enrolled for one
    /// of the nest's accounts and a member's actor key are admitted; a stranger,
    /// a key that does not parse, a removed device and — *Across nests: nobody* —
    /// a foreign member of a set this nest hosts are all refused.
    #[tokio::test]
    async fn relay_admit_serves_members_devices_and_nobody_else() {
        let acme = tempfile::tempdir().unwrap();
        let state = relay_test_state("relay-token", acme.path().to_path_buf());
        let db = state.db.clone();

        let member = [0x11u8; 32];
        let device_id = [0x22u8; 32];
        let device_key = [0x33u8; 32];
        db.create_user(&member, "free", "member").await.unwrap();
        db.register_sync_device(&member, &device_id, "laptop", None, "read,write")
            .await
            .unwrap();
        db.set_sync_device_grant(&member, &device_id, &device_key, b"grant")
            .await
            .unwrap();

        // A member of a set hosted here whose account lives on another nest.
        let foreign_member = [0x44u8; 32];
        db.register_foreign_channel_member(
            &[0x55u8; 32],
            &foreign_member,
            &[0x66u8; 32],
            None,
            crate::db::channels::RebindPower::Standing,
        )
        .await
        .unwrap();

        let disp = handshaken_relay(state).await;
        let hex = fauna_core::hex32::encode;

        assert!(
            relay_asks(&disp, hex(&device_key)).await,
            "an enrolled device's principal key is admitted"
        );
        assert!(
            relay_asks(&disp, hex(&member)).await,
            "a member's actor key is admitted"
        );
        assert!(
            !relay_asks(&disp, hex(&[0x99u8; 32])).await,
            "a key the nest does not know is refused"
        );
        assert!(
            !relay_asks(&disp, hex(&device_id)).await,
            "a device's row id is not its key"
        );
        assert!(
            !relay_asks(&disp, "not-a-key".to_string()).await,
            "a key that does not parse is refused, not an error"
        );
        assert!(
            !relay_asks(&disp, hex(&foreign_member)).await,
            "across nests: nobody — a foreign member of a hosted set is refused"
        );

        // Removing the device severs its admission.
        db.delete_device(&device_id, &member).await.unwrap();
        assert!(
            !relay_asks(&disp, hex(&device_key)).await,
            "a removed device's key is refused"
        );
    }

    /// A nest with no public name of its own hands the relay nothing — the relay
    /// stands by — and the same relay gets its cert once the nest has a name.
    #[tokio::test]
    async fn relay_is_handed_no_cert_until_the_nest_has_a_public_name() {
        use fauna_protocol::relay::RelayFetchTlsCertRequest;

        let acme = tempfile::tempdir().unwrap();
        crate::self_signed_cert::write_self_signed_bootstrap(acme.path(), Some("example.com"))
            .unwrap();
        let state = relay_test_state("relay-token", acme.path().to_path_buf());
        state.identity_domain.store(None);
        let disp = handshaken_relay(Arc::clone(&state)).await;

        let fetch = || async {
            let reply: RelayFetchTlsCertReply = fauna_sidecar_client::request_typed(
                &disp,
                KIND_RELAY_FETCH_TLS_CERT,
                &RelayFetchTlsCertRequest::default(),
            )
            .await
            .expect("fetch reply");
            reply.blob
        };
        assert!(fetch().await.is_none(), "a domainless nest hands no cert");

        state
            .identity_domain
            .store(Some(Arc::new("192.168.1.20".to_string())));
        assert!(fetch().await.is_none(), "an IP is not a public name");

        state
            .identity_domain
            .store(Some(Arc::new("nest.example.com".to_string())));
        assert!(fetch().await.is_some(), "a public name: the relay serves");
    }

    /// "Does this deployment run its relay" is the live channel, nothing else:
    /// true from a relay's handshake until its channel closes.
    #[tokio::test]
    async fn relay_sidecar_connected_follows_the_channel() {
        use crate::discovery_core::relay_sidecar_connected;

        let acme = tempfile::tempdir().unwrap();
        let state = relay_test_state("relay-token", acme.path().to_path_buf());
        assert!(
            !relay_sidecar_connected(&state),
            "no relay, none advertised"
        );

        let disp = handshaken_relay(Arc::clone(&state)).await;
        // The handshake reply is sent before the listener counts itself in; one
        // served request proves it is past that point.
        relay_asks(&disp, "00".repeat(32)).await;
        assert!(relay_sidecar_connected(&state));

        drop(disp);
        tokio::time::timeout(Duration::from_secs(5), async {
            while relay_sidecar_connected(&state) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the count drops when the relay's channel closes");
    }

    /// A cert change on disk reaches a connected relay as a
    /// `fauna.relay.cert_changed` push — and only a change AFTER the channel
    /// came up does: the relay fetches for itself when it connects.
    #[tokio::test]
    async fn relay_channel_pushes_cert_changed() {
        let acme = tempfile::tempdir().unwrap();
        let state = relay_test_state("relay-token", acme.path().to_path_buf());
        // A change before any relay is connected pushes nothing later.
        state.relay_cert_changed.send_modify(|g| *g += 1);

        let disp = handshaken_relay(Arc::clone(&state)).await;
        let mut pushed = disp.inbound_requests().expect("relay inbound");
        // The handshake reply proves the listener is past its `subscribe`.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), pushed.recv())
                .await
                .is_err(),
            "a change from before the channel came up must not be pushed"
        );

        state.relay_cert_changed.send_modify(|g| *g += 1);
        let req = tokio::time::timeout(Duration::from_secs(5), pushed.recv())
            .await
            .expect("push within the deadline")
            .expect("channel open");
        assert_eq!(req.kind, KIND_RELAY_CERT_CHANGED);
    }

    /// The seal-on-read impl seals the on-disk cert to a supplied X25519 and the
    /// matching secret opens it; with no cert on disk it returns `None`.
    #[test]
    fn seal_current_tls_cert_for_x25519_round_trips() {
        use fauna_mls::wrapped_blob::{TlsCertBlob, generate_x25519_keypair, unseal_tls_cert};

        let acme = tempfile::tempdir().unwrap();
        let (x_sk, x_pk) = generate_x25519_keypair();
        // No cert on disk yet → None.
        assert!(
            crate::storage::seal_current_tls_cert_for_x25519_impl(
                acme.path(),
                "relay",
                RELAY_BRIDGE_ID,
                "relay.example.com",
                &x_pk,
            )
            .unwrap()
            .is_none(),
            "no cert on disk seals nothing"
        );
        // Write a floor cert, then it seals + unseals to the exact PEM.
        crate::self_signed_cert::write_self_signed_bootstrap(acme.path(), Some("example.com"))
            .unwrap();
        let blob_bytes = crate::storage::seal_current_tls_cert_for_x25519_impl(
            acme.path(),
            "relay",
            RELAY_BRIDGE_ID,
            "relay.example.com",
            &x_pk,
        )
        .unwrap()
        .expect("sealed cert");
        let blob = TlsCertBlob::from_canonical_bytes(&blob_bytes).unwrap();
        let bundle = unseal_tls_cert(&blob, &x_sk).expect("unseal");
        assert_eq!(
            bundle.cert_chain,
            std::fs::read(acme.path().join(crate::acme::CERT_FILENAME)).unwrap()
        );
    }

    /// `relay_x25519_from_hello` accepts a valid 32-byte hex pubkey and rejects
    /// every malformed shape (absent, non-string, bad hex, wrong length).
    #[test]
    fn relay_x25519_from_hello_parses_and_validates() {
        let pk = [0x5au8; 32];
        let mut hello = SidecarHello {
            token: "t".into(),
            scope: "relay".into(),
            extra: relay_hello_extra(&pk),
        };
        assert_eq!(relay_x25519_from_hello(&hello), Some(pk));

        // Absent.
        hello.extra.clear();
        assert_eq!(relay_x25519_from_hello(&hello), None);
        // Not a string.
        hello
            .extra
            .insert(HELLO_EXTRA_X25519.to_string(), Value::Bool(true));
        assert_eq!(relay_x25519_from_hello(&hello), None);
        // Bad hex.
        hello.extra.insert(
            HELLO_EXTRA_X25519.to_string(),
            Value::String("nothex!!".into()),
        );
        assert_eq!(relay_x25519_from_hello(&hello), None);
        // Valid hex but wrong length (16 bytes).
        hello.extra.insert(
            HELLO_EXTRA_X25519.to_string(),
            Value::String(hex::encode([1u8; 16])),
        );
        assert_eq!(relay_x25519_from_hello(&hello), None);
    }

    // ── sidecar_ws_upgrade IP gate (fail-CLOSED) ─────────────────────────────

    /// The internal-sidecar WS upgrade gate fails **CLOSED** when no `ConnectInfo`
    /// is present (the hardening): both production serving
    /// paths inject it, so its absence means a middleware regression — and that must
    /// reject, never silently open the internal channel to a non-loopback source.
    #[tokio::test]
    async fn sidecar_ws_upgrade_rejects_when_connect_info_absent() {
        let state = Arc::new(AppState::for_test(Arc::new(
            crate::db::CacheDb::open_in_memory().unwrap(),
        )));
        let req = axum::http::Request::builder()
            .uri("/internal/relay/ws")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = sidecar_ws_upgrade(&state, req)
            .await
            .expect_err("a missing ConnectInfo must be rejected");
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    /// A non-loopback, non-allowlisted source is rejected even with `ConnectInfo`
    /// present (the IP allowlist itself).
    #[tokio::test]
    async fn sidecar_ws_upgrade_rejects_non_loopback_source() {
        let state = Arc::new(AppState::for_test(Arc::new(
            crate::db::CacheDb::open_in_memory().unwrap(),
        )));
        let mut req = axum::http::Request::builder()
            .uri("/internal/relay/ws")
            .body(axum::body::Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [8, 8, 8, 8],
                1234,
            ))));
        let resp = sidecar_ws_upgrade(&state, req)
            .await
            .expect_err("a non-loopback source must be rejected");
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }
}
