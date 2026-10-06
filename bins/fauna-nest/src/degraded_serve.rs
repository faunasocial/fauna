//! Degraded "needs-update" boot mode (version-compatibility.md § 2.2).
//!
//! When [`crate::db::CacheDb::open`] reports a
//! [`crate::db::migrations::SchemaVerdict::Incompatible`] verdict — the on-disk
//! DB was written by a newer nest carrying a breaking schema change this binary
//! predates — the nest MUST NOT crash-loop (an off-box brick the
//! `nest/common.md` § Client-state recoverability invariant outlaws) and MUST
//! NOT run destructive migrations against a DB it does not understand. Instead
//! it boots into this minimal mode: a TLS/WS listener that answers **every**
//! WS-RPC request — the anonymous discovery surface included — with the typed
//! [`fauna_protocol::RpcError::nest_outdated`] (`fauna.nest.outdated`) error, so
//! a client sees an actionable "update this nest" signal rather than a silent
//! crash-loop or a raw SQL leak (Dim 4).
//!
//! This path opens **no** database and constructs **no** `AppState` (both
//! require a DB this binary can operate); it shares only the on-disk TLS
//! material and the one shared accept loop with the normal boot —
//! [`crate::serve_tls`] and [`crate::serve_plain`] alike, so this listener's
//! admission bounds are the same ones every other listener gets.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use fauna_protocol::{Frame, Reply, RpcError, decode_frame, encode_frame};
use futures_util::StreamExt;

use crate::db::migrations::SchemaIncompatible;
use crate::ws::{Beat, ServerHeartbeat, WsHeartbeatPolicy};

/// Bring up the degraded listener and serve it until the process is replaced
/// (the s6 supervisor restarts the nest after the admin deploys a newer
/// binary). Returns `Err` only if the listener cannot bind; the accept loop
/// itself runs for the process lifetime.
pub async fn serve_incompatible(
    bind: std::net::SocketAddr,
    tls_config: Option<Arc<rustls::ServerConfig>>,
    incompatible: SchemaIncompatible,
) -> anyhow::Result<()> {
    tracing::error!(
        db_schema_version = incompatible.db_v,
        db_min_reader_version = incompatible.db_min,
        binary_schema_version = incompatible.bin_v,
        "INCOMPATIBLE DATABASE: the on-disk nest DB was written by a newer nest \
         carrying a breaking schema change this binary predates. Booting DEGRADED \
         'needs-update' mode — answering fauna.nest.outdated to all clients, NOT \
         migrating the DB and NOT crash-looping. Update this nest binary to recover."
    );

    let app = degraded_router();
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let local_addr = listener.local_addr()?;

    // A fresh default-capped limiter, shared by BOTH arms: the degraded path
    // opens no DB, so the client-set `transport_policy` override is unavailable
    // — the constant default per-IP cap is the right fallback.
    let per_ip_limit =
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP);

    if let Some(tls) = tls_config {
        tracing::warn!("degraded nest serving HTTPS on {local_addr} (needs-update mode)");
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        crate::serve_tls(listener, acceptor, app.into_make_service(), per_ip_limit).await;
    } else {
        // Plain-HTTP dev/e2e nest (no domain, no cert). Still come up so a client
        // gets the typed error rather than a connection refusal.
        //
        // It rides `serve_plain` — the SAME shared accept loop as the TLS arm and
        // as both normal-boot listeners — so the global cap, the per-IP permit,
        // the TCP keepalive and the `header_read_timeout` apply here too
        // (`transport-connection.md` § Abuse posture: "a defence added for one listener
        // cannot be missing on the other"). It was a bare `axum::serve` until
        // 2026-08-29, which is the exact shape that let one leaking client
        // accumulate 16 k accepted sockets on a nest that logged nothing — the
        // 2026-08-22 fix reached the two normal listeners and stopped short of
        // this one. `serve_plain` injects `ConnectInfo` itself, so the handlers
        // see the source address exactly as before.
        tracing::warn!("degraded nest serving HTTP on {local_addr} (needs-update mode)");
        crate::serve_plain(listener, app.into_make_service(), per_ip_limit).await;
    }
    Ok(())
}

/// The minimal router. Both WS endpoints (authenticated + anonymous) and any
/// other path funnel to the same `fauna.nest.outdated` signal. `/api/v1/health`
/// stays a 200 liveness probe (the process **is** up — only the schema is
/// incompatible) so the supervisor / deploy watchdog does not restart-loop the
/// box while it serves the needs-update signal.
pub(crate) fn degraded_router() -> axum::Router {
    degraded_router_with_heartbeat(WsHeartbeatPolicy::default())
}

/// [`degraded_router`] with an explicit heartbeat cadence. Tests use it to run
/// the real loop on a compressed clock; production always takes the [`Default`],
/// exactly as `WsState::with_heartbeat` does on the normal boot path (this mode
/// opens no DB and builds no `AppState`, so there is no `WsState` to read it
/// from — the policy is passed down the router instead).
pub fn degraded_router_with_heartbeat(heartbeat: WsHeartbeatPolicy) -> axum::Router {
    let ws_route = get(move |ws| ws_outdated(ws, heartbeat));
    axum::Router::new()
        .route("/api/v1/ws", ws_route.clone())
        .route("/api/v1/ws/{actor_id}", ws_route.clone())
        // A third-party principal's session sees the same typed signal; its
        // token cannot be verified without the DB either.
        .route(
            fauna_bridge_atproto::oauth_metadata::PATH_PRINCIPAL_WS,
            ws_route,
        )
        .route("/api/v1/health", get(health_outdated))
        .fallback(http_outdated)
}

/// Accept the WS upgrade (selecting the `fauna.v1` subprotocol when offered) and
/// drive the outdated-reply loop. No auth is checked: the authenticated endpoint
/// would need the DB to verify a bearer, and every request is rejected anyway.
///
/// **The frame caps are not optional here, precisely because nothing is
/// authenticated.** `transport-connection.md` § Abuse posture states the 2 MiB
/// message/frame cap as a property of *every* nest WS upgrade
/// (`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`, single-sourced); without
/// them this upgrade would silently take the library default of 64 MiB — 32x
/// wider — and it would buffer that frame in full *before* the loop below gets
/// to reject the request. Degraded mode is the state a nest sits in longest
/// unattended, so this is the last listener that should be the loose one.
async fn ws_outdated(ws: WebSocketUpgrade, heartbeat: WsHeartbeatPolicy) -> Response {
    ws.protocols(["fauna.v1"])
        .max_message_size(crate::routes::MAX_WS_MESSAGE_SIZE)
        .max_frame_size(crate::routes::MAX_WS_MESSAGE_SIZE)
        .on_upgrade(move |socket| handle_ws_outdated(socket, heartbeat))
}

/// Reply to every client Request with `fauna.nest.outdated`. Mirrors the normal
/// per-connection loop's framing (`routes::run_connection`) but with no
/// dispatch, no AppState, and no Push side — the connection only ever emits
/// error Replies correlated to the client's Requests.
///
/// It also mirrors that loop's **heartbeat** (`transport.md` § Connection
/// lifecycle). Degraded mode is the state a nest sits in longest unattended — it
/// serves the needs-update signal until a human deploys a newer binary, while
/// every client in the deployment reconnects against it on a loop — so a socket
/// that outlives its peer here is held for exactly as long as the box stays
/// stale. The peer population is the same one `run_connection` serves (our own
/// apps, native and browser), which is why this is a Ping and not the cheaper
/// inbound-idle timeout: a browser sends nothing at all while idle.
async fn handle_ws_outdated(mut socket: WebSocket, heartbeat: WsHeartbeatPolicy) {
    let mut hb = ServerHeartbeat::new(heartbeat);
    loop {
        let msg = tokio::select! {
            beat = hb.next_beat() => {
                match beat {
                    Beat::Ping => {
                        if socket.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                            break;
                        }
                    }
                    Beat::Dead => {
                        // Logged at `warn` deliberately: a connection reaped in
                        // silence is indistinguishable from a quiet night, which
                        // is how the leak this closes stayed invisible.
                        tracing::warn!(
                            timeout_ms = hb.liveness_timeout().as_millis() as u64,
                            "degraded WS peer answered no heartbeat within the liveness window; \
                             closing dead link",
                        );
                        break;
                    }
                }
                continue;
            }
            msg = socket.next() => msg,
        };
        // Any inbound frame proves the peer is alive, so re-arm before the frame
        // is even inspected — a Pong or a stray Ping counts exactly as much as a
        // Request here.
        hb.re_arm();
        let Some(Ok(msg)) = msg else { break };
        match msg {
            Message::Binary(bytes) => {
                let correlation_id = match decode_frame(&bytes) {
                    Ok(Frame::Request(req)) => req.correlation_id,
                    // A Cancel has no in-flight handler to abort here; ignore it.
                    Ok(Frame::Cancel(_)) => continue,
                    // Reply/Push from a client, or a malformed frame, is a
                    // protocol violation — close, exactly as the normal loop does.
                    _ => break,
                };
                let reply = Frame::Reply(Reply {
                    ty: Reply::TYPE,
                    correlation_id,
                    payload: crate::dispatch_core::err_to_value(RpcError::nest_outdated()),
                    ok: false,
                });
                let Ok(out) = encode_frame(&reply) else {
                    break;
                };
                if socket.send(Message::Binary(out)).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
            // No text frames in the WS-RPC protocol; treat as a violation.
            Message::Text(_) => break,
        }
    }
}

/// 200 liveness — the process is up; the schema is incompatible (surfaced over
/// WS-RPC, never on this query-less probe per the production-HTTP carve-out).
///
/// Carries the same `commit` + `build_id` identity pair as the healthy handler
/// (`crate::build_identity`): a degraded box is precisely the one an admin
/// most needs to identify, and omitting `build_id` here would leave the
/// promotion gate unable to name the artifact that is failing to serve.
async fn health_outdated() -> Response {
    axum::Json(crate::build_identity::identity_json("needs_update")).into_response()
}

/// Any non-WS, non-health HTTP path: a plain 503 so a stray HTTP probe sees the
/// box is intentionally not serving its normal surface.
async fn http_outdated() -> Response {
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "this nest is running an outdated version and must be updated",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::codec::{decode_strict, encode_canonical};
    use fauna_protocol::{Request, Value};

    /// A degraded Reply correlated to the client's Request carries
    /// `ok = false` and decodes to the `fauna.nest.outdated` `RpcError`.
    #[test]
    fn outdated_reply_is_typed_error() {
        let payload = crate::dispatch_core::err_to_value(RpcError::nest_outdated());
        let reply = Reply {
            ty: Reply::TYPE,
            correlation_id: 7,
            payload,
            ok: false,
        };
        let bytes = encode_frame(&Frame::Reply(reply)).unwrap();
        match decode_frame(&bytes).unwrap() {
            Frame::Reply(r) => {
                assert_eq!(r.correlation_id, 7);
                assert!(!r.ok, "degraded reply must be an error");
                // The payload re-decodes to the typed RpcError code.
                let err_bytes = encode_canonical(&r.payload).unwrap();
                let err: RpcError = decode_strict(&err_bytes).unwrap();
                assert_eq!(err.code, RpcError::CODE_NEST_OUTDATED);
            }
            other => panic!("expected Reply, got {other:?}"),
        }
    }

    /// Sanity: a Request frame the loop would receive decodes to its
    /// correlation_id (the value the degraded Reply echoes back).
    #[test]
    fn request_correlation_id_round_trips() {
        let req = Request {
            ty: Request::TYPE,
            correlation_id: 99,
            kind: "fauna.nest.info".into(),
            idempotency_key: [0u8; 16],
            payload: Value::Map(Default::default()),
            replay_forbidden: None,
            deadline_ms: None,
        };
        let bytes = encode_frame(&Frame::Request(req)).unwrap();
        match decode_frame(&bytes).unwrap() {
            Frame::Request(r) => assert_eq!(r.correlation_id, 99),
            other => panic!("expected Request, got {other:?}"),
        }
    }
}
