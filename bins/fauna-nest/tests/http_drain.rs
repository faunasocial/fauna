//! `fauna_nest::http_drain` — the bounded drain of what a route left of a
//! request body, so a refusal written before the body is read reaches a
//! `Connection: close` client instead of being lost to a reset (the module docs
//! own why). The end-to-end witness over a real socket and the real router is
//! `oauth_as_refusal_reaches_a_closing_client.rs`; these pin the drain's own
//! bounds and the cases it leaves alone.
//!
//! Tier 1 (`testing.md` § The four-tier taxonomy): in-process, no listener.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::header;
use axum::response::Response;
use axum::routing::post;
use fauna_nest::http_drain::{DRAIN_MAX_BYTES, drain_unread_body};
use futures_util::StreamExt;
use tower::ServiceExt; // oneshot

/// A body that counts what was pulled from it, in `chunk`-byte frames.
fn counted(total: usize, chunk: usize, pulled: Arc<AtomicUsize>) -> Body {
    let frames: Vec<Result<Bytes, std::io::Error>> = (0..total.div_ceil(chunk))
        .map(|i| Ok(Bytes::from(vec![b'a'; chunk.min(total - i * chunk)])))
        .collect();
    Body::from_stream(futures_util::stream::iter(frames).inspect(move |f| {
        if let Ok(b) = f {
            pulled.fetch_add(b.len(), Ordering::SeqCst);
        }
    }))
}

fn request(body: Body, len: usize) -> Request {
    Request::builder()
        .method("POST")
        .uri("/r")
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CONNECTION, "close")
        .body(body)
        .unwrap()
}

/// The route that reset the connection: it answers without touching the body
/// (a gate refusal, a budget's `429`).
fn answering_without_reading() -> Router {
    Router::new()
        .route("/r", post(|| async { "refused" }))
        .layer(axum::middleware::from_fn(drain_unread_body))
}

async fn status_and_text(response: Response) -> (u16, String) {
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// What the route left of a small body is read before the reply goes out, and
/// the reply is unchanged.
#[tokio::test]
async fn an_unread_body_is_read_before_the_reply_goes_out() {
    let pulled = Arc::new(AtomicUsize::new(0));
    let response = answering_without_reading()
        .oneshot(request(counted(2_000, 512, Arc::clone(&pulled)), 2_000))
        .await
        .unwrap();
    assert_eq!(
        pulled.load(Ordering::SeqCst),
        2_000,
        "the body the route left unread was not drained"
    );
    assert_eq!(
        status_and_text(response).await,
        (200, "refused".to_string())
    );
}

/// An anonymous peer cannot make the nest read an arbitrary amount: past the
/// bound the drain gives up and the connection closes as before.
#[tokio::test]
async fn the_drain_stops_at_its_bound() {
    let pulled = Arc::new(AtomicUsize::new(0));
    let total = 4 * DRAIN_MAX_BYTES;
    answering_without_reading()
        .oneshot(request(counted(total, 1024, Arc::clone(&pulled)), total))
        .await
        .unwrap();
    let read = pulled.load(Ordering::SeqCst);
    assert!(
        read > 0,
        "nothing was drained — the bound cannot be told from no drain at all"
    );
    assert!(
        read <= DRAIN_MAX_BYTES,
        "the drain read {read} bytes, past its bound of {DRAIN_MAX_BYTES}"
    );
}

/// A client that sent `Expect: 100-continue` has not sent its body: the drain
/// does not ask for it.
#[tokio::test]
async fn an_expect_continue_body_is_not_asked_for() {
    let pulled = Arc::new(AtomicUsize::new(0));
    let mut req = request(counted(2_000, 512, Arc::clone(&pulled)), 2_000);
    req.headers_mut()
        .insert(header::EXPECT, "100-continue".parse().unwrap());
    answering_without_reading().oneshot(req).await.unwrap();
    assert_eq!(
        pulled.load(Ordering::SeqCst),
        0,
        "an Expect: 100-continue body was read"
    );
}

/// A request that declares no body (no `Content-Length`, no
/// `Transfer-Encoding`) is passed straight through, its stream never polled.
#[tokio::test]
async fn a_request_declaring_no_body_is_not_drained() {
    let pulled = Arc::new(AtomicUsize::new(0));
    let req = Request::builder()
        .method("POST")
        .uri("/r")
        .body(counted(2_000, 512, Arc::clone(&pulled)))
        .unwrap();
    answering_without_reading().oneshot(req).await.unwrap();
    assert_eq!(pulled.load(Ordering::SeqCst), 0);
}

/// A body the route read whole reaches it whole through the lent body.
#[tokio::test]
async fn a_body_the_route_read_reaches_it_whole() {
    let app = Router::new()
        .route(
            "/r",
            post(|body: Body| async move {
                axum::body::to_bytes(body, usize::MAX)
                    .await
                    .unwrap()
                    .len()
                    .to_string()
            }),
        )
        .layer(axum::middleware::from_fn(drain_unread_body));
    let pulled = Arc::new(AtomicUsize::new(0));
    let response = app
        .oneshot(request(counted(3_000, 700, Arc::clone(&pulled)), 3_000))
        .await
        .unwrap();
    assert_eq!(status_and_text(response).await, (200, "3000".to_string()));
    assert_eq!(pulled.load(Ordering::SeqCst), 3_000);
}

/// A route that kept its body past the reply (moved it into a task) still owns
/// it: the drain takes nothing from under it.
#[tokio::test]
async fn a_body_the_route_kept_is_not_taken_from_it() {
    let (kept_tx, kept_rx) = tokio::sync::oneshot::channel::<Body>();
    let kept_tx = Arc::new(Mutex::new(Some(kept_tx)));
    let app = Router::new()
        .route(
            "/r",
            post(move |body: Body| {
                let kept_tx = Arc::clone(&kept_tx);
                async move {
                    let tx = kept_tx.lock().unwrap().take().unwrap();
                    let _ = tx.send(body);
                    "kept"
                }
            }),
        )
        .layer(axum::middleware::from_fn(drain_unread_body));
    let pulled = Arc::new(AtomicUsize::new(0));
    app.oneshot(request(counted(2_000, 512, Arc::clone(&pulled)), 2_000))
        .await
        .unwrap();
    assert_eq!(
        pulled.load(Ordering::SeqCst),
        0,
        "the drain read a body its route still held"
    );
    let body = kept_rx.await.unwrap();
    let read = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    assert_eq!(
        read.len(),
        2_000,
        "the route's kept body lost bytes to the drain"
    );
}
