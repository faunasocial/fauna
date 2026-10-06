//! **`p2p-share.member.admit` refuses through the real Welcome door** —
//! `docs/goal/architecture/dynamic-features.md` § Evaluation points +
//! § Charter members ("new member admissions to shared sets — the fan-out
//! chokepoint"), composed per `docs/goal/behavior/p2p.md` § Wormability walk —
//! the share leg, rule 8 (feature-plane half).
//!
//! The surface is composed in `welcome_deliver_core`'s first-reach arm, on the
//! nest-fact discriminator (`folder_channel_claims`), because that is the ONE
//! production door through which a new member reaches a claimed set's roster —
//! same-nest and cross-nest both fork inside that function — and the only one
//! that can refuse (the bar: never compose a surface where it binds
//! nothing). `fauna.folders.share` (`share_core`) deliberately carries NO gate
//! call: it is per-set rather than per-member, skippable by a patched client
//! for members 2..N, and gating it as well would double-spend the unrefundable
//! counterparty quota.
//!
//! This file lives outside `conformance_feature_gate_surfaces.rs` because that
//! file is `#![cfg(feature = "payments")]` and this surface is not: the gate
//! call is unconditional (the admission door exists in every flavor — excising
//! the `p2p-share` cargo feature removes the P2P transfer plane, not
//! nest-mediated folder sharing), so the pin must run in every flavor too.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::feature_gate::{
    Availability, FeaturePolicy, GatedFeature, RuleTier, SURFACE_P2P_SHARE_MEMBER_ADMIT,
};
use fauna_mls::types::ChannelId;
use fauna_nest::{
    conversations_handlers, db::CacheDb, feature_gate::CODE_FEATURE_DENIED, folder_handlers,
    routes::AppState, rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError, Value,
    conversations::{WelcomeDeliverRequest, WelcomeKind},
    decode_strict as decode, encode_canonical,
    folders::{FolderShareReply, FolderShareRequest},
};

// ── harness (the conformance-harness pattern) ────────────────────

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    conversations_handlers::register_conversations_handlers(&mut b);
    (b.build(), state)
}

fn enc<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

/// Deny the whole member at the admin tier — the coarsest restriction, and the
/// one whose *absence* at a surface is invisible any other way: an unwired
/// surface simply succeeds.
async fn deny_p2p_share(state: &Arc<AppState>, actor: &[u8; 32]) {
    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            actor,
            GatedFeature::P2pShare,
            &FeaturePolicy {
                availability: Availability::Deny,
                ..FeaturePolicy::NO_OPINION
            },
        )
        .await
        .unwrap();
}

fn detail(error: &RpcError, key: &str) -> Value {
    let Some(boxed) = &error.details else {
        panic!("refusal carries no details: {}", error.code);
    };
    let Value::Map(map) = boxed.as_ref() else {
        panic!("refusal details are not a map");
    };
    map.get(key)
        .unwrap_or_else(|| panic!("refusal details carry no {key}"))
        .clone()
}

/// Every refusal in this file must be the typed Dim-4 shape naming **this**
/// surface — not a neighbouring one, and not a generic internal error.
#[track_caller]
fn refused_at(error: &RpcError, surface: &str) {
    assert_eq!(
        error.code, CODE_FEATURE_DENIED,
        "expected the feature-gate denial code, got {}",
        error.code
    );
    assert_eq!(
        detail(error, "surface"),
        Value::String(surface.into()),
        "the refusal must name the surface that was composed"
    );
    assert_eq!(
        detail(error, "tier"),
        Value::String("admin".into()),
        "boundary 4 — the person it binds must see WHICH tier bound them"
    );
}

/// Owner binds a set to a client-created MLS group via the REAL
/// `fauna.folders.share` — the first-binder claim that makes the channel a
/// shared set (`folder_channel_claims`). Returns the nest-derived ChannelId.
async fn bind_shared_set(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
    group_id: &[u8],
) -> [u8; 32] {
    state.db.create_folder("shared-docs", &owner).await.unwrap();
    let reply: FolderShareReply = decode(
        &dispatch(
            router,
            state.clone(),
            owner,
            "fauna.folders.share",
            enc(&FolderShareRequest {
                name: "shared-docs".into(),
                group_id: hex::encode(group_id),
                ..Default::default()
            }),
        )
        .await
        .expect("share ok"),
    )
    .unwrap();
    assert!(reply.ok);
    ChannelId::from_group_id(group_id).0
}

fn welcome(recipient: [u8; 32], channel_id: [u8; 32], kind: WelcomeKind) -> Bytes {
    enc(&WelcomeDeliverRequest {
        recipient_actor_id: hex::encode(recipient),
        channel_id: hex::encode(channel_id),
        welcome_bytes: vec![0x01, 0x02, 0x03],
        kind,
        nest_url: None,
        extra: Default::default(),
    })
}

// ── p2p-share.member.admit ───────────────────────────────────────

/// A first-time Welcome onto a channel the caller claims as a folder share is
/// the member-admit operation: a denied plane cannot admit a new member, the
/// refusal is typed and names this surface, and the roster stays untouched
/// (the refusal precedes every write).
#[tokio::test]
async fn member_admit_is_gated_at_the_folder_welcome() {
    let (router, state) = router_and_state().await;
    let owner = [0xa1u8; 32];
    let member = [0xb2u8; 32];
    let group_id = vec![0x5au8; 24];
    let channel_id = bind_shared_set(&router, &state, owner, &group_id).await;

    deny_p2p_share(&state, &owner).await;

    let err = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.conversations.welcome.deliver",
        welcome(
            member,
            channel_id,
            WelcomeKind::Folder {
                group_id: hex::encode(&group_id),
            },
        ),
    )
    .await
    .expect_err("a denied p2p-share plane must refuse a new member admission");
    refused_at(&err, SURFACE_P2P_SHARE_MEMBER_ADMIT);

    assert!(
        !state
            .db
            .is_actor_in_channel(&member, &channel_id)
            .await
            .unwrap(),
        "the refusal must precede the roster write"
    );
}

/// The discriminator is the claim row — nest state — never the sender's
/// `req.kind`: real folder-share Welcomes ride the Group kind on the wire
/// today, and a patched client must not route around the gate by relabeling
/// the Welcome (the same self-declaration defect as trusting a post's schema).
#[tokio::test]
async fn the_gate_reads_the_claim_not_the_senders_kind() {
    let (router, state) = router_and_state().await;
    let owner = [0xa1u8; 32];
    let member = [0xb2u8; 32];
    let group_id = vec![0x6bu8; 24];
    let channel_id = bind_shared_set(&router, &state, owner, &group_id).await;

    deny_p2p_share(&state, &owner).await;
    // Group-kind Welcomes consult the recipient's inbox mode — open it so the
    // only thing standing between the sender and the roster is the gate.
    state.db.set_inbox_mode(&member, "open").await.unwrap();

    let err = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.conversations.welcome.deliver",
        welcome(
            member,
            channel_id,
            WelcomeKind::Group {
                group_id: hex::encode(&group_id),
            },
        ),
    )
    .await
    .expect_err("a Group-labeled Welcome on a claimed folder channel is still a member admission");
    refused_at(&err, SURFACE_P2P_SHARE_MEMBER_ADMIT);
}

/// The conversation plane never enters the gate: a DM Welcome on an unclaimed
/// channel flows under a full `p2p-share` deny — group-chat membership must
/// not be charged against (or refused by) the share plane's policy.
#[tokio::test]
async fn a_dm_welcome_is_untouched_by_a_p2p_share_deny() {
    let (router, state) = router_and_state().await;
    let sender = [0xa1u8; 32];
    let recipient = [0xb2u8; 32];
    // A DM channel: never claimed by any folder.
    let channel_id = ChannelId::from_group_id(&[0x7cu8; 24]).0;

    deny_p2p_share(&state, &sender).await;
    state.db.set_inbox_mode(&recipient, "open").await.unwrap();

    dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        welcome(recipient, channel_id, WelcomeKind::Dm),
    )
    .await
    .expect("a DM Welcome is not a share admission");
    assert!(
        state
            .db
            .is_actor_in_channel(&recipient, &channel_id)
            .await
            .unwrap(),
        "the conversation plane's delivery must be untouched"
    );
}

/// A re-Welcome of an established member is in-band (an idempotent retry or a
/// re-add), not a new admission: it flows ungated and spends nothing — the
/// same initiation-vs-in-band split the reach floor draws, resolved against
/// the feature's own records per § The quota grammar's newness-delta rule.
#[tokio::test]
async fn a_re_welcome_of_an_established_member_flows_ungated() {
    let (router, state) = router_and_state().await;
    let owner = [0xa1u8; 32];
    let member = [0xb2u8; 32];
    let group_id = vec![0x8du8; 24];
    let channel_id = bind_shared_set(&router, &state, owner, &group_id).await;

    // First admission under the generous tier-1 constants: allowed, spent.
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.conversations.welcome.deliver",
        welcome(
            member,
            channel_id,
            WelcomeKind::Folder {
                group_id: hex::encode(&group_id),
            },
        ),
    )
    .await
    .expect("first admission passes under tier-1 constants");

    // Deny the plane afterwards: the established member's re-Welcome is
    // in-band and must keep flowing.
    deny_p2p_share(&state, &owner).await;
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.conversations.welcome.deliver",
        welcome(
            member,
            channel_id,
            WelcomeKind::Folder {
                group_id: hex::encode(&group_id),
            },
        ),
    )
    .await
    .expect("a re-Welcome of an established member is not a new admission");
}
