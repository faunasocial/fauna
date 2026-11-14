//! Reads what a handler left of a request body before the server closes the
//! connection, so the close reaches the client as a clean end and not as a
//! reset that throws the reply away — the nest's twin of the Go bridges'
//! `internal/httpdrain` (`authorization-server.md` § As built → *A refusal
//! before the body drains it*).
//!
//! **The fault it closes.** A route that answers without reading its body — the
//! OAuth endpoints' DPoP gate refuses every correct client's FIRST request
//! before the body is read, deliberately, and the request budget's `429` is
//! written before the handler runs at all — answers on the head alone. When the
//! request said `Connection: close` (Python's urllib always does, as does
//! HTTP/1.0), hyper closes the socket right after that reply, and a body that
//! reaches the server after its head — a loaded client, or one that writes the
//! two apart — meets a closed socket (or one closing over it unread), which
//! answers with RST, not FIN. On Windows an RST makes the client's receive fail
//! (`WSAECONNABORTED` 10053, `WSAECONNRESET` 10054) and discards the reply it
//! had not yet read: the client never sees the refusal, nor the `DPoP-Nonce` it
//! needs to retry. RFC 9112 §9.6 asks a server to keep reading until the client
//! is done for exactly this reason. Measured on Windows: the windows consent test
//! red 2 of 3 at its first PAR post (2026-11-06); through `serve_plain`, a
//! client sending its body 20 ms after the head lost the reply in 196 of 200
//! posts undrained and in none of 200 drained (2026-11-13). Pinned by
//! `tests/oauth_as_refusal_reaches_a_closing_client.rs`.
//!
//! **Bounded both ways, because these routes face anonymous peers:** at most
//! [`DRAIN_MAX_BYTES`] (the request-taking OAuth endpoints' own body cap, so the
//! drain never makes a route read more of a body than the route itself would)
//! and at most [`DRAIN_TIMEOUT`] of waiting. The bytes are discarded unread —
//! nothing is parsed, resolved or stored — so a gate's "before the body is
//! read" ordering is untouched: the drain runs only once the reply is decided.
//! A body past the bound is left as before.
//!
//! **Left alone:** a request with no body; an HTTP/2 or later request, whose
//! stream ends without closing the connection; a client that sent `Expect:
//! 100-continue`, which holds its body until it hears `100 Continue` — reading
//! would ask for a body the route already refused, and none of it is on its
//! way to meet the close; and a handler that KEPT its body (moved into a task
//! that outlives the reply), which still owns it.

use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::Duration;

use axum::body::{Body, BodyDataStream};
use axum::extract::Request;
use axum::http::{HeaderMap, Version, header};
use axum::middleware::Next;
use axum::response::Response;
use futures_util::StreamExt;

/// The most of a leftover body the drain reads: the request-taking OAuth
/// endpoints' own body cap, so the drain never widens what a request can make
/// the nest read. Far above any form a refused request carries (a PAR form is
/// well under a kilobyte).
pub const DRAIN_MAX_BYTES: usize = crate::oauth_as_routes::PAR_MAX_BODY_BYTES;

/// How long the drain waits on a peer that declared a body and is slow to send
/// it — the Go twin's bound.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The handler's body, lent: the handler reads through it, and once the handler
/// has returned and let go of it, what is left is the drain's.
type Lent = Arc<Mutex<Option<BodyDataStream>>>;

/// The middleware (`axum::middleware::from_fn`): run the route, then drain what
/// it left of the body before the reply goes out.
pub async fn drain_unread_body(request: Request, next: Next) -> Response {
    let http1 = matches!(
        request.version(),
        Version::HTTP_09 | Version::HTTP_10 | Version::HTTP_11
    );
    if !http1 || !carries_body(request.headers()) || expects_continue(request.headers()) {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let lent: Lent = Arc::new(Mutex::new(Some(body.into_data_stream())));
    let handlers = Arc::clone(&lent);
    let body = Body::from_stream(futures_util::stream::poll_fn(move |cx| {
        let mut slot = handlers.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_mut() {
            Some(stream) => stream.poll_next_unpin(cx),
            None => Poll::Ready(None),
        }
    }));
    let response = next.run(Request::from_parts(parts, body)).await;
    drain(lent).await;
    response
}

/// Read and discard what is left, within the bounds. A handler still holding
/// its body (the `Arc` has another owner) is left with it.
async fn drain(lent: Lent) {
    let Ok(slot) = Arc::try_unwrap(lent) else {
        return;
    };
    let Some(mut rest) = slot.into_inner().unwrap_or_else(PoisonError::into_inner) else {
        return;
    };
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, async move {
        let mut read = 0usize;
        while read < DRAIN_MAX_BYTES {
            match rest.next().await {
                Some(Ok(chunk)) => read += chunk.len(),
                Some(Err(_)) | None => break,
            }
        }
    })
    .await;
}

/// Whether an HTTP/1 request carries a body at all (RFC 9112 §6.3): a
/// `Transfer-Encoding`, or a `Content-Length` above zero.
fn carries_body(headers: &HeaderMap) -> bool {
    headers.contains_key(header::TRANSFER_ENCODING)
        || headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .is_some_and(|n| n > 0)
}

fn expects_continue(headers: &HeaderMap) -> bool {
    headers
        .get(header::EXPECT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("100-continue"))
}
