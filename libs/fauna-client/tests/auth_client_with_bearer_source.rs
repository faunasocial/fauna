//! `AuthClient::with_bearer_source` accepts an arbitrary `BearerSource`.
//!
//! The desktop apps construct `AuthClient` over their existing
//! bearer cache (`LaunchMachineBearer`) so HTTP + WS share one
//! `/api/v1/auth/token` cache + one TTL-refresh loop + one 4xx-reactive
//! invalidation path. This test stands the constructor up over
//! `fauna_nest_http::StaticBearer` — the minimal `BearerSource` impl —
//! and asserts `ensure_auth()` returns the source's bearer without
//! reaching the network, and `clear_token()` no-ops (the trait default
//! for sources that can't refresh).

use std::sync::Arc;

use fauna_client::AuthClient;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::StaticBearer;

#[tokio::test]
async fn ensure_auth_returns_static_bearer_without_round_trip() {
    let kp = ActorKeypair::from_secret([7u8; 32]);
    let bearer: Arc<dyn fauna_nest_http::BearerSource> =
        Arc::new(StaticBearer("static-token".into()));
    let http = reqwest::Client::new();

    // Unreachable nest URL — if the impl tried to hit the network for a
    // token, `ensure_auth` would fail with a transport error. `StaticBearer`
    // never makes a request, so the test passes regardless of reachability.
    let client =
        AuthClient::with_bearer_source("http://127.0.0.1:1".into(), kp, Arc::clone(&bearer), http);

    let token = client.ensure_auth().await.expect("ensure_auth");
    assert_eq!(token, "static-token");
}

#[tokio::test]
async fn clear_token_is_a_noop_on_static_bearer() {
    let kp = ActorKeypair::from_secret([8u8; 32]);
    let bearer: Arc<dyn fauna_nest_http::BearerSource> =
        Arc::new(StaticBearer("immutable-token".into()));
    let http = reqwest::Client::new();

    let client =
        AuthClient::with_bearer_source("http://127.0.0.1:1".into(), kp, Arc::clone(&bearer), http);

    // `BearerSource::notify_401`'s trait default is a no-op; `StaticBearer`
    // doesn't override it. After "clear", the same static token is still
    // returned — proving the cache (such as it is) wasn't invalidated.
    let before = client.ensure_auth().await.expect("ensure_auth before");
    client.clear_token().await;
    let after = client.ensure_auth().await.expect("ensure_auth after");
    assert_eq!(before, "immutable-token");
    assert_eq!(after, "immutable-token");
}
