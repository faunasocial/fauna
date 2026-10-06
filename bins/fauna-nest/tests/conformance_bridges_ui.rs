//! Integration round-trip for `fauna.bridges.list` (Layer-3 Bridge
//! Management user-facing surface). Mirrors `conformance_bridge_blobs`
//! — a fake `BridgeProvider` is wired into `state.bridge.providers`,
//! the kind is dispatched through the live `RpcRouter`, and the reply
//! is decoded into `fauna_protocol::bridges_ui::ListBridgesReply` and
//! shape-checked.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/bridges_ui.rs`.
//! Authority for the migration plan tracked internally (§ T1b).

mod common;
use common::dispatch;

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use fauna_nest::{
    bridge_management::{
        BridgeError, BridgeFollow, BridgeIdentity, BridgeProvider, BridgeProviderRegistry,
        BridgeSetting, BridgeStatus, LinkReply,
    },
    bridges_ui_handlers,
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    Value,
    bridges_ui::{
        AddFollowReply, AddFollowRequest, CreateFeedReply, CreateFeedRequest, DeleteFeedReply,
        DeleteFeedRequest, LinkRequest, ListBridgesReply, ListBridgesRequest, ListFeedsReply,
        ListFeedsRequest, ListFollowsReply, ListFollowsRequest, RemoveFollowReply,
        RemoveFollowRequest, SetSettingsReply, SetSettingsRequest, UnlinkReply, UnlinkRequest,
    },
    decode_strict as decode, encode_canonical,
};
use std::collections::BTreeMap;
use std::sync::{Arc as StdArc, Mutex};

/// External sink — the FakeBridge keeps a clone, the test keeps a
/// clone. After dispatching `fauna.bridges.set_settings`, the test
/// reads back the last-seen settings via its handle.
type SettingsSink = StdArc<Mutex<Option<Value>>>;

/// Captures the (mode, params) the handler passed to `BridgeProvider::link`.
type LinkSink = StdArc<Mutex<Option<(String, Value)>>>;

/// Increment-on-call counter for `unlink` invocations (so the test can
/// assert the handler actually reached the provider).
type UnlinkCounter = StdArc<Mutex<u32>>;

/// Captures the (id, petname, extra) the handler passed to
/// `BridgeProvider::add_follow`.
type AddFollowSink = StdArc<Mutex<Option<(String, Option<String>, Option<Value>)>>>;

/// Captures every follow_id the handler passed to
/// `BridgeProvider::remove_follow`, in call order.
type RemoveFollowSink = StdArc<Mutex<Vec<String>>>;

/// Configurable result `BridgeProvider::link` returns. Cloned out of
/// the slot at call time; tests seed happy- and error-path values.
type LinkResultSlot = StdArc<Mutex<Result<LinkReply, BridgeError>>>;

/// Build a `Value::Map` from a string-keyed `(key, Value)` list — the
/// convenient shape for composing test `params` / `settings` / `extra`
/// payloads.
fn cbor_map(entries: &[(&str, Value)]) -> Value {
    Value::Map(
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect(),
    )
}

/// Look up a key in a `Value::Map`; returns `None` if `value` isn't a
/// map or the key isn't present.
fn cbor_map_get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    if let Value::Map(entries) = value {
        entries.get(key)
    } else {
        None
    }
}

struct FakeBridge {
    id: &'static str,
    name: &'static str,
    available: bool,
    status: Option<BridgeStatus>,
    /// Shared with the test harness; populated by `update_settings`.
    last_settings: SettingsSink,
    /// Static follow list the provider returns from `list_follows`.
    follows: Vec<BridgeFollow>,
    /// If `true`, `list_follows` returns a `BridgeError::provider_error`
    /// instead of `follows`.
    list_follows_errors: bool,
    /// Shared with the test harness; populated by `link`.
    last_link: LinkSink,
    /// Configured result `link` returns. Defaults to a "linked=true,
    /// identity=None, redirect_url=None" happy path; tests override
    /// for redirect / error scenarios.
    link_result: LinkResultSlot,
    /// Shared with the test harness; incremented on each `unlink` call.
    unlink_calls: UnlinkCounter,
    /// If `true`, `unlink` returns a `BridgeError::provider_error`
    /// instead of `Ok(())`.
    unlink_errors: bool,
    /// Shared with the test harness; populated by `add_follow`.
    last_add_follow: AddFollowSink,
    /// If `true`, `add_follow` returns
    /// `BridgeError::already_linked("duplicate follow")` instead of
    /// `Ok(())` — exercises the duplicate-follow surfacing path.
    add_follow_duplicate: bool,
    /// Shared with the test harness; captures every `remove_follow`
    /// call's `follow_id`.
    removed_follows: RemoveFollowSink,
    /// If `true`, `remove_follow` returns a
    /// `BridgeError::provider_error` instead of `Ok(())`.
    remove_follow_errors: bool,
}

impl FakeBridge {
    fn new(id: &'static str, name: &'static str) -> Self {
        Self {
            id,
            name,
            available: true,
            status: None,
            last_settings: StdArc::new(Mutex::new(None)),
            follows: Vec::new(),
            list_follows_errors: false,
            last_link: StdArc::new(Mutex::new(None)),
            link_result: StdArc::new(Mutex::new(Ok(LinkReply {
                extra: Default::default(),
                linked: true,
                identity: None,
                redirect_url: None,
            }))),
            unlink_calls: StdArc::new(Mutex::new(0)),
            unlink_errors: false,
            last_add_follow: StdArc::new(Mutex::new(None)),
            add_follow_duplicate: false,
            removed_follows: StdArc::new(Mutex::new(Vec::new())),
            remove_follow_errors: false,
        }
    }

    fn add_follow_sink(&self) -> AddFollowSink {
        self.last_add_follow.clone()
    }

    fn remove_follow_sink(&self) -> RemoveFollowSink {
        self.removed_follows.clone()
    }

    fn settings_sink(&self) -> SettingsSink {
        self.last_settings.clone()
    }

    fn link_sink(&self) -> LinkSink {
        self.last_link.clone()
    }

    fn link_result_slot(&self) -> LinkResultSlot {
        self.link_result.clone()
    }

    fn unlink_counter(&self) -> UnlinkCounter {
        self.unlink_calls.clone()
    }
}

#[async_trait]
impl BridgeProvider for FakeBridge {
    fn id(&self) -> &str {
        self.id
    }
    fn name(&self) -> &str {
        self.name
    }
    async fn available(&self, _state: &AppState) -> bool {
        self.available
    }
    fn link_modes(&self) -> Vec<fauna_nest::bridge_management::BridgeLinkMode> {
        Vec::new()
    }
    fn supports_follows(&self) -> bool {
        true
    }
    async fn status(
        &self,
        _state: &AppState,
        _actor_id: &str,
    ) -> Result<BridgeStatus, BridgeError> {
        match &self.status {
            Some(s) => Ok(BridgeStatus {
                linked: s.linked,
                identity: s.identity.clone(),
                mode: s.mode.clone(),
                settings: s.settings.clone(),
                link_modes: s.link_modes.clone(),
            }),
            None => Err(BridgeError::provider_error("simulated provider failure")),
        }
    }
    async fn link(
        &self,
        _state: &AppState,
        _actor_id: &str,
        mode: &str,
        params: Value,
    ) -> Result<LinkReply, BridgeError> {
        *self.last_link.lock().unwrap() = Some((mode.to_string(), params));
        self.link_result.lock().unwrap().clone()
    }
    async fn unlink(&self, _state: &AppState, _actor_id: &str) -> Result<(), BridgeError> {
        *self.unlink_calls.lock().unwrap() += 1;
        if self.unlink_errors {
            return Err(BridgeError::provider_error("simulated unlink failure"));
        }
        Ok(())
    }
    async fn update_settings(
        &self,
        _state: &AppState,
        _actor_id: &str,
        settings: Value,
    ) -> Result<(), BridgeError> {
        *self.last_settings.lock().unwrap() = Some(settings);
        Ok(())
    }
    async fn list_follows(
        &self,
        _state: &AppState,
        _actor_id: &str,
    ) -> Result<Vec<BridgeFollow>, BridgeError> {
        if self.list_follows_errors {
            return Err(BridgeError::provider_error("simulated upstream failure"));
        }
        Ok(self
            .follows
            .iter()
            .map(|f| BridgeFollow {
                id: f.id.clone(),
                petname: f.petname.clone(),
                created_at: f.created_at,
                extra: f.extra.clone(),
                unknown_keys: Default::default(),
            })
            .collect())
    }
    async fn add_follow(
        &self,
        _state: &AppState,
        _actor_id: &str,
        id: &str,
        petname: Option<&str>,
        extra: Option<Value>,
    ) -> Result<(), BridgeError> {
        *self.last_add_follow.lock().unwrap() =
            Some((id.to_string(), petname.map(str::to_string), extra));
        if self.add_follow_duplicate {
            return Err(BridgeError::already_linked());
        }
        Ok(())
    }
    async fn remove_follow(
        &self,
        _state: &AppState,
        _actor_id: &str,
        follow_id: &str,
    ) -> Result<(), BridgeError> {
        self.removed_follows
            .lock()
            .unwrap()
            .push(follow_id.to_string());
        if self.remove_follow_errors {
            return Err(BridgeError::provider_error("simulated remove failure"));
        }
        Ok(())
    }
}

async fn router_with_providers(
    providers: Vec<Box<dyn BridgeProvider>>,
) -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut state = AppState::for_test(db);
    let mut registry = BridgeProviderRegistry::new();
    for p in providers {
        registry.register(p);
    }
    state.bridge.providers = Some(Arc::new(registry));
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    (b.build(), state)
}

#[tokio::test]
async fn list_bridges_round_trips_provider_snapshot() {
    let providers: Vec<Box<dyn BridgeProvider>> = vec![
        Box::new(FakeBridge {
            status: Some(BridgeStatus {
                linked: true,
                identity: Some(BridgeIdentity {
                    extra: Default::default(),
                    label: "Handle".into(),
                    value: "did:plc:abc".into(),
                    display: "alice.bsky.social".into(),
                }),
                mode: Some("personal".into()),
                settings: vec![BridgeSetting {
                    extra: Default::default(),
                    key: "write_through".into(),
                    label: "Crosspost to Bluesky".into(),
                    setting_type: "bool".into(),
                    value: Value::Bool(true),
                    options: None,
                }],
                link_modes: None,
            }),
            ..FakeBridge::new("bluesky", "Bluesky")
        }),
        Box::new(FakeBridge {
            available: false,
            ..FakeBridge::new("activitypub", "ActivityPub")
        }),
    ];
    let (router, state) = router_with_providers(providers).await;

    let user_actor = [42u8; 32];
    let req = ListBridgesRequest {};
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, user_actor, "fauna.bridges.list", payload)
        .await
        .expect("list ok");
    let reply: ListBridgesReply = decode(&reply_bytes).unwrap();

    assert_eq!(reply.bridges.len(), 2);

    let bsky = &reply.bridges[0];
    assert_eq!(bsky.id, "bluesky");
    assert_eq!(bsky.name, "Bluesky");
    assert!(bsky.available);
    assert!(bsky.linked);
    assert_eq!(
        bsky.identity.as_ref().map(|i| i.display.as_str()),
        Some("alice.bsky.social"),
    );
    assert_eq!(bsky.mode.as_deref(), Some("personal"));
    assert!(bsky.supports_follows);
    // 1 provider-declared setting + the 2 uniform search-corpus rows the
    // handler appends for every content bridge (bluesky/nostr/activitypub) —
    // `append_search_policy_settings`, defaults for a never-configured actor.
    assert_eq!(bsky.settings.len(), 3);
    assert_eq!(bsky.settings[0].key, "write_through");
    assert_eq!(bsky.settings[0].value, Value::Bool(true));
    assert_eq!(bsky.settings[1].key, "show_in_search");
    assert_eq!(bsky.settings[1].value, Value::Bool(true));
    assert_eq!(bsky.settings[2].key, "limit_posts_in_search");
    assert_eq!(bsky.settings[2].value, Value::Integer(1000));
    assert!(bsky.error.is_none());

    let ap = &reply.bridges[1];
    assert_eq!(ap.id, "activitypub");
    assert!(!ap.available);
    assert!(!ap.linked);
    assert!(ap.identity.is_none());
    assert!(ap.settings.is_empty());
    assert!(ap.error.is_none(), "unavailable bridges report no error");
}

#[tokio::test]
async fn list_bridges_reports_provider_status_error() {
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(FakeBridge {
        // `status: None` forces provider.status(...) to return Err
        ..FakeBridge::new("nostr", "Nostr")
    })];
    let (router, state) = router_with_providers(providers).await;
    let req = ListBridgesRequest {};
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, [9u8; 32], "fauna.bridges.list", payload)
        .await
        .expect("list ok even when provider errors");
    let reply: ListBridgesReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.bridges.len(), 1);
    let b = &reply.bridges[0];
    assert!(
        b.available,
        "available stays true when provider.status fails"
    );
    assert!(!b.linked, "linked falls back to false on error");
    assert!(b.settings.is_empty());
    assert_eq!(
        b.error.as_deref(),
        Some("simulated provider failure"),
        "provider error surfaces as `error` field",
    );
}

// ── fauna.bridges.set_settings ───────────────────────────────────

#[tokio::test]
async fn set_settings_overwrites_provider_settings() {
    let fake = FakeBridge::new("bluesky", "Bluesky");
    let sink = fake.settings_sink();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;

    let req = SetSettingsRequest {
        bridge_id: "bluesky".into(),
        settings: Value::Map(BTreeMap::from([
            ("write_through".to_string(), Value::Bool(true)),
            ("poll_interval_s".to_string(), Value::Integer(60)),
        ])),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state,
        [11u8; 32],
        "fauna.bridges.set_settings",
        payload,
    )
    .await
    .expect("set_settings ok");
    let reply: SetSettingsReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);

    // The CBOR settings payload reaches `update_settings` typed —
    // no JSON shimmer at the boundary since T9+T10.
    let captured = sink.lock().unwrap().clone().expect("provider saw settings");
    assert_eq!(
        cbor_map_get(&captured, "write_through"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        cbor_map_get(&captured, "poll_interval_s"),
        Some(&Value::Integer(60)),
    );
}

#[tokio::test]
async fn set_settings_unknown_bridge_id_returns_not_found() {
    let providers: Vec<Box<dyn BridgeProvider>> =
        vec![Box::new(FakeBridge::new("bluesky", "Bluesky"))];
    let (router, state) = router_with_providers(providers).await;
    let req = SetSettingsRequest {
        bridge_id: "made-up".into(),
        settings: Value::Map(BTreeMap::new()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [12u8; 32],
        "fauna.bridges.set_settings",
        payload,
    )
    .await
    .expect_err("unknown bridge id rejects");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

#[tokio::test]
async fn set_settings_no_registry_returns_not_found() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    let router = b.build();
    let req = SetSettingsRequest {
        bridge_id: "anything".into(),
        settings: Value::Map(BTreeMap::new()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [13u8; 32],
        "fauna.bridges.set_settings",
        payload,
    )
    .await
    .expect_err("missing registry rejects");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

// ── fauna.bridges.list_follows ───────────────────────────────────

#[tokio::test]
async fn list_follows_returns_provider_follows() {
    let fake = FakeBridge {
        follows: vec![
            BridgeFollow {
                id: "did:plc:abc".into(),
                petname: Some("Alice".into()),
                created_at: Some(1_700_000_000),
                extra: Some(cbor_map(&[(
                    "handle",
                    Value::String("alice.bsky.social".into()),
                )])),
                unknown_keys: Default::default(),
            },
            BridgeFollow {
                id: "did:plc:def".into(),
                petname: None,
                created_at: None,
                extra: None,
                unknown_keys: Default::default(),
            },
        ],
        ..FakeBridge::new("bluesky", "Bluesky")
    };
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;

    let req = ListFollowsRequest {
        extra: Default::default(),
        bridge_id: "bluesky".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state,
        [14u8; 32],
        "fauna.bridges.list_follows",
        payload,
    )
    .await
    .expect("list_follows ok");
    let reply: ListFollowsReply = decode(&reply_bytes).unwrap();

    assert_eq!(reply.follows.len(), 2);
    assert_eq!(reply.follows[0].id, "did:plc:abc");
    assert_eq!(reply.follows[0].petname.as_deref(), Some("Alice"));
    assert_eq!(reply.follows[0].created_at, Some(1_700_000_000));
    // The HTTP twin omits the field when absent; the typed wire keeps
    // the slot present (`None`/`Some(Value::…)`).
    let extra = reply.follows[0].extra.as_ref().expect("extra carried");
    match extra {
        Value::Map(entries) => {
            assert!(entries.iter().any(|(k, v)| matches!(
                (k.as_str(), v),
                ("handle", Value::String(h)) if h == "alice.bsky.social"
            )));
        }
        other => panic!("expected extra to be a CBOR map, got {other:?}"),
    }
    assert_eq!(reply.follows[1].id, "did:plc:def");
    assert!(reply.follows[1].petname.is_none());
    assert!(reply.follows[1].created_at.is_none());
    assert!(
        reply.follows[1].extra.is_none(),
        "missing extra is None on wire"
    );
}

#[tokio::test]
async fn list_follows_unknown_bridge_id_returns_not_found() {
    let providers: Vec<Box<dyn BridgeProvider>> =
        vec![Box::new(FakeBridge::new("bluesky", "Bluesky"))];
    let (router, state) = router_with_providers(providers).await;
    let req = ListFollowsRequest {
        extra: Default::default(),
        bridge_id: "made-up".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [15u8; 32],
        "fauna.bridges.list_follows",
        payload,
    )
    .await
    .expect_err("unknown bridge id rejects");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

#[tokio::test]
async fn list_follows_provider_error_surfaces_as_provider_error_rpc() {
    let fake = FakeBridge {
        list_follows_errors: true,
        ..FakeBridge::new("nostr", "Nostr")
    };
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = ListFollowsRequest {
        extra: Default::default(),
        bridge_id: "nostr".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [16u8; 32],
        "fauna.bridges.list_follows",
        payload,
    )
    .await
    .expect_err("provider error rejects");
    assert_eq!(err.code, "fauna.bridges.provider_error");
}

// ── fauna.bridges.link ───────────────────────────────────────────

#[tokio::test]
async fn link_passes_mode_and_params_to_provider_returns_identity() {
    let fake = FakeBridge::new("bluesky", "Bluesky");
    let sink = fake.link_sink();
    let result = fake.link_result_slot();
    // Configure the provider to return a linked identity (the OAuth
    // callback case — already-completed flow returns the linked
    // identity directly).
    *result.lock().unwrap() = Ok(LinkReply {
        extra: Default::default(),
        linked: true,
        identity: Some(BridgeIdentity {
            extra: Default::default(),
            label: "Handle".into(),
            value: "did:plc:abc".into(),
            display: "alice.bsky.social".into(),
        }),
        redirect_url: None,
    });
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;

    let req = LinkRequest {
        bridge_id: "bluesky".into(),
        mode: "oauth".into(),
        params: Value::Map(BTreeMap::from([(
            "handle".to_string(),
            Value::String("alice.bsky.social".into()),
        )])),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, [20u8; 32], "fauna.bridges.link", payload)
        .await
        .expect("link ok");
    let reply: LinkReply = decode(&reply_bytes).unwrap();

    assert!(reply.linked);
    let id = reply.identity.expect("identity carried");
    assert_eq!(id.value, "did:plc:abc");
    assert_eq!(id.display, "alice.bsky.social");
    assert!(reply.redirect_url.is_none());

    let captured = sink
        .lock()
        .unwrap()
        .clone()
        .expect("provider saw link call");
    assert_eq!(captured.0, "oauth");
    assert_eq!(
        cbor_map_get(&captured.1, "handle"),
        Some(&Value::String("alice.bsky.social".into())),
    );
}

#[tokio::test]
async fn link_propagates_provider_redirect_url() {
    let fake = FakeBridge::new("bluesky", "Bluesky");
    let result = fake.link_result_slot();
    *result.lock().unwrap() = Ok(LinkReply {
        extra: Default::default(),
        linked: false,
        identity: None,
        redirect_url: Some("https://bsky.social/oauth/authorize?…".into()),
    });
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;

    let req = LinkRequest {
        bridge_id: "bluesky".into(),
        mode: "oauth".into(),
        params: Value::Map(BTreeMap::new()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, [21u8; 32], "fauna.bridges.link", payload)
        .await
        .expect("link ok");
    let reply: LinkReply = decode(&reply_bytes).unwrap();
    assert!(!reply.linked);
    assert!(reply.identity.is_none());
    assert_eq!(
        reply.redirect_url.as_deref(),
        Some("https://bsky.social/oauth/authorize?…")
    );
}

#[tokio::test]
async fn link_invalid_mode_surfaces_as_invalid_mode_rpc() {
    let fake = FakeBridge::new("bluesky", "Bluesky");
    let result = fake.link_result_slot();
    *result.lock().unwrap() = Err(BridgeError::invalid_mode("credentials"));
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = LinkRequest {
        bridge_id: "bluesky".into(),
        mode: "credentials".into(),
        params: Value::Map(BTreeMap::new()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(&router, state, [22u8; 32], "fauna.bridges.link", payload)
        .await
        .expect_err("invalid mode rejects");
    assert_eq!(err.code, "fauna.bridges.invalid_mode");
}

#[tokio::test]
async fn link_unknown_bridge_id_returns_not_found() {
    let providers: Vec<Box<dyn BridgeProvider>> =
        vec![Box::new(FakeBridge::new("bluesky", "Bluesky"))];
    let (router, state) = router_with_providers(providers).await;
    let req = LinkRequest {
        bridge_id: "made-up".into(),
        mode: "oauth".into(),
        params: Value::Map(BTreeMap::new()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(&router, state, [23u8; 32], "fauna.bridges.link", payload)
        .await
        .expect_err("unknown bridge id rejects");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

// ── fauna.bridges.unlink ─────────────────────────────────────────

#[tokio::test]
async fn unlink_reaches_provider_and_returns_ok() {
    let fake = FakeBridge::new("bluesky", "Bluesky");
    let counter = fake.unlink_counter();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = UnlinkRequest {
        extra: Default::default(),
        bridge_id: "bluesky".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, [24u8; 32], "fauna.bridges.unlink", payload)
        .await
        .expect("unlink ok");
    let reply: UnlinkReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);
    assert_eq!(*counter.lock().unwrap(), 1);
}

#[tokio::test]
async fn unlink_unknown_bridge_id_returns_not_found() {
    let providers: Vec<Box<dyn BridgeProvider>> =
        vec![Box::new(FakeBridge::new("bluesky", "Bluesky"))];
    let (router, state) = router_with_providers(providers).await;
    let req = UnlinkRequest {
        extra: Default::default(),
        bridge_id: "made-up".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(&router, state, [25u8; 32], "fauna.bridges.unlink", payload)
        .await
        .expect_err("unknown bridge id rejects");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

#[tokio::test]
async fn unlink_provider_error_surfaces_as_provider_error_rpc() {
    let fake = FakeBridge {
        unlink_errors: true,
        ..FakeBridge::new("nostr", "Nostr")
    };
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = UnlinkRequest {
        extra: Default::default(),
        bridge_id: "nostr".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(&router, state, [26u8; 32], "fauna.bridges.unlink", payload)
        .await
        .expect_err("provider error rejects");
    assert_eq!(err.code, "fauna.bridges.provider_error");
}

// ── fauna.bridges.add_follow ─────────────────────────────────────

#[tokio::test]
async fn add_follow_passes_payload_to_provider() {
    let fake = FakeBridge::new("bluesky", "Bluesky");
    let sink = fake.add_follow_sink();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;

    let req = AddFollowRequest {
        bridge_id: "bluesky".into(),
        id: "did:plc:abc".into(),
        petname: Some("Alice".into()),
        extra: Some(Value::Map(BTreeMap::from([(
            "handle".to_string(),
            Value::String("alice.bsky.social".into()),
        )]))),
        unknown_keys: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state,
        [30u8; 32],
        "fauna.bridges.add_follow",
        payload,
    )
    .await
    .expect("add_follow ok");
    let reply: AddFollowReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);

    let captured = sink
        .lock()
        .unwrap()
        .clone()
        .expect("provider saw add_follow");
    assert_eq!(captured.0, "did:plc:abc");
    assert_eq!(captured.1.as_deref(), Some("Alice"));
    let extra = captured.2.expect("extra carried to provider");
    assert_eq!(
        cbor_map_get(&extra, "handle"),
        Some(&Value::String("alice.bsky.social".into())),
    );
}

#[tokio::test]
async fn add_follow_without_optional_fields_omits_extra_at_boundary() {
    let fake = FakeBridge::new("nostr", "Nostr");
    let sink = fake.add_follow_sink();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;

    let req = AddFollowRequest {
        bridge_id: "nostr".into(),
        id: "npub1xyz".into(),
        petname: None,
        extra: None,
        unknown_keys: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let _reply_bytes = dispatch(
        &router,
        state,
        [31u8; 32],
        "fauna.bridges.add_follow",
        payload,
    )
    .await
    .expect("add_follow ok with no optional fields");

    let captured = sink
        .lock()
        .unwrap()
        .clone()
        .expect("provider saw add_follow");
    assert_eq!(captured.0, "npub1xyz");
    assert!(
        captured.1.is_none(),
        "missing petname surfaces as None at provider boundary"
    );
    // None must NOT synthesize a JSON `null` — the HTTP twin omits the
    // field entirely, and the handler must match that shape.
    assert!(
        captured.2.is_none(),
        "missing extra must stay None at the provider boundary, not synthesize JSON null"
    );
}

#[tokio::test]
async fn add_follow_unknown_bridge_id_returns_not_found() {
    let providers: Vec<Box<dyn BridgeProvider>> =
        vec![Box::new(FakeBridge::new("bluesky", "Bluesky"))];
    let (router, state) = router_with_providers(providers).await;
    let req = AddFollowRequest {
        bridge_id: "made-up".into(),
        id: "did:plc:abc".into(),
        petname: None,
        extra: None,
        unknown_keys: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [32u8; 32],
        "fauna.bridges.add_follow",
        payload,
    )
    .await
    .expect_err("unknown bridge id rejects");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

#[tokio::test]
async fn add_follow_duplicate_surfaces_as_already_linked_rpc() {
    let fake = FakeBridge {
        add_follow_duplicate: true,
        ..FakeBridge::new("bluesky", "Bluesky")
    };
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = AddFollowRequest {
        bridge_id: "bluesky".into(),
        id: "did:plc:abc".into(),
        petname: None,
        extra: None,
        unknown_keys: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [33u8; 32],
        "fauna.bridges.add_follow",
        payload,
    )
    .await
    .expect_err("duplicate follow rejects");
    assert_eq!(err.code, "fauna.bridges.already_linked");
}

// ── fauna.bridges.remove_follow ──────────────────────────────────

#[tokio::test]
async fn remove_follow_reaches_provider_and_returns_ok() {
    let fake = FakeBridge::new("bluesky", "Bluesky");
    let sink = fake.remove_follow_sink();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = RemoveFollowRequest {
        extra: Default::default(),
        bridge_id: "bluesky".into(),
        follow_id: "did:plc:abc".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state,
        [34u8; 32],
        "fauna.bridges.remove_follow",
        payload,
    )
    .await
    .expect("remove_follow ok");
    let reply: RemoveFollowReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);

    let captured = sink.lock().unwrap().clone();
    assert_eq!(captured, vec!["did:plc:abc".to_string()]);
}

#[tokio::test]
async fn remove_follow_unknown_bridge_id_returns_not_found() {
    let providers: Vec<Box<dyn BridgeProvider>> =
        vec![Box::new(FakeBridge::new("bluesky", "Bluesky"))];
    let (router, state) = router_with_providers(providers).await;
    let req = RemoveFollowRequest {
        extra: Default::default(),
        bridge_id: "made-up".into(),
        follow_id: "did:plc:abc".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [35u8; 32],
        "fauna.bridges.remove_follow",
        payload,
    )
    .await
    .expect_err("unknown bridge id rejects");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

#[tokio::test]
async fn remove_follow_provider_error_surfaces_as_provider_error_rpc() {
    let fake = FakeBridge {
        remove_follow_errors: true,
        ..FakeBridge::new("nostr", "Nostr")
    };
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = RemoveFollowRequest {
        extra: Default::default(),
        bridge_id: "nostr".into(),
        follow_id: "npub1xyz".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [36u8; 32],
        "fauna.bridges.remove_follow",
        payload,
    )
    .await
    .expect_err("provider error rejects");
    assert_eq!(err.code, "fauna.bridges.provider_error");
}

// ── availability gate (defense-in-depth) ───────────────────────
//
// `available:false` only hides a bridge from `list` + the UI; the
// mutating handlers also gate create/modify ops on availability so a
// bridge that can't run on this nest (e.g. Nostr on an encrypted nest)
// can't be driven out-of-band by a direct call. Cleanup ops
// (unlink/remove_follow) stay ungated so a user can always remove a
// now-unavailable bridge's state (user-controls-data invariant).

#[tokio::test]
async fn set_settings_on_unavailable_bridge_rejected_without_reaching_provider() {
    let fake = FakeBridge {
        available: false,
        ..FakeBridge::new("nostr", "Nostr")
    };
    let sink = fake.settings_sink();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = SetSettingsRequest {
        bridge_id: "nostr".into(),
        settings: Value::Map(BTreeMap::from([(
            "expose_content".to_string(),
            Value::Bool(true),
        )])),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [31u8; 32],
        "fauna.bridges.set_settings",
        payload,
    )
    .await
    .expect_err("unavailable bridge rejects set_settings");
    assert_eq!(err.code, "fauna.bridges.not_found");
    assert!(
        sink.lock().unwrap().is_none(),
        "update_settings must not reach the provider when unavailable"
    );
}

#[tokio::test]
async fn link_on_unavailable_bridge_rejected_without_reaching_provider() {
    let fake = FakeBridge {
        available: false,
        ..FakeBridge::new("nostr", "Nostr")
    };
    let sink = fake.link_sink();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = LinkRequest {
        bridge_id: "nostr".into(),
        mode: "generate".into(),
        params: Value::Map(BTreeMap::new()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(&router, state, [32u8; 32], "fauna.bridges.link", payload)
        .await
        .expect_err("unavailable bridge rejects link");
    assert_eq!(err.code, "fauna.bridges.not_found");
    assert!(
        sink.lock().unwrap().is_none(),
        "link must not reach the provider when unavailable"
    );
}

#[tokio::test]
async fn add_follow_on_unavailable_bridge_rejected_without_reaching_provider() {
    let fake = FakeBridge {
        available: false,
        ..FakeBridge::new("nostr", "Nostr")
    };
    let sink = fake.add_follow_sink();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = AddFollowRequest {
        bridge_id: "nostr".into(),
        id: "npub1xyz".into(),
        petname: None,
        extra: None,
        unknown_keys: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [33u8; 32],
        "fauna.bridges.add_follow",
        payload,
    )
    .await
    .expect_err("unavailable bridge rejects add_follow");
    assert_eq!(err.code, "fauna.bridges.not_found");
    assert!(
        sink.lock().unwrap().is_none(),
        "add_follow must not reach the provider when unavailable"
    );
}

#[tokio::test]
async fn unlink_on_unavailable_bridge_still_reaches_provider() {
    // Cleanup ops are NOT gated: a user can always remove a now-unavailable
    // bridge's state (user-controls-data invariant).
    let fake = FakeBridge {
        available: false,
        ..FakeBridge::new("nostr", "Nostr")
    };
    let counter = fake.unlink_counter();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = UnlinkRequest {
        extra: Default::default(),
        bridge_id: "nostr".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, [34u8; 32], "fauna.bridges.unlink", payload)
        .await
        .expect("unlink ok even when unavailable");
    let reply: UnlinkReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);
    assert_eq!(
        *counter.lock().unwrap(),
        1,
        "unlink must reach the provider even when unavailable"
    );
}

#[tokio::test]
async fn remove_follow_on_unavailable_bridge_still_reaches_provider() {
    // Cleanup op — ungated, same invariant as unlink.
    let fake = FakeBridge {
        available: false,
        ..FakeBridge::new("nostr", "Nostr")
    };
    let sink = fake.remove_follow_sink();
    let providers: Vec<Box<dyn BridgeProvider>> = vec![Box::new(fake)];
    let (router, state) = router_with_providers(providers).await;
    let req = RemoveFollowRequest {
        extra: Default::default(),
        bridge_id: "nostr".into(),
        follow_id: "npub1xyz".into(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state,
        [35u8; 32],
        "fauna.bridges.remove_follow",
        payload,
    )
    .await
    .expect("remove_follow ok even when unavailable");
    let reply: RemoveFollowReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);
    assert_eq!(
        sink.lock().unwrap().as_slice(),
        ["npub1xyz".to_string()],
        "remove_follow must reach the provider even when unavailable"
    );
}

#[tokio::test]
async fn list_bridges_empty_when_no_provider_registry() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    let router = b.build();
    let req = ListBridgesRequest {};
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, [1u8; 32], "fauna.bridges.list", payload)
        .await
        .expect("list ok with no providers");
    let reply: ListBridgesReply = decode(&reply_bytes).unwrap();
    assert!(reply.bridges.is_empty());
}

// ── fauna.bridges.feeds.* (T5) ──────────────────────────────────
//
// Unlike the BridgeProvider-trait kinds above, the feeds.* surface
// goes straight to `CacheDb` — no provider registry needed.

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    (b.build(), state)
}

#[tokio::test]
async fn feeds_list_empty_when_no_subscriptions() {
    let (router, state) = router_with_db_only().await;
    let req = ListFeedsRequest {};
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state,
        [7u8; 32],
        "fauna.bridges.feeds.list",
        payload,
    )
    .await
    .expect("list ok");
    let reply: ListFeedsReply = decode(&reply_bytes).unwrap();
    assert!(reply.subscriptions.is_empty());
}

#[tokio::test]
async fn feeds_create_then_list_round_trips() {
    let (router, state) = router_with_db_only().await;
    let actor = [11u8; 32];

    let create = CreateFeedRequest {
        extra: Default::default(),
        bridge: "bluesky".into(),
        feed_uri: "at://did:plc:abc/app.bsky.feed.generator/whats-hot".into(),
        name: "What's Hot".into(),
    };
    let payload = Bytes::from(encode_canonical(&create).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.bridges.feeds.create",
        payload,
    )
    .await
    .expect("create ok");
    let CreateFeedReply { id, .. } = decode(&reply_bytes).unwrap();
    assert!(id > 0, "server assigns a positive row id");

    let payload = Bytes::from(encode_canonical(&ListFeedsRequest {}).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, actor, "fauna.bridges.feeds.list", payload)
        .await
        .expect("list ok");
    let reply: ListFeedsReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.subscriptions.len(), 1);
    let s = &reply.subscriptions[0];
    assert_eq!(s.id, id);
    assert_eq!(s.bridge, "bluesky");
    assert_eq!(
        s.feed_uri,
        "at://did:plc:abc/app.bsky.feed.generator/whats-hot",
    );
    assert_eq!(s.name, "What's Hot");
    assert!(s.created_at > 0, "created_at populated from db");
}

#[tokio::test]
async fn feeds_create_is_idempotent_returning_same_id() {
    // The replay-safe rationale in register_bridges_ui_kinds: INSERT OR
    // IGNORE against UNIQUE(actor_id, bridge, feed_uri) means a
    // duplicate create returns the same id, not a constraint error.
    let (router, state) = router_with_db_only().await;
    let actor = [12u8; 32];

    let create = CreateFeedRequest {
        extra: Default::default(),
        bridge: "bluesky".into(),
        feed_uri: "at://did:plc:abc/app.bsky.feed.generator/whats-hot".into(),
        name: "What's Hot".into(),
    };
    let payload = Bytes::from(encode_canonical(&create).unwrap().to_vec());
    let first_id = {
        let reply_bytes = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.bridges.feeds.create",
            payload.clone(),
        )
        .await
        .expect("first create ok");
        let r: CreateFeedReply = decode(&reply_bytes).unwrap();
        r.id
    };
    let second_id = {
        let reply_bytes = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.bridges.feeds.create",
            payload,
        )
        .await
        .expect("second create ok");
        let r: CreateFeedReply = decode(&reply_bytes).unwrap();
        r.id
    };
    assert_eq!(
        first_id, second_id,
        "duplicate create must return the same id (replay-safe)"
    );
}

#[tokio::test]
async fn feeds_create_rejects_empty_fields() {
    let (router, state) = router_with_db_only().await;
    for req in [
        CreateFeedRequest {
            extra: Default::default(),
            bridge: "".into(),
            feed_uri: "at://x".into(),
            name: "x".into(),
        },
        CreateFeedRequest {
            extra: Default::default(),
            bridge: "bluesky".into(),
            feed_uri: "".into(),
            name: "x".into(),
        },
        CreateFeedRequest {
            extra: Default::default(),
            bridge: "bluesky".into(),
            feed_uri: "at://x".into(),
            name: "".into(),
        },
    ] {
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = dispatch(
            &router,
            state.clone(),
            [13u8; 32],
            "fauna.bridges.feeds.create",
            payload,
        )
        .await
        .expect_err("empty field rejected");
        assert_eq!(err.code, "fauna.bridges.invalid_params");
    }
}

#[tokio::test]
async fn feeds_create_rejects_oversize_fields() {
    let (router, state) = router_with_db_only().await;
    let big_bridge = "x".repeat(65);
    let big_uri = "u".repeat(2049);
    let big_name = "n".repeat(129);
    for req in [
        CreateFeedRequest {
            extra: Default::default(),
            bridge: big_bridge,
            feed_uri: "at://x".into(),
            name: "x".into(),
        },
        CreateFeedRequest {
            extra: Default::default(),
            bridge: "bluesky".into(),
            feed_uri: big_uri,
            name: "x".into(),
        },
        CreateFeedRequest {
            extra: Default::default(),
            bridge: "bluesky".into(),
            feed_uri: "at://x".into(),
            name: big_name,
        },
    ] {
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let err = dispatch(
            &router,
            state.clone(),
            [14u8; 32],
            "fauna.bridges.feeds.create",
            payload,
        )
        .await
        .expect_err("oversize field rejected");
        assert_eq!(err.code, "fauna.bridges.invalid_params");
    }
}

#[tokio::test]
async fn feeds_delete_removes_row() {
    let (router, state) = router_with_db_only().await;
    let actor = [15u8; 32];

    let create = CreateFeedRequest {
        extra: Default::default(),
        bridge: "bluesky".into(),
        feed_uri: "at://did:plc:abc/app.bsky.feed.generator/whats-hot".into(),
        name: "What's Hot".into(),
    };
    let payload = Bytes::from(encode_canonical(&create).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.bridges.feeds.create",
        payload,
    )
    .await
    .unwrap();
    let CreateFeedReply { id, .. } = decode(&reply_bytes).unwrap();

    let payload = Bytes::from(
        encode_canonical(&DeleteFeedRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.bridges.feeds.delete",
        payload,
    )
    .await
    .expect("delete ok");
    let reply: DeleteFeedReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);

    let payload = Bytes::from(encode_canonical(&ListFeedsRequest {}).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, actor, "fauna.bridges.feeds.list", payload)
        .await
        .unwrap();
    let reply: ListFeedsReply = decode(&reply_bytes).unwrap();
    assert!(reply.subscriptions.is_empty(), "row gone after delete");
}

#[tokio::test]
async fn feeds_delete_unknown_id_returns_not_found() {
    let (router, state) = router_with_db_only().await;
    let payload = Bytes::from(
        encode_canonical(&DeleteFeedRequest {
            extra: Default::default(),
            id: 9999,
        })
        .unwrap()
        .to_vec(),
    );
    let err = dispatch(
        &router,
        state,
        [16u8; 32],
        "fauna.bridges.feeds.delete",
        payload,
    )
    .await
    .expect_err("unknown id rejected");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

#[tokio::test]
async fn feeds_delete_other_actors_row_returns_not_found() {
    // Per-actor isolation: actor A can't delete actor B's subscription.
    // The DB DELETE filters by both id and actor_id, so the wrong-actor
    // path takes the same not_found shape as the unknown-id path.
    let (router, state) = router_with_db_only().await;
    let owner = [17u8; 32];
    let intruder = [18u8; 32];

    let create = CreateFeedRequest {
        extra: Default::default(),
        bridge: "bluesky".into(),
        feed_uri: "at://did:plc:abc/app.bsky.feed.generator/whats-hot".into(),
        name: "What's Hot".into(),
    };
    let payload = Bytes::from(encode_canonical(&create).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.bridges.feeds.create",
        payload,
    )
    .await
    .unwrap();
    let CreateFeedReply { id, .. } = decode(&reply_bytes).unwrap();

    let payload = Bytes::from(
        encode_canonical(&DeleteFeedRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let err = dispatch(
        &router,
        state.clone(),
        intruder,
        "fauna.bridges.feeds.delete",
        payload,
    )
    .await
    .expect_err("intruder rejected");
    assert_eq!(err.code, "fauna.bridges.not_found");

    // And the owner's row is still there.
    let payload = Bytes::from(encode_canonical(&ListFeedsRequest {}).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, owner, "fauna.bridges.feeds.list", payload)
        .await
        .unwrap();
    let reply: ListFeedsReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.subscriptions.len(), 1, "owner's row survived");
}

#[tokio::test]
async fn feeds_list_isolates_per_actor() {
    let (router, state) = router_with_db_only().await;
    let actor_a = [19u8; 32];
    let actor_b = [20u8; 32];

    for actor in [actor_a, actor_b] {
        let create = CreateFeedRequest {
            extra: Default::default(),
            bridge: "bluesky".into(),
            feed_uri: format!("at://did:plc:{}/feed", actor[0]),
            name: format!("Feed {}", actor[0]),
        };
        let payload = Bytes::from(encode_canonical(&create).unwrap().to_vec());
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.bridges.feeds.create",
            payload,
        )
        .await
        .unwrap();
    }

    let payload = Bytes::from(encode_canonical(&ListFeedsRequest {}).unwrap().to_vec());
    let reply_bytes = dispatch(&router, state, actor_a, "fauna.bridges.feeds.list", payload)
        .await
        .unwrap();
    let reply: ListFeedsReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.subscriptions.len(), 1, "only own row visible");
    assert_eq!(reply.subscriptions[0].feed_uri, "at://did:plc:19/feed");
}
