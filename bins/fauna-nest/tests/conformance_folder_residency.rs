//! Production-flow round-trips for the folders re-model **phase 5** content
//! residency (`docs/goal/behavior/file-sync.md` § Content residency;
//! `docs/goal/behavior/folders.md` § Target re-model owns the concept).
//!
//! Covered:
//! - the round trip: a fresh set projects full (absent), the consent flip
//!   projects `"metadata_only"` on BOTH arms (a writer member's seat uploads
//!   bytes too, so members must see it), and the flip back projects full;
//! - unknown values refused; reserved rails refused;
//! - the pairwise serving refusals, BOTH directions and same-request orders:
//!   website ⊕ metadata-only, WebDAV ⊕ metadata-only, paywall ⊕ metadata-only.
//!
//! The at-rest byte consequences (the flip-time chunk drop, the GC's
//! pin-manifests-walk-no-chunks arm) are pinned beside the oracle in
//! `backup/gc.rs`; this file owns the wire/handler contract.
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
        FolderCreateRequest, FolderSetWebPaywallRequest, FolderSummary, FolderUpdateRequest,
        FoldersListReply, FoldersListRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    (b.build(), state)
}

const ACTOR: [u8; 32] = [23u8; 32];

async fn create_set(router: &RpcRouter, state: &Arc<AppState>, name: &str) {
    dispatch(
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
    .expect("create ok");
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

fn residency_update(name: &str, residency: &str) -> FolderUpdateRequest {
    FolderUpdateRequest {
        name: name.into(),
        residency: Some(residency.into()),
        ..Default::default()
    }
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

/// Asserts the refusal's typed `code` — the serving pairs each carry their own
/// (`RpcError::CODE_FOLDERS_*`), so the apps render the repair through
/// `localized()` — and that its log-only `details` names `needle`.
fn refused(result: Result<Bytes, RpcError>, code: &str, needle: &str) {
    let err = result.expect_err("must be refused");
    assert_eq!(err.code, code, "{err:?}");
    // The refusal text rides `details` as a CBOR string — the debug render is
    // enough to name which refusal fired.
    let detail = err
        .details
        .as_deref()
        .map(|v| format!("{v:?}"))
        .unwrap_or_default();
    assert!(
        detail.contains(needle),
        "expected a refusal naming {needle:?}, got: {detail}"
    );
}

// ── The round trip ─────────────────────────────────────────────────────────

#[tokio::test]
async fn residency_round_trips_and_projects_on_both_arms() {
    let (router, state) = router_and_state().await;
    create_set(&router, &state, "vault").await;

    // A fresh set is full — the field is ABSENT on the wire (fail-closed:
    // only an explicit, parsed opt-in may stop bytes resting).
    assert_eq!(listed(&router, &state, "vault").await.residency, "");

    update(&router, &state, residency_update("vault", "metadata_only"))
        .await
        .expect("the consent flip commits");
    assert_eq!(
        listed(&router, &state, "vault").await.residency,
        "metadata_only"
    );

    // The member arm carries it too — a writer member's seat uploads bytes
    // exactly as the owner's does, so its skip-the-bytes gate reads this
    // field off its own projection.
    let raw_group_id = b"raw-group-id".as_slice();
    state
        .db
        .set_folder_mls_group("vault", &ACTOR, Some(raw_group_id))
        .await
        .unwrap();
    let member = [24u8; 32];
    common::seed_dispatch_actor(&state.db, &member).await;
    let channel = fauna_mls::types::ChannelId::from_group_id(raw_group_id).0;
    state
        .db
        .register_actor_channel(&member, &channel)
        .await
        .unwrap();
    let reply: FoldersListReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            member,
            "fauna.folders.list",
            encode(&FoldersListRequest {
                include_shared_with_me: Some(true),
                extra: Default::default(),
            }),
        )
        .await
        .expect("member list ok"),
    )
    .unwrap();
    let member_row = reply
        .folders
        .iter()
        .find(|s| s.name == "vault" && s.role.as_deref() == Some("member"))
        .expect("member row projected");
    assert_eq!(
        member_row.residency, "metadata_only",
        "residency rides the member arm — the member's engine gates on it"
    );

    // The flip back to full clears the column; the wire field disappears.
    update(&router, &state, residency_update("vault", "full"))
        .await
        .expect("flip back commits");
    assert_eq!(listed(&router, &state, "vault").await.residency, "");
}

#[tokio::test]
async fn unknown_residency_values_are_refused() {
    let (router, state) = router_and_state().await;
    create_set(&router, &state, "vault").await;
    for bad in ["metadata", "META_ONLY", "none", ""] {
        refused(
            update(&router, &state, residency_update("vault", bad)).await,
            "fauna.folders.invalid_request",
            "unknown residency",
        );
    }
}

#[tokio::test]
async fn reserved_rails_refuse_residency() {
    let (router, state) = router_and_state().await;
    state.db.create_folder("__mail", &ACTOR).await.unwrap();
    refused(
        update(&router, &state, residency_update("__mail", "metadata_only")).await,
        "fauna.folders.invalid_request",
        "belong to the nest",
    );
}

// ── The pairwise serving refusals, both directions ─────────────────────────

#[tokio::test]
async fn a_website_serving_folder_refuses_metadata_only_and_vice_versa() {
    let (router, state) = router_and_state().await;
    create_set(&router, &state, "site").await;
    update(
        &router,
        &state,
        FolderUpdateRequest {
            name: "site".into(),
            website_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("website on");
    refused(
        update(&router, &state, residency_update("site", "metadata_only")).await,
        RpcError::CODE_FOLDERS_RESIDENCY_BLOCKED_BY_WEBSITE,
        "website",
    );

    // The other direction: serving moves second onto a metadata-only folder.
    create_set(&router, &state, "vault").await;
    update(&router, &state, residency_update("vault", "metadata_only"))
        .await
        .expect("flip");
    refused(
        update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "vault".into(),
                website_enabled: Some(true),
                ..Default::default()
            },
        )
        .await,
        RpcError::CODE_FOLDERS_METADATA_ONLY_CANNOT_SERVE,
        "metadata-only",
    );

    // Same-request orders cannot smuggle the pair through in either order.
    create_set(&router, &state, "both").await;
    refused(
        update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "both".into(),
                website_enabled: Some(true),
                residency: Some("metadata_only".into()),
                ..Default::default()
            },
        )
        .await,
        RpcError::CODE_FOLDERS_RESIDENCY_BLOCKED_BY_WEBSITE,
        "metadata-only",
    );
}

#[tokio::test]
async fn a_webdav_served_folder_refuses_metadata_only_and_vice_versa() {
    let (router, state) = router_and_state().await;
    create_set(&router, &state, "dav").await;
    update(
        &router,
        &state,
        FolderUpdateRequest {
            name: "dav".into(),
            webdav_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("webdav on");
    refused(
        update(&router, &state, residency_update("dav", "metadata_only")).await,
        RpcError::CODE_FOLDERS_RESIDENCY_BLOCKED_BY_WEBDAV,
        "WebDAV",
    );

    create_set(&router, &state, "vault").await;
    update(&router, &state, residency_update("vault", "metadata_only"))
        .await
        .expect("flip");
    refused(
        update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "vault".into(),
                webdav_enabled: Some(true),
                ..Default::default()
            },
        )
        .await,
        RpcError::CODE_FOLDERS_METADATA_ONLY_CANNOT_SERVE,
        "metadata-only",
    );
}

#[tokio::test]
async fn a_metadata_only_folder_refuses_a_paywall_and_a_paywalled_one_refuses_the_flip() {
    let (router, state) = router_and_state().await;

    // Paywall moves second onto a metadata-only folder: refused before the
    // website/tier checks so the message names the real repair.
    create_set(&router, &state, "vault").await;
    update(&router, &state, residency_update("vault", "metadata_only"))
        .await
        .expect("flip");
    refused(
        dispatch(
            &router,
            Arc::clone(&state),
            ACTOR,
            "fauna.folders.set_web_paywall",
            encode(&FolderSetWebPaywallRequest {
                name: "vault".into(),
                tier: Some("gold".into()),
                ..Default::default()
            }),
        )
        .await,
        RpcError::CODE_FOLDERS_METADATA_ONLY_CANNOT_SERVE,
        "metadata-only",
    );

    // The flip moves second onto a paywalled folder.
    create_set(&router, &state, "zine").await;
    update(
        &router,
        &state,
        FolderUpdateRequest {
            name: "zine".into(),
            website_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("website on");
    state
        .db
        .create_subscription_tier(
            &ACTOR, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
    dispatch(
        &router,
        Arc::clone(&state),
        ACTOR,
        "fauna.folders.set_web_paywall",
        encode(&FolderSetWebPaywallRequest {
            name: "zine".into(),
            tier: Some("gold".into()),
            ..Default::default()
        }),
    )
    .await
    .expect("paywall set");
    // A paywalled folder is necessarily website-serving, so the flip's
    // refusal names the FIRST serving surface it meets — the website.
    refused(
        update(&router, &state, residency_update("zine", "metadata_only")).await,
        RpcError::CODE_FOLDERS_RESIDENCY_BLOCKED_BY_WEBSITE,
        "website",
    );
}

#[tokio::test]
async fn a_public_folder_is_never_metadata_only_in_either_direction() {
    let (router, state) = router_and_state().await;

    // Public first, then the flip: refused, row untouched.
    create_set(&router, &state, "open").await;
    update(&router, &state, audience_update("open", "public"))
        .await
        .expect("declassify");
    refused(
        update(&router, &state, residency_update("open", "metadata_only")).await,
        RpcError::CODE_FOLDERS_RESIDENCY_BLOCKED_BY_PUBLIC,
        "public folder",
    );
    let row = listed(&router, &state, "open").await;
    assert_eq!(
        (row.audience.as_str(), row.residency.as_str()),
        ("public", "")
    );

    // A folder born public refuses the flip too (create takes no residency).
    dispatch(
        &router,
        Arc::clone(&state),
        ACTOR,
        "fauna.folders.create",
        encode(&FolderCreateRequest {
            name: "born".into(),
            audience: Some("public".into()),
            ..Default::default()
        }),
    )
    .await
    .expect("born public");
    refused(
        update(&router, &state, residency_update("born", "metadata_only")).await,
        RpcError::CODE_FOLDERS_RESIDENCY_BLOCKED_BY_PUBLIC,
        "public folder",
    );

    // The repair works: leave public, then the flip goes through.
    update(&router, &state, audience_update("open", "private"))
        .await
        .expect("back to private");
    update(&router, &state, residency_update("open", "metadata_only"))
        .await
        .expect("flip after repair");

    // Metadata-only first, then public: refused, row untouched.
    refused(
        update(&router, &state, audience_update("open", "public")).await,
        RpcError::CODE_FOLDERS_METADATA_ONLY_CANNOT_GO_PUBLIC,
        "metadata-only folder",
    );
    let row = listed(&router, &state, "open").await;
    assert_eq!(row.residency, "metadata_only");
    assert_ne!(row.audience, "public");

    // The repair works: back to full, then public goes through.
    update(&router, &state, residency_update("open", "full"))
        .await
        .expect("back to full");
    update(&router, &state, audience_update("open", "public"))
        .await
        .expect("public after repair");

    // Same-request orders cannot smuggle the pair through.
    create_set(&router, &state, "both").await;
    refused(
        update(
            &router,
            &state,
            FolderUpdateRequest {
                name: "both".into(),
                audience: Some("public".into()),
                residency: Some("metadata_only".into()),
                ..Default::default()
            },
        )
        .await,
        RpcError::CODE_FOLDERS_METADATA_ONLY_CANNOT_GO_PUBLIC,
        "metadata-only folder",
    );
    // Leaving public and going metadata-only in one request is allowed.
    update(
        &router,
        &state,
        FolderUpdateRequest {
            name: "open".into(),
            audience: Some("private".into()),
            residency: Some("metadata_only".into()),
            ..Default::default()
        },
    )
    .await
    .expect("leave public + flip in one request");
    assert_eq!(
        listed(&router, &state, "open").await.residency,
        "metadata_only"
    );
}

// ── A member on another nest is no bar to metadata-only (`file-sync.md`
// § Relay serving → *A member on another nest*): the interim refusal that held
// the pair while the cross-nest leg was unbuilt is gone, in both directions.
// The other direction (the member moves second) is pinned beside the Welcome
// relay in `conformance_cross_nest_conversations_client`; the leg itself is
// witnessed end to end by `fauna-sync-agent`'s `cross_nest_agent_capstone`. ──

/// Bind `name` to a group and seat one `channel_foreign_members` row on its
/// derived channel, exactly as the Welcome relay writes it. Returns the channel
/// and the foreign member.
async fn bind_with_foreign_member(state: &Arc<AppState>, name: &str) -> ([u8; 32], [u8; 32]) {
    let raw_group_id = format!("raw-group-id-{name}");
    state
        .db
        .set_folder_mls_group(name, &ACTOR, Some(raw_group_id.as_bytes()))
        .await
        .unwrap();
    let channel = fauna_mls::types::ChannelId::from_group_id(raw_group_id.as_bytes()).0;
    let foreign_member = [25u8; 32];
    state
        .db
        .register_foreign_channel_member(
            &channel,
            &foreign_member,
            &[26u8; 32],
            None,
            fauna_nest::db::channels::RebindPower::Standing,
        )
        .await
        .unwrap();
    (channel, foreign_member)
}

#[tokio::test]
async fn a_folder_with_a_member_on_another_nest_goes_metadata_only() {
    let (router, state) = router_and_state().await;
    create_set(&router, &state, "shared").await;
    let (channel, foreign_member) = bind_with_foreign_member(&state, "shared").await;

    update(&router, &state, residency_update("shared", "metadata_only"))
        .await
        .expect("a member on another nest is served by relay, so the flip commits");
    assert_eq!(
        listed(&router, &state, "shared").await.residency,
        "metadata_only"
    );
    update(&router, &state, residency_update("shared", "metadata_only"))
        .await
        .expect("re-sending the state the folder already holds commits");
    assert!(
        state
            .db
            .foreign_channel_member_exists(&channel, &foreign_member)
            .await
            .unwrap(),
        "the member on another nest stays on the roster"
    );
}
