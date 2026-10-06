//! Production-flow round-trips for the folders re-model **phase 4** audience
//! property + website toggle (`docs/goal/behavior/folders.md` § Target
//! re-model; the public exception is owned by `docs/goal/principles.md` § The
//! user always controls their data).
//!
//! Covered:
//! - the derived tri-state: a fresh set lists `"private"`, a group-bound one
//!   `"shared"`, a declassified one `"public"` — and a declassified *bound*
//!   set is `"public"` (the group survives as the write roster);
//! - born-public: `fauna.folders.create` with `audience: "public"`;
//!   `"shared"` at create is refused (the share flow binds);
//! - transitions on `fauna.folders.update`: →`public` from any audience,
//!   →`private` only unbound, →`shared` only bound; unknown values refused;
//! - the cross-toggle refusals, both flip orders: WebDAV-serve ⊕ public and
//!   paywall ⊕ public;
//! - the paywall gate re-key: a website-enabled Sync folder paywalls (the
//!   former web-type gate), a toggle-off one is refused;
//! - the website toggle round-trip + projection; reserved rails refused.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{db::CacheDb, folder_handlers, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    folders::{
        FolderCreateReply, FolderCreateRequest, FolderSetWebPaywallRequest, FolderSummary,
        FolderUpdateReply, FolderUpdateRequest, FoldersListReply, FoldersListRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    (b.build(), state)
}

const ACTOR: [u8; 32] = [22u8; 32];

async fn create_set(
    router: &RpcRouter,
    state: &Arc<AppState>,
    name: &str,
    audience: Option<&str>,
) -> Result<Bytes, RpcError> {
    dispatch(
        router,
        Arc::clone(state),
        ACTOR,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: name.into(),
            retention_policy: None,
            audience: audience.map(str::to_string),
            ..Default::default()
        }),
    )
    .await
}

async fn update(
    router: &RpcRouter,
    state: &Arc<AppState>,
    req: FolderUpdateRequest,
) -> Result<Bytes, RpcError> {
    dispatch(
        router,
        Arc::clone(state),
        ACTOR,
        "fauna.folders.update",
        encode(&req),
    )
    .await
}

fn audience_update(name: &str, audience: &str) -> FolderUpdateRequest {
    FolderUpdateRequest {
        name: name.into(),
        audience: Some(audience.into()),
        ..Default::default()
    }
}

async fn listed(router: &RpcRouter, state: &Arc<AppState>, name: &str) -> FolderSummary {
    let reply: FoldersListReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            ACTOR,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    reply
        .folders
        .into_iter()
        .find(|s| s.name == name)
        .expect("set listed")
}

#[tokio::test]
async fn audience_derives_private_shared_public_and_projects() {
    let (router, state) = router_and_state().await;

    // Fresh unbound set → private.
    let _: FolderCreateReply =
        decode(&create_set(&router, &state, "docs", None).await.unwrap()).unwrap();
    assert_eq!(listed(&router, &state, "docs").await.audience, "private");

    // Group-bound (the share flow's at-rest effect) → shared.
    state
        .db
        .set_folder_mls_group("docs", &ACTOR, Some(b"raw-group-id".as_slice()))
        .await
        .unwrap();
    assert_eq!(listed(&router, &state, "docs").await.audience, "shared");

    // Declassified bound set → public (the group survives as the write
    // roster; the audience the content rests for is the world).
    let _: FolderUpdateReply = decode(
        &update(&router, &state, audience_update("docs", "public"))
            .await
            .expect("declassify a bound set"),
    )
    .unwrap();
    assert_eq!(listed(&router, &state, "docs").await.audience, "public");

    // Flip back: bound → "shared" (never "private").
    let err = update(&router, &state, audience_update("docs", "private"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
    let _: FolderUpdateReply = decode(
        &update(&router, &state, audience_update("docs", "shared"))
            .await
            .expect("flip a bound set back to shared"),
    )
    .unwrap();
    assert_eq!(listed(&router, &state, "docs").await.audience, "shared");
}

#[tokio::test]
async fn born_public_create_and_the_shared_refusal() {
    let (router, state) = router_and_state().await;

    // Born public — plaintext from the first chunk, no re-seal pass owed.
    let _: FolderCreateReply = decode(
        &create_set(&router, &state, "site", Some("public"))
            .await
            .expect("born-public create"),
    )
    .unwrap();
    assert_eq!(listed(&router, &state, "site").await.audience, "public");

    // Flip back: unbound → "private" (never "shared").
    let err = update(&router, &state, audience_update("site", "shared"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
    let _: FolderUpdateReply = decode(
        &update(&router, &state, audience_update("site", "private"))
            .await
            .expect("flip an unbound set back to private"),
    )
    .unwrap();
    assert_eq!(listed(&router, &state, "site").await.audience, "private");

    // "shared" is entered by sharing, never spelled at create.
    let err = create_set(&router, &state, "born-shared", Some("shared"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");

    // Unknown audience values are refused at both doors.
    let err = create_set(&router, &state, "born-loud", Some("LOUD"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
    let err = update(&router, &state, audience_update("site", "LOUD"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
}

#[tokio::test]
async fn webdav_and_public_refuse_each_other_in_both_orders() {
    let (router, state) = router_and_state().await;
    let _: FolderCreateReply =
        decode(&create_set(&router, &state, "docs", None).await.unwrap()).unwrap();

    // Order 1: serve ON, then declassify → refused.
    let _: FolderUpdateReply = decode(
        &update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "docs".into(),
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("webdav on"),
    )
    .unwrap();
    let err = update(&router, &state, audience_update("docs", "public"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");

    // Serve OFF, declassify, then serve ON → refused the other way.
    let _: FolderUpdateReply = decode(
        &update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "docs".into(),
                webdav_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let _: FolderUpdateReply = decode(
        &update(&router, &state, audience_update("docs", "public"))
            .await
            .expect("declassify once un-served"),
    )
    .unwrap();
    let err = update(
        &router,
        &state,
        FolderUpdateRequest {
            name: "docs".into(),
            webdav_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");

    // One request carrying both halves is refused too, in either pairing.
    let _: FolderCreateReply =
        decode(&create_set(&router, &state, "both", None).await.unwrap()).unwrap();
    let err = update(
        &router,
        &state,
        FolderUpdateRequest {
            name: "both".into(),
            webdav_enabled: Some(true),
            audience: Some("public".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
}

#[tokio::test]
async fn paywall_and_public_refuse_each_other_and_the_gate_keys_on_the_toggle() {
    let (router, state) = router_and_state().await;
    state
        .db
        .create_subscription_tier(
            &ACTOR, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();

    // A Sync folder with the WEBSITE TOGGLE on paywalls — the re-keyed gate
    // (formerly `mode == "web"` only).
    let _: FolderCreateReply =
        decode(&create_set(&router, &state, "site", None).await.unwrap()).unwrap();
    let _: FolderUpdateReply = decode(
        &update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "site".into(),
                website_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("website on"),
    )
    .unwrap();
    assert!(listed(&router, &state, "site").await.website_enabled);
    let ok = dispatch(
        &router,
        Arc::clone(&state),
        ACTOR,
        "fauna.folders.set_web_paywall",
        encode(&FolderSetWebPaywallRequest {
            name: "site".into(),
            tier: Some("gold".into()),
            ..Default::default()
        }),
    )
    .await;
    assert!(ok.is_ok(), "{ok:?}");

    // Paywalled → declassify refused (a world-readable paywall is no paywall).
    let err = update(&router, &state, audience_update("site", "public"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");

    // Clear the paywall, declassify, then paywall again → refused the other way.
    let _ = dispatch(
        &router,
        Arc::clone(&state),
        ACTOR,
        "fauna.folders.set_web_paywall",
        encode(&FolderSetWebPaywallRequest {
            name: "site".into(),
            tier: None,
            ..Default::default()
        }),
    )
    .await
    .expect("unpaywall");
    let _: FolderUpdateReply = decode(
        &update(&router, &state, audience_update("site", "public"))
            .await
            .expect("declassify once un-paywalled"),
    )
    .unwrap();
    let err = dispatch(
        &router,
        Arc::clone(&state),
        ACTOR,
        "fauna.folders.set_web_paywall",
        encode(&FolderSetWebPaywallRequest {
            name: "site".into(),
            tier: Some("gold".into()),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");

    // A toggle-off folder still cannot paywall.
    let _: FolderCreateReply =
        decode(&create_set(&router, &state, "plain", None).await.unwrap()).unwrap();
    let err = dispatch(
        &router,
        Arc::clone(&state),
        ACTOR,
        "fauna.folders.set_web_paywall",
        encode(&FolderSetWebPaywallRequest {
            name: "plain".into(),
            tier: Some("gold".into()),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
}

#[tokio::test]
async fn website_toggle_round_trips_and_reserved_rails_refuse_everything() {
    let (router, state) = router_and_state().await;
    let _: FolderCreateReply =
        decode(&create_set(&router, &state, "site", None).await.unwrap()).unwrap();

    // Toggle on → projected; off → projected.
    let _: FolderUpdateReply = decode(
        &update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "site".into(),
                website_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(listed(&router, &state, "site").await.website_enabled);
    let _: FolderUpdateReply = decode(
        &update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "site".into(),
                website_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(!listed(&router, &state, "site").await.website_enabled);

    // Reserved rails: the create is refused outright (the namespace is the
    // nest's, `reserved-folders.md` § The management surface refuses the
    // namespace — whole), so the rail is minted nest-side; audience + website
    // are then both refused on it.
    let err = create_set(&router, &state, "__mail", None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
    state
        .db
        .get_or_create_reserved_folder(&ACTOR, "mail")
        .await
        .expect("a rail mints");
    let err = update(&router, &state, audience_update("__mail", "public"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
    let err = update(
        &router,
        &state,
        FolderUpdateRequest {
            name: "__mail".into(),
            website_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");

    // A reserved set cannot be BORN public either.
    let err = dispatch(
        &router,
        Arc::clone(&state),
        ACTOR,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: "__post".into(),
            retention_policy: None,
            audience: Some("public".into()),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
}
