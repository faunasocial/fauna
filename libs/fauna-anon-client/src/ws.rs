//! Minimal tungstenite WebSocket → Bytes Stream+Sink adapter for the
//! **anonymous** (pre-identity) endpoint.
//!
//! This is a pared-down twin of `fauna-client`'s `ws_adapter`: the anonymous
//! connection has no reconnect supervisor, so there is no close-code
//! classification (`ReconnectSignal`) — a `Close` frame simply ends the
//! stream, which the fixed `RpcDispatcher` surfaces as
//! `fauna.protocol.disconnected`.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::{Sink, Stream};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::AnonClientError;
use crate::tls_verify::CaptureHandle;

/// Builds the wss/ws URL for the anonymous (pre-identity) endpoint:
/// `/api/v1/ws` — no `{actor_id}` segment. An anonymous connection has no
/// proven actor to key on (transport.md § Pre-identity).
pub(crate) fn build_anon_ws_url(nest_url: &str) -> String {
    let base = fauna_core::web::http_to_ws(nest_url);
    format!("{base}/api/v1/ws")
}

use crate::tls_dial::ensure_tls_provider;

/// Bounds the connect phase (TCP + TLS + WS-upgrade, whichever of the three
/// branches below runs) of [`connect_anonymous`]. Reuses `dispatch`'s
/// request/reply deadline as a connect analogue — pre-identity connects have
/// no per-kind `KindRegistry` metadata to size a bespoke deadline from, and
/// this crate has exactly one deadline constant to stay consistent with.
/// Without this bound, a blackholed peer (SYN silently dropped, never a
/// connection-refused) hangs the connect for as long as the OS TCP SYN-retry
/// ladder — measured ~127 s on Linux's default `tcp_syn_retries=6` — instead
/// of failing fast; native callers (registration, admin claim, handle-check
/// probes) currently have no caller-side deadline of their own on this path.
const CONNECT_DEADLINE: std::time::Duration = crate::dispatch::DEFAULT_DEADLINE;

/// Connect to the nest's anonymous endpoint with a bearer-less handshake:
/// `Sec-WebSocket-Protocol: fauna.v1` (no `bearer.<token>` element). The
/// connection routes only the fixed pre-identity allowlist (auth bootstrap,
/// public discovery, registration, admin claim, invite, storage-mode); an
/// off-allowlist kind gets `fauna.protocol.unauthenticated` without tearing
/// the connection down (transport.md § Pre-identity).
///
/// Over `wss://` the TLS handshake uses the **capturing verifier**
/// ([`crate::tls_verify::capturing_client_config`]): it provisionally accepts the
/// cert (encrypt-only) and records its SPKI + WebPKI validity into the returned
/// [`CaptureHandle`], so the caller can authenticate the connection via the
/// in-band channel binding ([`crate::trust::graduate_handshake`]). This replaces
/// the old `FAUNA_INSECURE_TLS=1 → accept any cert` path (security.md
/// § Transport trust). A plain `ws://` dev nest has no TLS, so the handle stays
/// at its default (no cert captured).
///
/// `resolve`, when `Some`, dials that `SocketAddr` directly instead of resolving
/// `nest_url`'s host via system DNS — `request`'s URI (hence SNI + `Host` +
/// cert-identity) is still built from `nest_url`, so only the socket target
/// changes. Lets a caller reach a freshly-provisioned box by its known IP
/// before its DNS record has propagated; MITM-safe because the capturing
/// verifier's channel binding authenticates the box by identity, not by the
/// address dialed (security.md § Transport trust Axis 1).
pub(crate) async fn connect_anonymous(
    nest_url: &str,
    resolve: Option<std::net::SocketAddr>,
) -> Result<(AnonWsAdapter, CaptureHandle), AnonClientError> {
    let url = build_anon_ws_url(nest_url);

    let mut request: Request = url
        .clone()
        .into_client_request()
        .map_err(|e| AnonClientError::WebSocket(format!("invalid ws url: {e}")))?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static("fauna.v1"),
    );

    // rustls 0.23 needs a process-default CryptoProvider before the first
    // `wss://` handshake; install ring once (no-op on a plain `ws://` dev nest).
    ensure_tls_provider();

    let (config, capture) = crate::tls_verify::capturing_client_config();
    // Cap inbound message/frame size symmetric with nest's 2 MiB client-WS-RPC
    // limit — pre-identity replies are tiny, so 2 MiB is far above any
    // legitimate frame (shared with the authenticated connects below and in
    // `fauna-client`, `fauna_ws_substrate::handshake::rpc_ws_config`).
    let ws_config = fauna_ws_substrate::rpc_ws_config();
    // The process-wide dial budget, taken before the deadline below starts so a
    // paced dial never eats its own connect bound
    // (`fauna_ws_substrate::dial_budget` — every native dial to a nest passes it).
    let budget_key = fauna_ws_substrate::dial_budget::nest_key(request.uri());
    let started = std::time::Instant::now();
    fauna_ws_substrate::dial_budget::acquire(&budget_key).await;
    let budget_wait = started.elapsed();
    let nest_for_log = budget_key.clone();
    // `Box::pin` the deep TLS + WS-upgrade handshake future onto the heap so the
    // enclosing `connect_anonymous` future (and everything that holds it inline up
    // the chain — `AnonymousNestClient::connect`, `mint_bearer_over_handshake`, the
    // `fauna-ffi` `mint_bearer` / silent-challenge / registration exports) stays
    // small. The native UniFFI apps poll this connect future on the foreign
    // async executor's thread (Swift's cooperative pool / Kotlin's dispatcher),
    // whose stack is much smaller than a tokio worker's 2 MB — `async_runtime =
    // "tokio"` only supplies a tokio *context*, it does NOT move polling onto a
    // worker thread. The inline rustls + tungstenite handshake state machine is
    // large enough to overflow that small stack under load (FaunaMacOS SIGBUS
    // "Could not determine thread index for stack guard region", 2026-06-16).
    // Boxing keeps the big state machine off the stack — the shared root fix for
    // every anon-WS caller (#2). See docs/goal/architecture/apps/native-async-execution.md.
    //
    // The whole branch (TCP connect + TLS/WS-upgrade, whichever runs) is
    // wrapped in one `CONNECT_DEADLINE` bound from the outside — deliberately
    // external to `tokio_tungstenite`'s own connect futures, none of which take
    // a deadline, so bounding must happen at this call site regardless of what
    // any one branch does internally.
    let (ws_stream, _resp) = tokio::time::timeout(CONNECT_DEADLINE, async move {
        if let Some(addr) = resolve {
            // Temporary DNS override: dial `addr` directly (`request`'s URI —
            // hence SNI/`Host`/cert-identity — is untouched, built from
            // `nest_url` above). `client_async_tls_with_config` mode-dispatches
            // on the URL scheme (`tungstenite::client::uri_mode`), so passing a
            // Rustls connector even for a plain `ws://` override is harmless —
            // it wraps the socket as `MaybeTlsStream::Plain` for a non-TLS
            // scheme.
            let tcp = TcpStream::connect(addr)
                .await
                .map_err(|e| AnonClientError::WebSocket(format!("tcp connect {addr}: {e}")))?;
            let connector = tokio_tungstenite::Connector::Rustls(config);
            Box::pin(tokio_tungstenite::client_async_tls_with_config(
                request,
                tcp,
                Some(ws_config),
                Some(connector),
            ))
            .await
        } else if url.starts_with("wss://") {
            let connector = tokio_tungstenite::Connector::Rustls(config);
            Box::pin(tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(ws_config),
                false,
                Some(connector),
            ))
            .await
        } else {
            // Plain `ws://` — no TLS, nothing to capture.
            Box::pin(tokio_tungstenite::connect_async_with_config(
                request,
                Some(ws_config),
                false,
            ))
            .await
        }
        .map_err(|e| {
            fauna_ws_substrate::dial_budget::note_dial_error(&budget_key, &e);
            AnonClientError::WebSocket(format!("ws connect: {e}"))
        })
    })
    .await
    .map_err(|_elapsed| {
        AnonClientError::WebSocket(format!("connect timed out after {CONNECT_DEADLINE:?}"))
    })??;

    // The one timing line a slow mint needs: budget wait vs the connect itself
    // (TCP + TLS + WS upgrade), so "the nest is slow" and "this process is
    // paced" read apart.
    tracing::debug!(
        nest = %nest_for_log,
        budget_wait_ms = budget_wait.as_millis() as u64,
        connect_ms = (started.elapsed() - budget_wait).as_millis() as u64,
        "anonymous connect up"
    );
    Ok((AnonWsAdapter { inner: ws_stream }, capture))
}

/// Connect to the nest's **authenticated** endpoint with the subprotocol-bearer
/// handshake: `Sec-WebSocket-Protocol: fauna.v1, bearer.<token>`, routing the
/// full authenticated kind set. The bearer twin of [`connect_anonymous`], and
/// the leaf-crate sibling of `fauna-client`'s `connect_with_subprotocol_bearer`.
///
/// **Trust (security.md § Cross-connection binding):** the bearer rides a
/// *different* TLS connection than the `fauna.auth.handshake` that minted it
/// ([`crate::mint_bearer_over_handshake`], which graduates the binding). Over
/// `wss://`, if that handshake graduated a bound SPKI for this host (the
/// self-signed / DNS-`self=` path), require the bearer connection to present that
/// exact SPKI ([`crate::pinned_spki`]) — a MITM cannot complete TLS as a cert
/// whose key it lacks. Otherwise fall through to strict WebPKI
/// (`connect_async_with_config`): a public-CA nest is authenticated the boring
/// way, and a non-WebPKI cert with no pin is **rejected**, never accept-any.
/// (Unlike [`connect_anonymous`]'s capturing verifier — which provisionally
/// accepts any cert — this connect has a real, already-graduated identity to hold
/// the transport to, so accept-any would be a downgrade.)
pub(crate) async fn connect_authed(
    nest_url: &str,
    actor_id_hex: &str,
    token: &str,
) -> Result<AnonWsAdapter, AnonClientError> {
    let url = fauna_ws_substrate::actor_ws_url(nest_url, actor_id_hex);

    let mut request: Request = url
        .clone()
        .into_client_request()
        .map_err(|e| AnonClientError::WebSocket(format!("invalid ws url: {e}")))?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        fauna_ws_substrate::bearer_subprotocol_header(token)
            .map_err(|e| AnonClientError::WebSocket(format!("bad bearer header: {e}")))?,
    );

    // Symmetric with nest's 2 MiB client-WS-RPC limit (as `connect_anonymous`).
    let ws_config = fauna_ws_substrate::rpc_ws_config();

    // The shared pinned-or-strict-WebPKI dial (layer 1 — no graduate-retry:
    // this connect's own caller is the mint path that graduates).
    let pin = crate::tls_dial::resolve_graduated_pin(&request);
    let (ws_stream, _resp) = crate::tls_dial::dial_ws_trusted(request, ws_config, pin)
        .await
        .map_err(|e| AnonClientError::WebSocket(format!("ws connect: {e}")))?;

    Ok(AnonWsAdapter { inner: ws_stream })
}

/// Bytes-shaped Stream+Sink wrapping a tungstenite WebSocketStream. `Stream`
/// yields `Bytes` for each incoming binary frame (text/ping/pong are filtered);
/// `Sink<Bytes>` writes each Bytes payload as a binary WS frame.
pub(crate) struct AnonWsAdapter {
    inner: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AdapterError {
    #[error("ws transport: {0}")]
    Transport(String),
}

impl AdapterError {
    fn transport(e: impl std::fmt::Display) -> Self {
        Self::Transport(e.to_string())
    }
}

impl Stream for AnonWsAdapter {
    type Item = Result<Bytes, AdapterError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(Message::Binary(b)))) => {
                    return Poll::Ready(Some(Ok(b)));
                }
                // A close frame ends the stream; the dispatcher synthesises
                // `fauna.protocol.disconnected` for any in-flight call.
                Poll::Ready(Some(Ok(Message::Close(_)))) => return Poll::Ready(None),
                // Non-binary frames are not part of the wire protocol; skip.
                Poll::Ready(Some(Ok(
                    Message::Text(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_),
                ))) => continue,
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(AdapterError::transport(e))));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl Sink<Bytes> for AnonWsAdapter {
    type Error = AdapterError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner)
            .poll_ready(cx)
            .map_err(AdapterError::transport)
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        Pin::new(&mut self.get_mut().inner)
            .start_send(Message::Binary(item))
            .map_err(AdapterError::transport)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner)
            .poll_flush(cx)
            .map_err(AdapterError::transport)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner)
            .poll_close(cx)
            .map_err(AdapterError::transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_anon_ws_url_swaps_scheme_and_omits_actor_id() {
        assert_eq!(
            build_anon_ws_url("https://nest.example.com"),
            "wss://nest.example.com/api/v1/ws"
        );
        assert_eq!(
            build_anon_ws_url("http://localhost:8080"),
            "ws://localhost:8080/api/v1/ws"
        );
    }

    /// Regression guard for the cooperative-pool stack overflow (FaunaMacOS
    /// SIGBUS "Could not determine thread index for stack guard region",
    /// 2026-06-16). The native UniFFI apps poll `connect_anonymous` on a
    /// foreign async-executor thread (Swift's cooperative pool / Kotlin's
    /// dispatcher) whose stack is far smaller than a tokio worker's, so the
    /// future MUST stay small: the deep `rustls` + `tungstenite` TLS/WS handshake
    /// state machine is `Box::pin`ned onto the heap (see `connect_anonymous`)
    /// instead of held inline. If a refactor un-boxes it the future balloons by
    /// several KB and the overflow returns — this pins the fix. The threshold is
    /// generous vs. the boxed size yet far below the un-boxed size.
    #[test]
    fn connect_anonymous_future_stays_small_for_foreign_stacks() {
        let fut = connect_anonymous("wss://nest.example.com", None);
        let size = std::mem::size_of_val(&fut);
        // Boxed today: ~296 bytes. Un-boxed (the bug): ~10.9 KB — held inline up
        // the mint/connect chain on the small foreign cooperative-pool stack.
        assert!(
            size <= 2048,
            "connect_anonymous future is {size} bytes — expected <= 2048. The \
             deep TLS+WS handshake future must stay Box::pin'd (ws.rs) so it does \
             not overflow the small foreign cooperative-pool stack the native \
             UniFFI clients poll it on (see the connect_anonymous comment)."
        );
    }

    /// Regression guard for the connect-phase deadline. Before this fix,
    /// `connect_anonymous` had no bound at all on the DNS-override branch
    /// (measured: a genuinely blackholed peer hangs ~127 s on Linux's default
    /// `tcp_syn_retries=6`) — this exercises the plain-`ws://` branch instead,
    /// which needs no external network: a local listener accepts the TCP
    /// connection (so the hang is NOT a fast connection-refused) but never
    /// completes the WS upgrade, so without the `CONNECT_DEADLINE` wrap the
    /// call would hang forever on the real never-arriving response. Paused
    /// time (established pattern: `download_file_bytes_test.rs`) makes the
    /// real 30 s deadline resolve instantly in test wall-clock time.
    #[tokio::test(start_paused = true)]
    async fn connect_anonymous_bounds_a_blackholed_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral listener");
        let addr = listener.local_addr().expect("listener local_addr");

        // Accept the connection but never write the WS-upgrade response, and
        // never drop the stream — a real peer that never speaks, not a
        // connection-refused shortcut.
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            std::future::pending::<()>().await;
            drop(stream); // unreachable — keeps `stream` alive for the compiler
        });

        let started = tokio::time::Instant::now();
        let result = connect_anonymous(&format!("http://{addr}"), None).await;
        let elapsed = started.elapsed();

        // `result`'s `Ok` side (`AnonWsAdapter`) isn't `Debug`, so match instead
        // of formatting the whole `Result` in the assertion message.
        match &result {
            Err(AnonClientError::WebSocket(msg)) if msg.contains("timed out") => {}
            Err(other) => panic!("expected a connect-timeout WebSocket error, got Err({other:?})"),
            Ok(_) => panic!(
                "expected a connect-timeout WebSocket error, but connect succeeded — the \
                 blackholed listener didn't actually block the WS upgrade"
            ),
        }
        assert!(
            elapsed >= CONNECT_DEADLINE,
            "timeout fired before the deadline: {elapsed:?} < {CONNECT_DEADLINE:?}"
        );
    }
}
