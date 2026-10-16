//! A nest that completes the anonymous upgrade and then closes it with **4426**
//! — the app offered a `fauna.v<N>` it does not serve — must surface on the
//! first request as the typed [`AnonClientError::SubprotocolMismatch`], the
//! same verdict the pre-upgrade HTTP 426 already gets
//! (`connect_anonymous_classifies_a_426_upgrade_rejection`), never as a
//! retryable `RpcDisconnected` (`transport-connection.md` § Connection
//! lifecycle, the 4426 paragraph; `version-compatibility.md` § Dimension 4).

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use fauna_anon_client::{AnonClientError, AnonymousNestClient};

/// Accept one upgrade echoing `fauna.v1` (as the nest's 4426 path selects the
/// app's own offered token), close it with `code`, then drain until the peer
/// hangs up.
async fn serve_one_close(listener: TcpListener, code: u16) {
    let (stream, _) = listener.accept().await.expect("accept");
    // result_large_err: the callback's Err type is tungstenite's own
    // `ErrorResponse`, fixed by the `Callback` trait signature.
    #[allow(clippy::result_large_err)]
    let echo_fauna_subprotocol =
        |_req: &tokio_tungstenite::tungstenite::handshake::server::Request,
         mut res: tokio_tungstenite::tungstenite::handshake::server::Response| {
            res.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                tokio_tungstenite::tungstenite::http::HeaderValue::from_static("fauna.v1"),
            );
            Ok(res)
        };
    let mut ws = tokio_tungstenite::accept_hdr_async(stream, echo_fauna_subprotocol)
        .await
        .expect("ws upgrade");
    ws.send(Message::Close(Some(CloseFrame {
        code: CloseCode::from(code),
        reason: "subprotocol fauna.v2 required".into(),
    })))
    .await
    .expect("send close");
    while let Some(msg) = ws.next().await {
        if msg.is_err() {
            break;
        }
    }
}

#[tokio::test]
async fn a_4426_close_after_the_upgrade_is_a_subprotocol_mismatch() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(serve_one_close(listener, 4426));

    let client = AnonymousNestClient::connect(&format!("http://127.0.0.1:{port}"))
        .await
        .expect("the upgrade itself completes");
    match client.request::<(), ()>("fauna.nest.info", ()).await {
        Err(AnonClientError::SubprotocolMismatch) => {}
        other => panic!("expected SubprotocolMismatch, got {other:?}"),
    }
}

/// The negative control: any other close stays the ordinary disconnect, so the
/// typed verdict is the close code's and nothing else's.
#[tokio::test]
async fn a_1000_close_after_the_upgrade_stays_a_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(serve_one_close(listener, 1000));

    let client = AnonymousNestClient::connect(&format!("http://127.0.0.1:{port}"))
        .await
        .expect("the upgrade itself completes");
    match client.request::<(), ()>("fauna.nest.info", ()).await {
        Err(AnonClientError::RpcDisconnected { .. } | AnonClientError::WebSocket(_)) => {}
        other => panic!("expected an ordinary disconnect, got {other:?}"),
    }
}
