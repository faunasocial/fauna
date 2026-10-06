//! Integration round-trip for the **positive** `fauna.bridges.link`
//! (`mode:"oauth"`) path of the Bluesky provider — the half the deterministic
//! `tests/api/test_bluesky_oauth.py` deferred because `oauth.authorize(handle)`
//! does a live atproto resolve (handle→DID→DID-doc→PDS metadata→auth-server
//! metadata→PAR). Replacement coverage owed when the `auth/start`/`auth/status`
//! HTTP twins were deleted (Commit C;
//! `api-layers.md` § Bluesky — "a tier_3 exercising `fauna.bridges.{link,list}`
//! … is still owed").
//!
//! The far end is faked **in-process at the `atrium_xrpc::HttpClient::send_http`
//! boundary** — no TLS, no DNS, no real server. The real `BlueskyProvider` and
//! a real `BlueskyOAuthClient` (built via `build_oauth_client_with_responder`)
//! run; only the network is canned, so this exercises Fauna's actual `link()`
//! glue (mode validation → param parse → `authorize()` → `redirect_url`
//! mapping) end-to-end through the live `RpcRouter`.
//!
//! Authority: `bridges.md` § Linking a Bluesky account (OAuth) — the link mode
//! advertises `client_action:"oauth_redirect"` and the reply carries
//! `redirect_url` (`bridge_provider.rs:55`, `:164-168`).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real in-memory `CacheDb` + real
//! atproto OAuth client; only the HTTP far end is canned). Feature-gated
//! `bluesky`.
#![cfg(feature = "bluesky")]

mod common;
use common::dispatch;

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;

use fauna_bridge_atproto::keypair::Es256Keypair;
use fauna_bridge_atproto::oauth::{
    BlueskyOAuthConfig, FakeHttpResponder, build_oauth_client_with_responder, http,
};
use fauna_nest::{
    bluesky::{
        bridge_provider::BlueskyProvider, db_helpers, init_db, storage_backend::CacheDbBackend,
    },
    bridge_management::{BridgeProviderRegistry, LinkReply},
    bridges_ui_handlers,
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{Value, bridges_ui::LinkRequest, decode_strict as decode, encode_canonical};

// The fake atproto identity the canned far end serves. `.test` is a reserved
// testing TLD and a syntactically valid atproto handle (atrium's own suite uses
// `xn--ls8h.test`); the DID is 24 base32 chars, matching the did:plc shape.
const HANDLE: &str = "alice.test";
const DID: &str = "did:plc:faketestbridgeuser234567";
const PDS: &str = "https://pds.alice.test";
const ISSUER: &str = "https://auth.alice.test";
const AUTH_ENDPOINT: &str = "https://auth.alice.test/oauth/authorize";
const PAR_ENDPOINT: &str = "https://auth.alice.test/oauth/par";
const REQUEST_URI: &str = "urn:ietf:params:oauth:request_uri:faketestbridge";

fn resp(status: u16, body: Vec<u8>) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

/// A canned atproto far end covering the five HTTP hops `OAuthClient::authorize`
/// makes, in order: handle→DID (well-known), DID→DID-document (PLC directory),
/// PDS protected-resource metadata, authorization-server metadata, and the
/// pushed-authorization-request POST (which atrium expects to return `201`).
fn fake_atproto_far_end() -> FakeHttpResponder {
    Arc::new(|req: &http::Request<Vec<u8>>| {
        let uri = req.uri().to_string();
        let method = req.method().as_str();

        // 1. handle → DID (GET https://<handle>/.well-known/atproto-did)
        if method == "GET" && uri.contains("alice.test/.well-known/atproto-did") {
            return resp(200, DID.as_bytes().to_vec());
        }
        // 2. DID → DID document (GET {plc_directory}/{did})
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
        // 3. PDS protected-resource metadata. `resource` must EXACTLY match the
        //    PDS URL and `authorization_servers` must hold exactly one entry.
        if method == "GET" && uri.contains("pds.alice.test/.well-known/oauth-protected-resource") {
            let meta = serde_json::json!({
                "resource": PDS,
                "authorization_servers": [ISSUER],
                "scopes_supported": [],
            });
            return resp(200, meta.to_string().into_bytes());
        }
        // 4. authorization-server metadata. `issuer` must EXACTLY match the
        //    authorization_servers entry; PAR endpoint must be present.
        if method == "GET" && uri.contains("auth.alice.test/.well-known/oauth-authorization-server")
        {
            let meta = serde_json::json!({
                "issuer": ISSUER,
                "authorization_endpoint": AUTH_ENDPOINT,
                "token_endpoint": format!("{ISSUER}/oauth/token"),
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
        // 5. pushed authorization request — atrium expects `201 Created`.
        if method == "POST" && uri.contains("/oauth/par") {
            let body = serde_json::json!({
                "request_uri": REQUEST_URI,
                "expires_in": 299,
            });
            return resp(201, body.to_string().into_bytes());
        }
        // Anything else is a harness gap — fail loudly so the test diagnoses it.
        resp(
            404,
            format!("unexpected request: {method} {uri}").into_bytes(),
        )
    })
}

/// The identity domain this fake nest is claimed onto, and the OAuth public URL
/// it derives — `https://<domain>`, exactly what `bluesky::oauth_public_url`
/// composes for a public DNS name.
const DOMAIN: &str = "nest.test";
const DOMAIN_URL: &str = "https://nest.test";

/// Build a live `RpcRouter` + `AppState` with the real `BlueskyProvider`
/// registered and a real OAuth client whose every HTTP hop is served by the
/// in-process canned far end.
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    // Creates bluesky_accounts (link's already-linked check) + bluesky_oauth_states
    // (the OAuth state store authorize() writes into).
    init_db(&db).await.unwrap();

    let keypair = Es256Keypair::generate().unwrap();
    let backend = Arc::new(CacheDbBackend::new(db.clone()));
    let config = BlueskyOAuthConfig {
        public_url: DOMAIN_URL.to_string(),
        keypair,
        backend,
    };
    let oauth = build_oauth_client_with_responder(config, fake_atproto_far_end())
        .expect("build fake-far-end oauth client");

    let state = AppState::for_test(Arc::clone(&db));
    // The nest derives its OAuth client from the CLAIMED identity domain, so a
    // fake client is presented the way a real one arrives: claim the box onto
    // `nest.test` and preset the cache under the URL that domain derives
    // (`bluesky::oauth_public_url`). The production rule still runs — this test
    // would stop finding a client the moment the derivation changed shape,
    // which is the point of not bypassing it.
    state
        .identity_domain
        .store(Some(Arc::new(DOMAIN.to_string())));
    state.bluesky.preset(DOMAIN_URL, Arc::new(oauth));
    let mut state = state;
    let mut registry = BridgeProviderRegistry::new();
    registry.register(Box::new(BlueskyProvider));
    state.bridge.providers = Some(Arc::new(registry));
    let state = Arc::new(state);

    let mut b = RpcRouter::builder();
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    (b.build(), state)
}

/// The positive path: `fauna.bridges.link {bridge_id:"bluesky", mode:"oauth",
/// params:{handle}}` resolves the handle through the (faked) atproto far end and
/// returns an authorization `redirect_url` that points at the resolved
/// authorization endpoint and carries the PAR `request_uri`.
#[tokio::test]
async fn link_oauth_mode_returns_authorize_redirect_url() {
    let (router, state) = router_and_state().await;

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
    let reply_bytes = dispatch(&router, state, [7u8; 32], "fauna.bridges.link", payload)
        .await
        .expect("link ok");
    let reply: LinkReply = decode(&reply_bytes).unwrap();

    // The OAuth flow only *starts* at link; linking completes at the callback.
    assert!(
        !reply.linked,
        "oauth link starts the flow, does not complete it"
    );
    assert!(reply.identity.is_none(), "no identity until callback");

    let url = reply
        .redirect_url
        .expect("oauth link returns a redirect_url (bridges.md § Linking a Bluesky account)");
    assert!(
        url.starts_with(AUTH_ENDPOINT),
        "redirect points at the resolved authorization endpoint, got {url}"
    );
    assert!(
        url.contains("request_uri="),
        "redirect carries the PAR request_uri, got {url}"
    );
    assert!(
        url.contains("faketestbridge"),
        "redirect carries our fake far end's request_uri, got {url}"
    );
}

/// Sanity countercheck that the assertion above isn't vacuous: an unknown link
/// mode is rejected before any atproto resolve happens (the far end is never
/// hit), surfacing the wire-stable `fauna.bridges.invalid_mode`.
#[tokio::test]
async fn link_unknown_mode_rejected_before_resolve() {
    let (router, state) = router_and_state().await;
    let req = LinkRequest {
        bridge_id: "bluesky".into(),
        mode: "sms".into(),
        params: Value::Map(BTreeMap::new()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(&router, state, [8u8; 32], "fauna.bridges.link", payload)
        .await
        .expect_err("unknown mode rejects");
    assert_eq!(err.code, "fauna.bridges.invalid_mode");
}

/// An actor that already has a linked Bluesky account can't re-link: `link`
/// surfaces `fauna.bridges.already_linked` *before* any atproto resolve (the
/// check precedes `authorize()`, so the far end is never hit).
#[tokio::test]
async fn link_when_already_linked_rejected_before_resolve() {
    let (router, state) = router_and_state().await;
    let actor = [9u8; 32];
    {
        let conn = state.db.conn().await;
        db_helpers::upsert_linked_account(&conn, &hex::encode(actor), DID, HANDLE)
            .expect("seed an existing linked account");
    }

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
    let err = dispatch(&router, state, actor, "fauna.bridges.link", payload)
        .await
        .expect_err("already-linked actor rejects re-link");
    assert_eq!(err.code, "fauna.bridges.already_linked");
}
