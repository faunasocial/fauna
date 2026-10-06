//! The client's bearer-subprotocol *connect* step.
//!
//! The substrate-neutral transport — the tungstenite `WebSocketStream` ⇄
//! `Bytes` adapter, the 30 s Ping / 60 s dead-link heartbeat, and the
//! close-code → [`ReconnectSignal`] mapping — lives in [`fauna_ws_substrate`]
//! (shared with the nest↔nest federation channel), which also supplies the
//! bearer-handshake pieces this module needs (`{wss|ws}://<nest>/api/v1/ws/
//! {actor_id}`, the `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>` header,
//! and the size-capped `WebSocketConfig` — shared with `fauna-anon-client`'s
//! authenticated-endpoint connect, `fauna_ws_substrate::handshake`). This
//! module keeps only what is genuinely client-specific: the cross-connection
//! SPKI pin for the self-signed / LAN trust path. The types are re-exported so
//! existing `fauna_client::ws_adapter::…` paths keep resolving.

use fauna_ws_substrate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, TungsteniteAdapter};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;

use crate::auth_client::AuthClient;
use crate::error::NestClientError;

pub use fauna_ws_substrate::{AdapterError, ReconnectSignal};

/// Connect to the nest with the subprotocol-bearer handshake.
/// Returns the wrapped Bytes stream/sink ready to feed `RpcDispatcher::new`.
pub(crate) async fn connect_with_subprotocol_bearer(
    auth: &AuthClient,
) -> Result<TungsteniteAdapter, NestClientError> {
    let token = auth.ensure_auth().await.map_err(map_bearer_err)?;
    let actor_id = auth.actor_id_hex();
    // `nest_url()` reads the current (SRV-reconnect-swappable) value; bind it
    // once so the WS URL build + the cross-binding authority below see the same
    // snapshot even if a recovery swaps the cell mid-connect.
    let nest_url = auth.nest_url();
    let url = fauna_ws_substrate::actor_ws_url(&nest_url, &actor_id);

    // WHICH IDENTITY this connection speaks as — the one fact no log carried,
    // and the reason a whole class of wrong-actor bug reads as a feature bug.
    // The nest scopes every caller-scoped read to the connection's actor
    // (`fauna.media.list` → `enumerate_readable_folders(caller)`, and its
    // siblings), so a session that reconnects as the *outgoing* actor after an
    // identity change answers `Ok` with an empty set — indistinguishable, from
    // the app's side, from "you genuinely have nothing". One line per connect
    // (connects are rare and back off), actor id only: it is the user's own
    // public identifier, never secret material (§ Persistence & privacy).
    tracing::info!(actor = %actor_id, "authenticated WS connect");

    let mut request: Request = url
        .into_client_request()
        .map_err(|e| NestClientError::WebSocket(format!("invalid ws url: {e}")))?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        fauna_ws_substrate::bearer_subprotocol_header(&token)
            .map_err(|e| NestClientError::WebSocket(format!("bad bearer header: {e}")))?,
    );

    let ws_config = fauna_ws_substrate::rpc_ws_config();

    // Cross-connection binding (security.md § Cross-connection binding): the
    // bearer rides a *different* TLS connection than the `fauna.auth.handshake`
    // that authenticated the nest, so it must present the SPKI a prior
    // handshake graduated (or WebPKI-authenticate the boring way) — the shared
    // trusted dial, including the § Pin custody graduate-and-retry fallback
    // for a process that holds a bearer but no identity key (the apple File
    // Provider extension, a background agent).
    let (ws_stream, _resp) =
        fauna_anon_client::tls_dial::dial_ws_trusted_with_graduate_retry(request, ws_config)
            .await
            .map_err(map_ws_connect_err)?;

    Ok(TungsteniteAdapter::new(
        ws_stream,
        KEEPALIVE_INTERVAL,
        KEEPALIVE_TIMEOUT,
    ))
}

/// Map a bearer-acquisition failure on the connect path.
///
/// Everything becomes [`NestClientError::Auth`] carrying the cause, as it
/// always did — except the nest-identity verdict, which is passed through
/// **unchanged**. Re-wrapping that one as `Auth` is not cosmetic: it is what
/// hides it from [`crate::reconnect::ClientChannel::connect_error_is_terminal`],
/// leaving the supervisor to back off and re-sign a doomed handshake forever on
/// a possible MITM (`security.md` § Post-auth surfacing — the plumbing rule
/// that the typed verdict must survive every seam).
///
/// A named function rather than an inline closure so the seam is reachable from
/// a test: the wrapping arm compiles just as happily either way, so only an
/// assertion can tell the two apart.
fn map_bearer_err(e: NestClientError) -> NestClientError {
    match e {
        e @ NestClientError::NestIdentityChanged { .. } => e,
        other => NestClientError::Auth(format!("ws bearer: {other}")),
    }
}

/// Map a WS-upgrade failure to a [`NestClientError`], distinguishing a server
/// **`401`** or **`426`** (both rejected at the *upgrade*, before any close
/// frame is possible — axum's `WebSocketUpgrade` can't set a close code
/// pre-handshake) from a transport failure.
///
/// A `401` is surfaced as [`NestClientError::Api`] so the reconnect
/// supervisor can recognise it (`reconnect::ClientChannel::
/// connect_error_is_auth_rejection`) and re-mint the bearer before retrying,
/// rather than backing off forever with the dead token. This is the only path
/// by which a stale bearer is rejected after a nest factory-reset wipes its
/// token store: the rejection happens at the HTTP upgrade, so the WS never
/// opens and no `4401` close code is ever seen.
///
/// A `426` — the client's subprotocol is genuinely incompatible with the
/// nest's (a fork, or a major-version skew) — is surfaced as
/// [`NestClientError::SubprotocolMismatch`], the same error an *already-open*
/// connection would get from a `4426` close (`ReconnectSignal::
/// SubprotocolMismatch`, `fauna_ws_substrate::adapter`). No retry can fix a
/// version mismatch, so `connect_error_is_terminal` treats both sources the
/// same way (`transport.md` § Close codes).
fn map_ws_connect_err(e: tokio_tungstenite::tungstenite::Error) -> NestClientError {
    use tokio_tungstenite::tungstenite::Error as WsErr;
    if let WsErr::Http(ref resp) = e {
        let status = resp.status().as_u16();
        // 429: the nest is throttling this actor's upgrades. Not an auth
        // rejection (no re-mint), not terminal — typed so the supervisor knows
        // the nest answered; its `Retry-After` is already held by the dial
        // budget (`tls_dial::dial_ws_trusted` → `dial_budget::note_dial_error`).
        if status == 401 || status == 429 {
            return NestClientError::Api {
                status,
                message: format!("ws upgrade rejected: {e}"),
            };
        }
        if status == 426 {
            return NestClientError::SubprotocolMismatch;
        }
    }
    NestClientError::WebSocket(format!("ws connect: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The connect path's bearer seam must not launder the identity verdict
    /// into `Auth`. This is the link between "the mint produced the verdict"
    /// and "the supervisor stopped": `connect_error_is_terminal` reads the
    /// error value, so a wrapped verdict is an invisible one.
    #[test]
    fn the_identity_verdict_survives_the_connect_paths_bearer_seam() {
        let e = map_bearer_err(NestClientError::NestIdentityChanged {
            host: "nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: Some("bb".repeat(32)),
        });
        let NestClientError::NestIdentityChanged { host, .. } = e else {
            panic!("the verdict must pass through unchanged, got {e:?}");
        };
        assert_eq!(host, "nest.example");
    }

    /// …while every other bearer failure keeps the `Auth`-wrapped shape the
    /// supervisor's other arms expect, cause text and all.
    #[test]
    fn other_bearer_failures_keep_their_auth_wrapping() {
        let e = map_bearer_err(NestClientError::RpcTimeout);
        let NestClientError::Auth(msg) = e else {
            panic!("expected Auth, got {e:?}");
        };
        assert!(msg.contains("ws bearer"), "got {msg}");
    }

    /// A real upgrade rejection — a raw TCP listener answering HTTP 426 to the
    /// upgrade request, not a synthesized enum variant — must classify as
    /// [`NestClientError::SubprotocolMismatch`], the same way the nest's own
    /// pre-upgrade 426 (`transport.md` § Close codes) does in production.
    /// Regression for the row this closes: before the fix, 426 fell through to
    /// the generic `WebSocket` branch and the reconnect supervisor backed off
    /// forever instead of failing fast (`reconnect::tests::
    /// subprotocol_mismatch_is_terminal` pins the other half).
    #[tokio::test]
    async fn map_ws_connect_err_classifies_a_real_426_upgrade_rejection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await; // drain the upgrade request
            stream
                .write_all(b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        let url = format!("ws://{addr}/api/v1/ws/deadbeef");
        let err = tokio_tungstenite::connect_async(url).await.unwrap_err();
        server.await.unwrap();

        assert!(
            matches!(
                map_ws_connect_err(err),
                NestClientError::SubprotocolMismatch
            ),
            "a real HTTP 426 upgrade rejection must classify as SubprotocolMismatch"
        );
    }
}
