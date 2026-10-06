#![cfg(feature = "nostr")]
//! The nsec-deposit gate for the server-side Nostr bridge (Phase-4 S8.9).
//!
//! Server-side Nostr bridging acts as the user's Nostr agent — it signs with
//! the user's key and unwraps NIP-17 gift-wrap DMs — so it runs exactly for
//! the users who made the explicit per-user trust act of depositing their
//! `nsec` on the box (`nostr_accounts.encrypted_privkey`,
//! `docs/goal/ui/nostr.md` § The bridging gate;
//! `docs/goal/architecture/nest/storage-modes.md` § The transition contract
//! rule 3: "nsec deposited (Nostr bridging)"). The box-level gate is
//! "at least one deposited nsec"; a box with no depositor has the Nostr
//! surface cleanly unavailable (the mail-`msek`-gating pattern).
//!
//! This replaced an interim box-level gate on the retired storage mode: the
//! widening is safe because inbound gift-wrap DM content now seals at ingest through
//! the D2 resolver (see the sealing probes in `nostr_relay_interop.rs` and
//! `sync_worker`'s unit tests).
//!
//! In-process (tier_1-ish): no nest binary, no client driver — just the gate
//! helper + the real `NostrProvider::available`.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use fauna_nest::bridge_management::{BridgeProvider, BridgeProviderRegistry};
use fauna_nest::bridges_ui_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::nostr::bridge_provider::NostrProvider;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::{
    Value,
    bridges_ui::{LinkReply, LinkRequest, ListBridgesReply, ListBridgesRequest},
    decode_strict as decode, encode_canonical,
};

/// Build an `AppState` over a fresh in-memory db with the nostr tables
/// created, optionally seeding a `nostr_accounts` row whose
/// `encrypted_privkey` presence is given by `deposited`.
async fn state_with_account(account: Option<bool>) -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    fauna_nest::nostr::init_db(&db)
        .await
        .expect("init nostr tables");
    if let Some(deposited) = account {
        let conn = db.conn().await;
        fauna_nest::nostr::db::link_account(
            &conn,
            "aa".repeat(32).as_str(),
            "bb".repeat(32).as_str(),
            if deposited { "generate" } else { "nip07" },
            deposited.then_some(&[0x42u8; 64][..]),
            None,
            None,
        )
        .unwrap();
    }
    Arc::new(AppState::for_test(db))
}

#[tokio::test]
async fn nostr_bridging_available_iff_an_nsec_is_deposited() {
    let fresh = state_with_account(None).await;
    assert!(
        !fauna_nest::nostr::nostr_bridging_available(&fresh).await,
        "a box with no Nostr accounts must NOT expose the Nostr bridge"
    );

    let no_deposit = state_with_account(Some(false)).await;
    assert!(
        !fauna_nest::nostr::nostr_bridging_available(&no_deposit).await,
        "a linked account WITHOUT a deposited nsec (NIP-07/bunker) must NOT \
         open the bridge — the box holds no key and no trust act was made"
    );

    let deposited = state_with_account(Some(true)).await;
    assert!(
        fauna_nest::nostr::nostr_bridging_available(&deposited).await,
        "a deposited nsec must open the bridge — this is the S8.9 widening \
         (the explicit per-user trust act)"
    );
}

#[tokio::test]
async fn nostr_provider_available_tracks_nsec_deposit() {
    let no_deposit = state_with_account(Some(false)).await;
    assert!(
        !NostrProvider.available(&no_deposit).await,
        "NostrProvider must report unavailable with no deposited nsec"
    );

    let deposited = state_with_account(Some(true)).await;
    assert!(
        NostrProvider.available(&deposited).await,
        "NostrProvider must report available once an nsec is deposited"
    );
}

#[tokio::test]
async fn gate_fails_closed_when_nostr_tables_are_absent() {
    // Boot ordering safety: if `init_db` has not created the nostr tables the
    // predicate must read `false` (fail closed), never error the caller.
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    assert!(
        !fauna_nest::nostr::nostr_bridging_available(&state).await,
        "missing nostr tables must fail closed"
    );
}

// ── the nsec-deposit gate deadlocking the first deposit ────
//
// The three tests above prove the *predicate*: a fresh box's `available()`
// reads false. This section proves the *handler sequence* the predicate
// feeds into — `fauna.bridges.link` — the level the deadlock actually lives
// at (`link_handler` gates `provider.link(...)` on `provider.available()`
// BEFORE it runs, so the deposit-creating call that would flip `available()`
// true can never execute on a box with zero deposits). A handler-level test
// through the real `NostrProvider`, not the direct-predicate calls above,
// is required to catch this class of bug.

/// Real `NostrProvider` wired into a live `RpcRouter`, mirroring
/// `conformance_bridges_ui.rs::router_with_providers` but nostr-specific (the
/// real provider, not a `FakeBridge`) — the review's "handler-level, not
/// db-direct" requirement.
async fn router_with_real_nostr_provider() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    fauna_nest::nostr::init_db(&db)
        .await
        .expect("init nostr tables");
    let mut state = AppState::for_test(db);
    // A "generate" link seals the deposit under the seed the database holds.
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
    let mut registry = BridgeProviderRegistry::new();
    registry.register(Box::new(NostrProvider));
    state.bridge.providers = Some(Arc::new(registry));
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    bridges_ui_handlers::register_bridges_ui_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch_link(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    mode: &str,
) -> Result<LinkReply, fauna_protocol::RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let req = LinkRequest {
        bridge_id: "nostr".into(),
        mode: mode.into(),
        params: Value::Map(BTreeMap::new()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let meta = router
        .kind_meta("fauna.bridges.link")
        .expect("kind registered");
    let reply_bytes = (meta.handler)(state, actor, payload).await?;
    Ok(decode(&reply_bytes).unwrap())
}

#[tokio::test]
async fn generate_link_succeeds_on_a_fresh_box_with_zero_deposits() {
    let (router, state) = router_with_real_nostr_provider().await;
    // Sanity: this box really is in the deadlocked state the predicate tests
    // above already prove — zero deposits, so `available()` reads false.
    assert!(
        !NostrProvider.available(&state).await,
        "a fresh box must start unavailable (no deposit yet)"
    );

    // The FIRST-EVER link on this box, via the deposit-creating "generate"
    // mode, must succeed despite `available()` being false — that call is
    // what bootstraps availability. Pre-fix this returned `unavailable`: the box could
    // never mint its first custodial key.
    let reply = dispatch_link(&router, state.clone(), [1u8; 32], "generate")
        .await
        .expect("generate must be reachable on a zero-deposit box");
    assert!(reply.linked, "generate must report linked");
    assert!(
        reply.identity.is_some(),
        "generate must return the new pubkey"
    );

    // And the deposit really did bootstrap availability for the box.
    assert!(
        NostrProvider.available(&state).await,
        "the box must be available immediately after its first deposit"
    );
}

#[tokio::test]
async fn nip07_link_stays_gated_on_availability_on_a_fresh_box() {
    // The fix must be narrowly scoped to the deposit-creating modes — every
    // other already-gated bridging act, including a non-custodial link mode
    // like nip07, must stay gated exactly as before (the review's "while
    // every already-gated bridging act stays gated" requirement).
    let (router, state) = router_with_real_nostr_provider().await;
    let err = dispatch_link(&router, state, [2u8; 32], "nip07")
        .await
        .expect_err("nip07 must stay gated on a zero-deposit box");
    assert_eq!(err.code, "fauna.bridges.not_found");
}

#[tokio::test]
async fn list_surfaces_link_modes_for_an_unlinked_actor_on_a_fresh_box() {
    // The `generate_link_succeeds...` fix above is unreachable from any
    // client unless `fauna.bridges.list` also tells the client a link is
    // worth attempting: pre-fix, `list_handler` short-circuited on
    // `!available` and never surfaced `link_modes`, so no client could ever
    // discover that `generate`/`import` were reachable — the bootstrap fix
    // in `link_handler` was live at the wire but invisible to every UI.
    let (router, state) = router_with_real_nostr_provider().await;
    common::seed_dispatch_actor(&state.db, &[3u8; 32]).await;
    let req = ListBridgesRequest {};
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let meta = router
        .kind_meta("fauna.bridges.list")
        .expect("kind registered");
    let reply_bytes = (meta.handler)(state, [3u8; 32], payload)
        .await
        .expect("list ok");
    let reply: ListBridgesReply = decode(&reply_bytes).unwrap();
    let nostr = reply
        .bridges
        .iter()
        .find(|b| b.id == "nostr")
        .expect("nostr bridge present in the list");
    assert!(!nostr.available, "sanity: still unavailable, zero deposits");
    assert!(!nostr.linked, "sanity: this actor has no account yet");
    let modes = nostr
        .link_modes
        .as_ref()
        .expect("link_modes must be discoverable even while unavailable");
    assert!(
        modes.iter().any(|m| m.mode == "generate"),
        "the deposit-bootstrapping generate mode must be offered"
    );
}

// ── P2.5 serving-vs-agency split (spec R8 (account-data-plane.md § The ratified decisions)) ────────────────────────────────
//
// `nostr_serving_available` gates the relay ENDPOINTS (`/nostr` WS +
// `/nostr/info`): serve when a user deposited an nsec (this box is itself a
// head) OR when some actor holds a non-expired `nostr_push` pairing (this box
// is the keyless public serving face of a paired head). Agency
// (`nostr_bridging_available`) stays nsec-only — serving ≠ agency.

#[tokio::test]
async fn nostr_serving_available_via_deposit_or_pairing_but_agency_stays_nsec_only() {
    use fauna_protocol::pair::capability::NOSTR_PUSH;

    // Neither a deposit nor a pairing → serving closed.
    let bare = state_with_account(None).await;
    assert!(
        !fauna_nest::nostr::nostr_serving_available(&bare).await,
        "no nsec deposit and no nostr_push pairing → relay must not serve"
    );

    // A deposited nsec (this box is itself a head) → serving open (Phase 1).
    let deposited = state_with_account(Some(true)).await;
    assert!(
        fauna_nest::nostr::nostr_serving_available(&deposited).await,
        "a deposited nsec opens serving (Phase 1)"
    );

    // A keyless box (no account at all) with a `nostr_push` pairing → serving
    // open (Phase 2: the public serving face of a paired head).
    let proxied = state_with_account(None).await;
    proxied
        .db
        .store_pairing(
            &[0xA1u8; 32],
            &[0xB2u8; 32],
            &[NOSTR_PUSH.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        fauna_nest::nostr::nostr_serving_available(&proxied).await,
        "a nostr_push pairing opens serving on a keyless box (Phase 2 proxy)"
    );
    // ...but AGENCY stays nsec-only: the keyless box runs no agent acts.
    assert!(
        !fauna_nest::nostr::nostr_bridging_available(&proxied).await,
        "serving ≠ agency: a pairing does NOT open the agent surface"
    );

    // Per-request derivation: revoking the pairing flips serving back to closed
    // with no restart / no boot reconcile.
    proxied
        .db
        .revoke_pairing(&[0xA1u8; 32], &[0xB2u8; 32])
        .await
        .unwrap();
    assert!(
        !fauna_nest::nostr::nostr_serving_available(&proxied).await,
        "revoking the pairing flips serving back to unavailable immediately"
    );

    // An EXPIRED nostr_push pairing must not open serving.
    let expired = state_with_account(None).await;
    expired
        .db
        .store_pairing(
            &[0xC3u8; 32],
            &[0xD4u8; 32],
            &[NOSTR_PUSH.to_string()],
            Some(1), // long-expired
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        !fauna_nest::nostr::nostr_serving_available(&expired).await,
        "an expired nostr_push pairing must not open serving"
    );
}

#[tokio::test]
async fn relay_info_endpoint_503s_until_a_nostr_push_pairing_flips_it_to_serving() {
    use axum::extract::State;
    use axum::http::StatusCode;
    use fauna_nest::nostr::relay_endpoint::info_handler;
    use fauna_protocol::pair::capability::NOSTR_PUSH;

    // The REAL `/nostr/info` handler exercising the shared serving gate. Its
    // gate line is identical to `ws_handler`'s (P2.5 swapped both onto
    // `nostr_serving_available`), so this one handler proves the wiring for the
    // relay-serving pair.
    let state = state_with_account(None).await;
    let resp = info_handler(State(state.clone())).await;
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "keyless box, no pairing → /nostr/info 503s"
    );

    // A paired head enrolls this box as its serving face (lands a `nostr_push`
    // pairing). The very next request flips to serving — no restart.
    state
        .db
        .store_pairing(
            &[0x51u8; 32],
            &[0x62u8; 32],
            &[NOSTR_PUSH.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let resp = info_handler(State(state.clone())).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a nostr_push pairing flips the relay to serving (503 → 200) with no deposit"
    );
}
