//! **Every wired gate surface refuses through its own real handler** —
//! `docs/goal/architecture/dynamic-features.md` § Evaluation points, the
//! per-surface composition of the decision function W2 (account-data-plane.md § Workstreams) slice 2 shipped.
//!
//! `conformance_feature_gate.rs` proves the floor's *behaviour* on one surface
//! (`payments.claim.redeem`): the typed refusal, the tier attribution, the
//! anti-cycling property. This file proves the other half — that each surface
//! the registry declares is **actually composed at the call site it names**, and
//! that the refusal carries that surface's own identifier rather than a
//! neighbour's.
//!
//! **Why one test per surface rather than a loop over the registry.** The defect
//! this file exists to catch is a *missing call*, and a missing call has no
//! symbol to enumerate: a registry-driven loop can only iterate the identifiers,
//! which exist whether or not anything ever calls them (that is exactly the
//! state this slice found — 11 declared identifiers, 1 call site). Only driving
//! the real handler distinguishes "wired" from "declared". Each test below
//! therefore calls a shipped kind, unchanged on the wire, and never reads
//! `fauna.features.status` first — a client that never asks is the client the
//! floor is for.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

#![cfg(feature = "payments")]

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::feature_gate::{Availability, FeaturePolicy, GatedFeature, RuleTier};
use fauna_core::identity::ActorKeypair;
use fauna_nest::{
    db::CacheDb, feature_gate::CODE_FEATURE_DENIED, folder_handlers, payment_handlers,
    routes::AppState, rpc_router::RpcRouter, subscription_handlers,
};
use fauna_protocol::{
    RpcError, Value, encode_canonical,
    folders::{FolderCreateRequest, FolderSetWebPaywallRequest, FolderUpdateRequest},
    payments::{ClaimMintRequest, ProviderSetRequest},
    subscriptions::{TierAskingPrice, TierCreateRequest, TierUpdateRequest},
};

mod common;

// ── harness ──────────────────────────────────────────────────────

fn build_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    payment_handlers::register_payment_handlers(&mut b);
    folder_handlers::register_folders_handlers(&mut b);
    #[cfg(all(feature = "zaps", feature = "nostr"))]
    fauna_nest::nostr::zap_signer_handlers::register_nostr_zap_signer_handlers(&mut b);
    b.build()
}

/// The one author every test here acts as — fixed, so the tier helpers can
/// sign the birth `KeyBlob` `tiers.create` requires without threading it.
fn author_kp() -> ActorKeypair {
    ActorKeypair::from_secret([0xA7; 32])
}

async fn harness() -> (RpcRouter, Arc<AppState>, [u8; 32]) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    #[cfg(all(feature = "zaps", feature = "nostr"))]
    fauna_nest::nostr::init_db(&db).await.unwrap();
    let state = Arc::new(AppState::for_test(db));
    let author = author_kp().actor_id().0;
    state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();
    (build_router(), state, author)
}

async fn encode_call(
    router: &RpcRouter,
    state: Arc<AppState>,
    kind: &str,
    bearer: [u8; 32],
    payload: impl serde::Serialize,
) -> Result<Bytes, RpcError> {
    let bytes = encode_canonical(&payload).unwrap();
    common::call_raw(router, state, kind, bearer, Bytes::from(bytes.to_vec())).await
}

/// Deny the whole member at the admin tier — the coarsest restriction, and the
/// one whose *absence* at a surface is invisible any other way: an unwired
/// surface simply succeeds.
async fn deny(state: &Arc<AppState>, actor: &[u8; 32], feature: GatedFeature) {
    state
        .db
        .put_feature_policy(
            RuleTier::Admin,
            actor,
            feature,
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

async fn create_free_tier(router: &RpcRouter, state: &Arc<AppState>, author: [u8; 32], name: &str) {
    encode_call(
        router,
        state.clone(),
        "fauna.subscriptions.tiers.create",
        author,
        TierCreateRequest {
            name: name.into(),
            rank: 1,
            description: None,
            price_hint: None,
            payment_url: None,
            auto_approve: false,
            encrypted_upload: common::encrypted_keyblob::birth_upload(&author_kp(), name),
            unlocks_post: None,
            asking_price: None,
            hidden: false,
            extra: Default::default(),
        },
    )
    .await
    .expect("a free tier is not a payments operation");
}

fn priced_create(name: &str) -> TierCreateRequest {
    TierCreateRequest {
        name: name.into(),
        rank: 1,
        description: None,
        price_hint: None,
        payment_url: None,
        auto_approve: false,
        encrypted_upload: common::encrypted_keyblob::birth_upload(&author_kp(), name),
        unlocks_post: None,
        asking_price: Some(TierAskingPrice::msats(21_000)),
        hidden: false,
        extra: Default::default(),
    }
}

// ── payments.provider.configure ──────────────────────────────────

/// The sell side's first door: a denied plane cannot have a payment rail
/// configured onto it.
#[tokio::test]
async fn provider_configure_is_gated() {
    let (router, state, author) = harness().await;
    create_free_tier(&router, &state, author, "gold").await;
    deny(&state, &author, GatedFeature::Payments).await;

    let error = encode_call(
        &router,
        state.clone(),
        "fauna.payments.providers.set",
        author,
        ProviderSetRequest {
            kind: "fake".into(),
            webhook_secret: "s3cret".into(),
            tier: "gold".into(),
            extra: Default::default(),
        },
    )
    .await
    .expect_err("a denied payments plane refuses provider configuration");
    refused_at(&error, "payments.provider.configure");

    assert!(
        state
            .db
            .list_payment_providers(&author)
            .await
            .unwrap()
            .is_empty(),
        "the refusal is the floor, not a warning: no config row was written"
    );
}

// ── payments.claim.mint ──────────────────────────────────────────

/// The one payments operation that reaches nothing external — and therefore the
/// one a looping client could run without limit if it were ungated.
#[tokio::test]
async fn claim_mint_is_gated() {
    let (router, state, author) = harness().await;
    create_free_tier(&router, &state, author, "gold").await;
    deny(&state, &author, GatedFeature::Payments).await;

    let error = encode_call(
        &router,
        state.clone(),
        "fauna.payments.claims.mint",
        author,
        ClaimMintRequest {
            tier: "gold".into(),
            valid_until: None,
            extra: Default::default(),
        },
    )
    .await
    .expect_err("a denied payments plane mints no claims");
    refused_at(&error, "payments.claim.mint");

    assert!(
        state
            .db
            .list_payment_claims(&author)
            .await
            .unwrap()
            .is_empty(),
        "no bearer code escaped the gate"
    );
}

// ── payments.tier.price ──────────────────────────────────────────

/// Pricing a tier is a payments operation; creating an ordinary tier is not.
/// The control matters as much as the pin: a gate that fired on every
/// `tiers.create` would take the whole subscription plane down with the
/// payments member, and subscriptions are not a registry member.
#[tokio::test]
async fn tier_pricing_is_gated_but_a_free_tier_is_not() {
    let (router, state, author) = harness().await;
    deny(&state, &author, GatedFeature::Payments).await;

    // The control: an unpriced tier still lands under a full payments deny.
    create_free_tier(&router, &state, author, "followers").await;
    assert!(
        state
            .db
            .get_subscription_tier(&author, "followers")
            .await
            .unwrap()
            .is_some()
    );

    let error = encode_call(
        &router,
        state.clone(),
        "fauna.subscriptions.tiers.create",
        author,
        priced_create("sold"),
    )
    .await
    .expect_err("a priced tier is a payments operation");
    refused_at(&error, "payments.tier.price");

    assert!(
        state
            .db
            .get_subscription_tier(&author, "sold")
            .await
            .unwrap()
            .is_none(),
        "the gate runs BEFORE the insert — a refused create leaves no row"
    );
}

/// The editable half. `tiers.update` has no clear verb, so only a request that
/// actually carries a price is an operation to bind — an update that touches
/// the description must not be refused by the payments plane.
#[tokio::test]
async fn tier_repricing_is_gated_but_an_unrelated_update_is_not() {
    let (router, state, author) = harness().await;
    create_free_tier(&router, &state, author, "gold").await;
    deny(&state, &author, GatedFeature::Payments).await;

    // The control: a description edit carries no price and passes.
    encode_call(
        &router,
        state.clone(),
        "fauna.subscriptions.tiers.update",
        author,
        TierUpdateRequest {
            name: "gold".into(),
            rank: None,
            description: Some("still free".into()),
            price_hint: None,
            payment_url: None,
            auto_approve: None,
            unlocks_post: None,
            asking_price: None,
            extra: Default::default(),
        },
    )
    .await
    .expect("an unpriced update is not a payments operation");

    let error = encode_call(
        &router,
        state.clone(),
        "fauna.subscriptions.tiers.update",
        author,
        TierUpdateRequest {
            name: "gold".into(),
            rank: None,
            description: None,
            price_hint: None,
            payment_url: None,
            auto_approve: None,
            unlocks_post: None,
            asking_price: Some(TierAskingPrice::msats(21_000)),
            extra: Default::default(),
        },
    )
    .await
    .expect_err("attaching a price to an existing tier is a payments operation");
    refused_at(&error, "payments.tier.price");

    assert!(
        state
            .db
            .get_subscription_tier(&author, "gold")
            .await
            .unwrap()
            .expect("tier still exists")
            .asking_price()
            .is_none(),
        "the refused reprice never reached the row"
    );
}

// ── payments.paywall.designate ───────────────────────────────────

/// The tier half of the paywall surface: designating a tier as a per-post
/// unlock. Distinct from `tier.price` — a designation can be made with no price
/// at all, and a rule-setter may bind the two separately, which is why they are
/// separate registry rows.
#[tokio::test]
async fn post_unlock_designation_is_gated() {
    let (router, state, author) = harness().await;
    deny(&state, &author, GatedFeature::Payments).await;

    let mut req = priced_create("unlock");
    req.asking_price = None;
    req.unlocks_post = Some(hex::encode([7u8; 32]));

    let error = encode_call(
        &router,
        state.clone(),
        "fauna.subscriptions.tiers.create",
        author,
        req,
    )
    .await
    .expect_err("designating a post unlock is a payments operation");
    refused_at(&error, "payments.paywall.designate");
}

/// The folder half — and its de-escalation control. Clearing a paywall must
/// stay possible under a deny, or a tier that can only *tighten* would trap
/// content behind a paywall its owner can no longer remove.
#[tokio::test]
async fn web_paywall_designation_is_gated_but_clearing_it_is_not() {
    let (router, state, author) = harness().await;
    create_free_tier(&router, &state, author, "gold").await;
    // A real website set through the shipped create + update handlers — the
    // paywall handler checks the website toggle, so a hand-inserted row would
    // test a path production never reaches.
    encode_call(
        &router,
        state.clone(),
        "fauna.folders.create",
        author,
        FolderCreateRequest {
            name: "site".into(),
            ..Default::default()
        },
    )
    .await
    .expect("create the set");
    encode_call(
        &router,
        state.clone(),
        "fauna.folders.update",
        author,
        FolderUpdateRequest {
            name: "site".into(),
            website_enabled: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("serve the set as the website");
    deny(&state, &author, GatedFeature::Payments).await;

    let error = encode_call(
        &router,
        state.clone(),
        "fauna.folders.set_web_paywall",
        author,
        FolderSetWebPaywallRequest {
            name: "site".into(),
            tier: Some("gold".into()),
            name_hash: None,
            extra: Default::default(),
        },
    )
    .await
    .expect_err("paywalling a web set is a payments operation");
    refused_at(&error, "payments.paywall.designate");

    // The control: un-paywalling is never gated.
    encode_call(
        &router,
        state.clone(),
        "fauna.folders.set_web_paywall",
        author,
        FolderSetWebPaywallRequest {
            name: "site".into(),
            tier: None,
            name_hash: None,
            extra: Default::default(),
        },
    )
    .await
    .expect("clearing a paywall is de-escalation and must never be refused");
}

// ── payments.unlock.purchase ─────────────────────────────────────

/// The buy side's second surface, at the **provider-webhook** ingress — the one
/// gated door that is not WS-RPC, so its refusal is an HTTP status rather than
/// an `RpcError`.
///
/// Two things are pinned at once and both matter. The refusal must be **4xx**:
/// the module's own rule is that every rejection is non-2xx (the web-serving
/// catch-all would otherwise record a `200` as delivered), and a quota that is
/// not going to move should not invite the provider's retry ladder. And the
/// entitlement must not land — a gate that logged and continued would be
/// indistinguishable from this test's happy path in every other respect.
///
/// The zap-purchase ingress of the same surface is pinned in
/// `conformance_zap_purchase.rs`
/// (`a_payments_quota_degrades_a_zap_purchase_to_a_tip`), where the refusal
/// degrades to a tip instead — same surface, two mechanisms, two honest
/// failure shapes.
#[tokio::test]
async fn unlock_purchase_is_gated_at_the_webhook_ingress() {
    use axum::extract::{Path, State};
    use axum::http::HeaderMap;

    let (router, state, author) = harness().await;
    create_free_tier(&router, &state, author, "gold").await;
    state
        .db
        .upsert_payment_provider(&author, "fake", "s3cret", "gold")
        .await
        .unwrap();
    deny(&state, &author, GatedFeature::Payments).await;

    let buyer = ActorKeypair::generate().actor_id().0;
    let body = serde_json::json!({
        "id": "evt-1",
        "event": "payment",
        "reference": hex::encode(buyer),
    })
    .to_string();
    let mut headers = HeaderMap::new();
    headers.insert(
        fauna_payments::fake::SIGNATURE_HEADER,
        fauna_payments::fake::sign("s3cret", body.as_bytes())
            .parse()
            .unwrap(),
    );

    let response = fauna_nest::payment_routes::payment_webhook(
        State(state.clone()),
        Path((hex::encode(author), "fake".to_string())),
        headers,
        Bytes::from(body),
    )
    .await;

    assert_eq!(
        response.status(),
        axum::http::StatusCode::FORBIDDEN,
        "a gated payment is refused 4xx — never a 2xx the provider records as delivered"
    );
    assert_eq!(
        state
            .db
            .list_subscribe_requests(&author)
            .await
            .unwrap()
            .len(),
        0,
        "no entitlement may be granted past the gate"
    );
    assert!(
        state
            .db
            .list_payment_claims(&author)
            .await
            .unwrap()
            .is_empty(),
        "and no claim code was minted for the unbound fallback either"
    );
}

// ── zaps.signer.designate ────────────────────────────────────────

/// The zaps trust-root enablement. Also the subset edge at a real handler: the
/// deny below is authored on **`payments`**, and it must reach `zaps` — the
/// runtime half of "excising `payments` excises `zaps` with it".
#[cfg(all(feature = "zaps", feature = "nostr"))]
#[tokio::test]
async fn zap_signer_designation_is_gated_through_the_subset_edge() {
    use fauna_protocol::nostr::AddZapSignerRequest;

    let (router, state, author) = harness().await;
    deny(&state, &author, GatedFeature::Payments).await;

    assert!(
        state
            .db
            .feature_policies_for(&author, GatedFeature::Zaps)
            .await
            .unwrap()
            .is_empty(),
        "no zaps document of its own — the deny must arrive along the edge"
    );

    let error = encode_call(
        &router,
        state.clone(),
        "fauna.nostr.zap_signers.add",
        author,
        AddZapSignerRequest {
            signer_pubkey: hex::encode([3u8; 32]),
            label: "Alby".into(),
            extra: Default::default(),
        },
    )
    .await
    .expect_err("a payments deny reaches the zaps plane's trust root");
    refused_at(&error, "zaps.signer.designate");

    let conn = state.db.conn().await;
    let signers =
        fauna_nest::nostr::db::list_zap_signers(&conn, &hex::encode(author)).expect("list signers");
    drop(conn);
    assert!(
        signers.is_empty(),
        "the empty roster is load-bearing: a denied account must not be able to arm it"
    );
}
