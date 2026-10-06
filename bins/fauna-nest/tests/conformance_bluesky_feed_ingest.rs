//! tier_3 for the consume-side Bluesky feed poller (`bridges.md` § Unified feed
//! ingestion → *Bridge ingestion*) — the network half the headless
//! `feed_ingest.rs` / `ingest.rs` pins cannot reach.
//!
//! The session is **earned, never seeded**: `fauna.bridges.link` runs the real
//! `BlueskyProvider::link` → `authorize()` against the canned far end (the same
//! `send_http` seam `conformance_bluesky_link.rs` uses), then the real OAuth
//! callback route exchanges a code at the canned token endpoint — a DPoP-nonce
//! challenge first, as a real authorization server answers — so the session
//! store holds whatever atrium itself writes. `feed_worker::poll_bluesky_feeds`
//! then restores that session and reads `getTimeline` / `getFeed` from the
//! canned PDS.
//!
//! Tier: tier_3 (real `AppState` + in-memory `CacheDb` + real atproto OAuth
//! client; only the HTTP far end is canned). Feature-gated `bluesky`.
#![cfg(feature = "bluesky")]

mod common;
use common::dispatch;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use bytes::Bytes;
use tower::ServiceExt;

use fauna_bridge_atproto::keypair::Es256Keypair;
use fauna_bridge_atproto::oauth::{
    BlueskyOAuthConfig, FakeHttpResponder, build_oauth_client_with_responder, http,
};
use fauna_bridge_atproto::test_support::test_cid;
use fauna_nest::{
    bluesky::{
        auth_routes, bridge_provider::BlueskyProvider, db_helpers, feed_worker, init_db,
        storage_backend::CacheDbBackend,
    },
    bridge_management::{BridgeProviderRegistry, LinkReply},
    bridges_ui_handlers,
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    Value,
    bridges_ui::{CreateFeedRequest, LinkRequest},
    decode_strict as decode, encode_canonical,
};

const HANDLE: &str = "alice.test";
const DID: &str = "did:plc:faketestbridgeuser234567";
const PDS: &str = "https://pds.alice.test";
const ISSUER: &str = "https://auth.alice.test";
const AUTH_ENDPOINT: &str = "https://auth.alice.test/oauth/authorize";
const PAR_ENDPOINT: &str = "https://auth.alice.test/oauth/par";
const TOKEN_ENDPOINT: &str = "https://auth.alice.test/oauth/token";
const REQUEST_URI: &str = "urn:ietf:params:oauth:request_uri:faketestbridge";
const DPOP_NONCE: &str = "fake-dpop-nonce-1";

const DOMAIN: &str = "nest.test";
const DOMAIN_URL: &str = "https://nest.test";

/// The custom feed the actor subscribes to.
const FEED_URI: &str = "at://did:plc:feedgenerator2345678901/app.bsky.feed.generator/cats";

const ACTOR: [u8; 32] = [7u8; 32];

fn resp(status: u16, body: Vec<u8>) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

/// What the far end saw, for the assertions that the flow went where it should.
#[derive(Default)]
struct Seen {
    /// The PAR form body's `state` — what the authorization server will echo
    /// on the redirect back to the callback.
    par_state: Option<String>,
    /// Every token-endpoint POST's DPoP proof carried a nonce (`true`) or not.
    token_nonces: Vec<bool>,
    get_timeline: usize,
    get_feed: usize,
}

/// A `FeedViewPost` whose post is by `did`, keyed by `rkey`.
fn feed_item(did: &str, handle: &str, rkey: &str, text: &str, seed: u8) -> serde_json::Value {
    serde_json::json!({
        "post": {
            "uri": format!("at://{did}/app.bsky.feed.post/{rkey}"),
            "cid": test_cid(seed),
            "author": { "did": did, "handle": handle },
            "record": {
                "$type": "app.bsky.feed.post",
                "text": text,
                "createdAt": "2026-09-29T10:00:00.000Z",
            },
            "indexedAt": "2026-09-29T10:00:01.000Z",
        }
    })
}

const BOB: &str = "did:plc:bobfaketestfollowed2345";
const CAROL: &str = "did:plc:carolfaketestfeedposter2";

/// The two timeline posts and the one custom-feed post, as `(at_uri, text)`.
fn timeline_uris() -> [String; 2] {
    [
        format!("at://{BOB}/app.bsky.feed.post/t1"),
        format!("at://{BOB}/app.bsky.feed.post/t2"),
    ]
}
fn feed_uri_post() -> String {
    format!("at://{CAROL}/app.bsky.feed.post/f1")
}

/// Did the request's `DPoP` proof JWT carry a `nonce` claim?
fn dpop_has_nonce(req: &http::Request<Vec<u8>>) -> bool {
    use base64::Engine;
    let Some(jwt) = req.headers().get("dpop").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(payload) = jwt.split('.').nth(1) else {
        return false;
    };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("nonce").cloned())
        .is_some()
}

/// The canned atproto far end: the five `authorize()` hops of
/// `conformance_bluesky_link.rs`, then the token endpoint (a `use_dpop_nonce`
/// challenge before the token reply), then the PDS's `getProfile` (the
/// callback's handle lookup), `getTimeline` and `getFeed`.
fn fake_far_end(seen: Arc<Mutex<Seen>>, feed_outage: Arc<AtomicBool>) -> FakeHttpResponder {
    Arc::new(move |req: &http::Request<Vec<u8>>| {
        let uri = req.uri().to_string();
        let method = req.method().as_str();

        if method == "GET" && uri.contains("alice.test/.well-known/atproto-did") {
            return resp(200, DID.as_bytes().to_vec());
        }
        if method == "GET" && uri.contains("plc.directory") {
            let doc = serde_json::json!({
                "id": DID,
                "alsoKnownAs": [format!("at://{HANDLE}")],
                "service": [{
                    "id": "#atproto_pds",
                    "type": "AtprotoPersonalDataServer",
                    "serviceEndpoint": PDS,
                }],
            });
            return resp(200, doc.to_string().into_bytes());
        }
        if method == "GET" && uri.contains("pds.alice.test/.well-known/oauth-protected-resource") {
            let meta = serde_json::json!({
                "resource": PDS,
                "authorization_servers": [ISSUER],
                "scopes_supported": [],
            });
            return resp(200, meta.to_string().into_bytes());
        }
        if method == "GET" && uri.contains("auth.alice.test/.well-known/oauth-authorization-server")
        {
            let meta = serde_json::json!({
                "issuer": ISSUER,
                "authorization_endpoint": AUTH_ENDPOINT,
                "token_endpoint": TOKEN_ENDPOINT,
                "scopes_supported": ["atproto", "transition:chat.bsky"],
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["private_key_jwt", "none"],
                "token_endpoint_auth_signing_alg_values_supported": ["ES256"],
                "dpop_signing_alg_values_supported": ["ES256"],
                "pushed_authorization_request_endpoint": PAR_ENDPOINT,
                "require_pushed_authorization_requests": true,
            });
            return resp(200, meta.to_string().into_bytes());
        }
        if method == "POST" && uri.contains("/oauth/par") {
            let state = url::form_urlencoded::parse(req.body())
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.into_owned());
            seen.lock().unwrap().par_state = state;
            let body = serde_json::json!({ "request_uri": REQUEST_URI, "expires_in": 299 });
            return resp(201, body.to_string().into_bytes());
        }
        if method == "POST" && uri.starts_with(TOKEN_ENDPOINT) {
            let has_nonce = dpop_has_nonce(req);
            seen.lock().unwrap().token_nonces.push(has_nonce);
            if !has_nonce {
                // RFC 9449 § 8: the server demands a nonce and the client retries.
                return http::Response::builder()
                    .status(400)
                    .header("content-type", "application/json")
                    .header("DPoP-Nonce", DPOP_NONCE)
                    .body(
                        serde_json::json!({ "error": "use_dpop_nonce" })
                            .to_string()
                            .into_bytes(),
                    )
                    .unwrap();
            }
            let body = serde_json::json!({
                "access_token": "fake-access-token",
                "token_type": "DPoP",
                "expires_in": 3600,
                "refresh_token": "fake-refresh-token",
                "scope": "atproto transition:chat.bsky",
                "sub": DID,
            });
            return http::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .header("DPoP-Nonce", DPOP_NONCE)
                .body(body.to_string().into_bytes())
                .unwrap();
        }
        if method == "GET" && uri.starts_with(&format!("{PDS}/xrpc/app.bsky.actor.getProfile")) {
            let body = serde_json::json!({ "did": DID, "handle": HANDLE });
            return resp(200, body.to_string().into_bytes());
        }
        if method == "GET" && uri.starts_with(&format!("{PDS}/xrpc/app.bsky.feed.getTimeline")) {
            seen.lock().unwrap().get_timeline += 1;
            let [t1, t2] = timeline_uris();
            let rkey = |u: &str| u.rsplit('/').next().unwrap().to_string();
            // Newest-first, as the AppView serves it.
            let body = serde_json::json!({
                "feed": [
                    feed_item(BOB, "bob.test", &rkey(&t2), "second timeline post", 2),
                    feed_item(BOB, "bob.test", &rkey(&t1), "first timeline post", 1),
                ],
            });
            return resp(200, body.to_string().into_bytes());
        }
        if method == "GET" && uri.starts_with(&format!("{PDS}/xrpc/app.bsky.feed.getFeed")) {
            seen.lock().unwrap().get_feed += 1;
            if feed_outage.load(Ordering::SeqCst) {
                return resp(
                    502,
                    serde_json::json!({ "error": "UpstreamFailure", "message": "feed generator down" })
                        .to_string()
                        .into_bytes(),
                );
            }
            let body = serde_json::json!({
                "feed": [feed_item(CAROL, "carol.test", "f1", "a cat picture, probably", 3)],
            });
            return resp(200, body.to_string().into_bytes());
        }
        resp(
            404,
            format!("unexpected request: {method} {uri}").into_bytes(),
        )
    })
}

struct Harness {
    router: RpcRouter,
    state: Arc<AppState>,
    seen: Arc<Mutex<Seen>>,
    feed_outage: Arc<AtomicBool>,
}

async fn harness() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    init_db(&db).await.unwrap();

    let seen = Arc::new(Mutex::new(Seen::default()));
    let feed_outage = Arc::new(AtomicBool::new(false));
    let oauth = build_oauth_client_with_responder(
        BlueskyOAuthConfig {
            public_url: DOMAIN_URL.to_string(),
            keypair: Es256Keypair::generate().unwrap(),
            backend: Arc::new(CacheDbBackend::new(db.clone())),
        },
        fake_far_end(Arc::clone(&seen), Arc::clone(&feed_outage)),
    )
    .expect("build fake-far-end oauth client");

    let mut state = AppState::for_test(Arc::clone(&db));
    state
        .identity_domain
        .store(Some(Arc::new(DOMAIN.to_string())));
    state.bluesky.preset(DOMAIN_URL, Arc::new(oauth));
    let mut registry = BridgeProviderRegistry::new();
    registry.register(Box::new(BlueskyProvider));
    state.bridge.providers = Some(Arc::new(registry));
    let state = Arc::new(state);

    let mut b = RpcRouter::builder();
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    Harness {
        router: b.build(),
        state,
        seen,
        feed_outage,
    }
}

/// Link through the real provider, then complete the flow at the real
/// callback route with the `state` the authorization server was handed at
/// PAR — the redirect an authorization server sends back. Returns the
/// callback's `Location`.
async fn link_through_callback(h: &Harness) -> String {
    let req = LinkRequest {
        bridge_id: "bluesky".into(),
        mode: "oauth".into(),
        params: Value::Map(BTreeMap::from([(
            "handle".to_string(),
            Value::String(HANDLE.into()),
        )])),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &h.router,
        Arc::clone(&h.state),
        ACTOR,
        "fauna.bridges.link",
        payload,
    )
    .await
    .expect("link starts the oauth flow");
    let reply: LinkReply = decode(&reply_bytes).unwrap();
    assert!(
        reply.redirect_url.is_some(),
        "oauth link returns a redirect"
    );

    let par_state = h
        .seen
        .lock()
        .unwrap()
        .par_state
        .clone()
        .expect("authorize() pushed a state at PAR");
    let uri = format!(
        "/api/v1/bluesky/auth/callback?code=fake-code&state={}&iss={}",
        urlencoding::encode(&par_state),
        urlencoding::encode(ISSUER),
    );
    let resp = auth_routes::routes()
        .with_state(Arc::clone(&h.state))
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    resp.headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn subscribe_custom_feed(h: &Harness) {
    let req = CreateFeedRequest {
        bridge: "bluesky".into(),
        feed_uri: FEED_URI.into(),
        name: "Cats".into(),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    dispatch(
        &h.router,
        Arc::clone(&h.state),
        ACTOR,
        "fauna.bridges.feeds.create",
        payload,
    )
    .await
    .expect("subscribe a custom feed");
}

async fn count(state: &AppState, sql: &str) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// The callback, reached with exactly what the authorization server echoes,
/// links the account under the actor who started the flow and leaves a
/// session the poller can restore — the DPoP-nonce challenge answered on the
/// way.
#[tokio::test(flavor = "multi_thread")]
async fn the_callback_links_the_starting_actor_through_a_dpop_nonce_challenge() {
    let h = harness().await;
    let location = link_through_callback(&h).await;
    assert!(
        location.contains("result=linked"),
        "the callback completes the link: Location = {location:?}"
    );

    let (linked, actors) = {
        let conn = h.state.db.conn().await;
        let mut stmt = conn
            .prepare("SELECT actor_id FROM bluesky_accounts")
            .unwrap();
        let actors = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(stmt);
        let linked = db_helpers::get_linked_account(&conn, &hex::encode(ACTOR)).unwrap();
        (linked, actors)
    };
    let linked = linked.unwrap_or_else(|| {
        panic!(
            "the link must rest under the actor who started it; bluesky_accounts holds {actors:?}"
        )
    });
    assert_eq!(linked.bluesky_did, DID);
    assert_eq!(
        linked.bluesky_handle, HANDLE,
        "getProfile ran on the new session"
    );

    let nonces = h.seen.lock().unwrap().token_nonces.clone();
    assert_eq!(
        nonces,
        vec![false, true],
        "the token leg answered the nonce challenge with one retry"
    );
}

/// The feature, end to end over an earned session: the timeline's posts and
/// the subscribed custom feed's post rest as `bluesky`-source rows with their
/// map rows, and a second poll stores nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_poll_stores_timeline_and_custom_feed_posts_once() {
    let h = harness().await;
    link_through_callback(&h).await;
    subscribe_custom_feed(&h).await;

    let stored = feed_worker::poll_bluesky_feeds(&h.state, &hex::encode(ACTOR))
        .await
        .expect("the poll runs on the stored session");
    assert_eq!(stored, 3, "two timeline posts and one custom-feed post");

    {
        let conn = h.state.db.conn().await;
        for uri in timeline_uris().iter().chain([&feed_uri_post()]) {
            assert!(
                db_helpers::get_post_id_for_at_uri(&conn, uri)
                    .unwrap()
                    .is_some(),
                "{uri} rests with its map row"
            );
        }
    }
    assert_eq!(
        count(
            &h.state,
            "SELECT COUNT(*) FROM content WHERE source = 'bluesky'"
        )
        .await,
        3
    );

    let again = feed_worker::poll_bluesky_feeds(&h.state, &hex::encode(ACTOR))
        .await
        .unwrap();
    assert_eq!(again, 0, "a re-poll of the same window stores nothing");
    assert_eq!(
        count(&h.state, "SELECT COUNT(*) FROM bluesky_posts").await,
        3
    );

    let seen = h.seen.lock().unwrap();
    assert_eq!((seen.get_timeline, seen.get_feed), (2, 2));
}

/// One feed generator's outage is not the timeline's problem.
#[tokio::test(flavor = "multi_thread")]
async fn a_get_feed_outage_does_not_lose_the_timeline() {
    let h = harness().await;
    link_through_callback(&h).await;
    subscribe_custom_feed(&h).await;
    h.feed_outage.store(true, Ordering::SeqCst);

    let stored = feed_worker::poll_bluesky_feeds(&h.state, &hex::encode(ACTOR))
        .await
        .expect("a getFeed outage does not fail the poll");
    assert_eq!(stored, 2, "the timeline's two posts still land");
    {
        let conn = h.state.db.conn().await;
        assert!(
            db_helpers::get_post_id_for_at_uri(&conn, &feed_uri_post())
                .unwrap()
                .is_none()
        );
    }

    // The generator recovers: the next tick picks its post up.
    h.feed_outage.store(false, Ordering::SeqCst);
    let stored = feed_worker::poll_bluesky_feeds(&h.state, &hex::encode(ACTOR))
        .await
        .unwrap();
    assert_eq!(stored, 1);
}
