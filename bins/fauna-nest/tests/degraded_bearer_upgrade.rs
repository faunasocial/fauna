//! Integration test — the **degraded** ("needs-update") listener refuses a
//! bearer-authenticated WS upgrade with `401`, and keeps answering the anonymous
//! surface with the typed `fauna.nest.outdated` reply loop
//! (`version-compatibility.md` § 2.2: the degraded nest "rejects connects with
//! the typed `fauna.nest.outdated`").
//!
//! **Why the bearer route is refused rather than served.** The degraded boot
//! opens no database, so it cannot verify any bearer. Accepting the upgrade
//! anyway opened a session that looked live to the app — its reconnect
//! supervisor reported Connected — while every request in it was refused one at
//! a time, so a signed-in app never learned that its nest needed updating, only
//! that each screen had failed. A `401` is what a healthy nest answers a bearer
//! it cannot honour (a factory reset that wiped its token store): the app's
//! supervisor re-mints over the silent challenge, which this nest answers with
//! the typed `fauna.nest.outdated`, and that refusal ends the session onto the
//! app's non-retry update surface (`fauna-client`'s `connect_error_is_terminal`).

use fauna_nest::ws::WsHeartbeatPolicy;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

async fn start() -> String {
    let app =
        fauna_nest::degraded_serve::degraded_router_with_heartbeat(WsHeartbeatPolicy::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("ws://{addr}")
}

async fn upgrade(url: String, protocols: &str) -> Result<(), u16> {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, protocols.parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Ok(_) => Ok(()),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => Err(resp.status().as_u16()),
        Err(other) => panic!("unexpected upgrade failure: {other}"),
    }
}

#[tokio::test]
async fn the_bearer_upgrade_is_refused_with_401() {
    let base = start().await;
    let actor = "ab".repeat(32);
    assert_eq!(
        upgrade(
            format!("{base}/api/v1/ws/{actor}"),
            "fauna.v1, bearer.some-token"
        )
        .await,
        Err(401),
        "a degraded nest cannot verify a bearer, so it must not open a session on one"
    );
}

#[tokio::test]
async fn the_anonymous_upgrade_still_opens() {
    let base = start().await;
    assert_eq!(
        upgrade(format!("{base}/api/v1/ws"), "fauna.v1").await,
        Ok(()),
        "the anonymous surface keeps answering fauna.nest.outdated in-session"
    );
}
