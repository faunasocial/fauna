//! Production-flow round-trip for `fauna.folders.set_web_paywall` — the
//! nest-state half of the paywalled-`web`-folder design
//! (`docs/goal/behavior/monetization.md` § Pillar 2, the folder bullet;
//! design ratified 2026-07-12).
//!
//! Covered:
//! - set → project: paywalling a website-enabled folder stamps `web_paywall_tier` and
//!   `fauna.folders.list` projects it on the owner summary;
//! - clear: `tier: None` un-paywalls unconditionally;
//! - the web-type gate: a folder is rejected (the webdav
//!   sync-type-gate twin, inverted);
//! - the tier-exists gate: an unknown tier is rejected (the entitlement seam
//!   Pillar 3's engine consults must resolve);
//! - missing set → `not_found`; the `User | Admin` allowlist arm.
//!
//! The tier row is seeded via `CacheDb::create_subscription_tier` — fixture
//! setup arranging a precondition (the tier-create *flow* is Pillar 1's,
//! proven in its own conformance suite), not the action under test.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    folder_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    folders::{
        FolderCreateReply, FolderCreateRequest, FolderSetWebPaywallReply,
        FolderSetWebPaywallRequest, FoldersListReply, FoldersListRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    (b.build(), state)
}

const ACTOR: [u8; 32] = [21u8; 32];

/// Create a folder; `website` flips its website toggle on afterwards — the
/// gate the paywall keys on (the former `mode = "web"` spelling is retired).
async fn create_set(router: &RpcRouter, state: &Arc<AppState>, name: &str, website: bool) {
    let _: FolderCreateReply = decode(
        &dispatch(
            router,
            Arc::clone(state),
            ACTOR,
            "fauna.folders.create",
            encode(&FolderCreateRequest {
                name: name.into(),
                retention_policy: None,
                ..Default::default()
            }),
        )
        .await
        .expect("create set ok"),
    )
    .unwrap();
    if website {
        dispatch(
            router,
            Arc::clone(state),
            ACTOR,
            "fauna.folders.update",
            encode(&fauna_protocol::folders::FolderUpdateRequest {
                name: name.into(),
                website_enabled: Some(true),
                ..Default::default()
            }),
        )
        .await
        .expect("website on");
    }
}

async fn set_paywall(
    router: &RpcRouter,
    state: &Arc<AppState>,
    name: &str,
    tier: Option<&str>,
) -> Result<Bytes, RpcError> {
    dispatch(
        router,
        Arc::clone(state),
        ACTOR,
        "fauna.folders.set_web_paywall",
        encode(&FolderSetWebPaywallRequest {
            name: name.into(),
            tier: tier.map(str::to_string),
            ..Default::default()
        }),
    )
    .await
}

async fn listed_tier(router: &RpcRouter, state: &Arc<AppState>, name: &str) -> Option<String> {
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
        .web_paywall_tier
}

#[tokio::test]
async fn paywall_set_clear_round_trips_and_projects() {
    let (router, state) = router_and_state().await;
    create_set(&router, &state, "site", true).await;
    state
        .db
        .create_subscription_tier(
            &ACTOR,
            "gold",
            1,
            None,
            Some("5 EUR"),
            None,
            false,
            None,
            None,
            false,
        )
        .await
        .unwrap();

    // Set: stamps the tier and the owner summary projects it.
    let reply: FolderSetWebPaywallReply = decode(
        &set_paywall(&router, &state, "site", Some("gold"))
            .await
            .expect("paywall ok"),
    )
    .unwrap();
    assert!(reply.ok);
    assert_eq!(
        listed_tier(&router, &state, "site").await.as_deref(),
        Some("gold")
    );

    // Clear: unconditional un-paywall.
    let _: FolderSetWebPaywallReply =
        decode(&set_paywall(&router, &state, "site", None).await.unwrap()).unwrap();
    assert_eq!(listed_tier(&router, &state, "site").await, None);
}

#[tokio::test]
async fn paywall_rejects_a_folder_whose_website_toggle_is_off() {
    let (router, state) = router_and_state().await;
    create_set(&router, &state, "docs", false).await;
    state
        .db
        .create_subscription_tier(
            &ACTOR, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
    let err = set_paywall(&router, &state, "docs", Some("gold"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
}

#[tokio::test]
async fn paywall_rejects_unknown_tier() {
    let (router, state) = router_and_state().await;
    create_set(&router, &state, "site", true).await;
    let err = set_paywall(&router, &state, "site", Some("no-such-tier"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");
}

#[tokio::test]
async fn paywall_missing_set_is_not_found() {
    let (router, state) = router_and_state().await;
    state
        .db
        .create_subscription_tier(
            &ACTOR, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
    let err = set_paywall(&router, &state, "ghost", Some("gold"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.not_found", "{err:?}");
    // Clear on a missing set is not_found too (the column write finds no row).
    let err = set_paywall(&router, &state, "ghost", None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.folders.not_found", "{err:?}");
}

#[test]
fn allowlist_admits_user_and_admin_only() {
    assert!(is_permitted(
        CallerClass::User,
        "fauna.folders.set_web_paywall"
    ));
    assert!(is_permitted(
        CallerClass::Admin,
        "fauna.folders.set_web_paywall"
    ));
    assert!(!is_permitted(
        CallerClass::BridgeMda,
        "fauna.folders.set_web_paywall"
    ));
    assert!(!is_permitted(
        CallerClass::ContentProcessor,
        "fauna.folders.set_web_paywall"
    ));
}
