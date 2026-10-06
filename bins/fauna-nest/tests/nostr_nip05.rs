#![cfg(feature = "nostr")]
//! **NIP-05 serving — tier_3 acceptance for `GET /.well-known/nostr.json`**
//! (`docs/goal/ui/nostr.md` § Goal — the `you@<nest-domain>` identity leg — +
//! § Architecture; N1).
//!
//! Drives the route **through the real `build_router`** — the same axum app
//! `main.rs` serves, so this proves the route is actually mounted (behind the
//! `nostr` cargo feature) and returns the NIP-05 wire shape:
//!
//!  * a linked account's handle resolves to its **hex** pubkey plus a `relays`
//!    hint pointing at the nest's own `wss://<domain>/nostr` relay;
//!  * serving is keyed on the account being **linked**, not on a deposited nsec
//!    — an NIP-07/remote account (no `encrypted_privkey`) is still served
//!    (identity ≠ agency; `nostr.md` § The bridging gate gates *agent acts*, not
//!    identity publication);
//!  * an **unknown** name and an **absent** `?name=` both return an empty
//!    `names` map — no enumeration (the F5/SMTP-enumeration posture,
//!    `network-exposure.md`);
//!  * a handle that exists but has **no linked Nostr account** returns empty;
//!  * NIP-05 names are matched **case-insensitively** (handles rest lowercase);
//!  * every response carries `Access-Control-Allow-Origin: *` (NIP-05 is fetched
//!    cross-origin by web clients).
//!
//! Only compiled under `--features nostr`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ed25519_dalek::SigningKey;
use fauna_bridge_nostr::signing::Keypair;
use fauna_nest::db::CacheDb;
use fauna_nest::nostr::{self, db};
use fauna_nest::routes::AppState;
use tower::ServiceExt; // oneshot

const DOMAIN: &str = "nest.test";

/// A fresh in-memory `AppState` with the `nostr_*` tables created and
/// `config.nest.domain = Some(DOMAIN)` (the relay-hint source). Built via TOML
/// because `NestConfig` is not `Clone` — the escape hatch config.rs's own tests
/// use.
async fn state_with_domain() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let toml = format!(
        r#"
[nest]
mode = "public"
listen = "127.0.0.1:0"
db_path = "nest.db"
domain = "{DOMAIN}"
"#
    );
    let config: fauna_nest::config::NestConfig = toml::from_str(&toml).expect("parse test config");
    Arc::new(AppState {
        config: Arc::new(config),
        ..AppState::for_test(db)
    })
}

/// Deterministic actor id from a byte seed.
fn actor_from(seed: u8) -> [u8; 32] {
    SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .to_bytes()
}

/// Register `handle` on a fresh actor and link a Nostr account to it. `deposit`
/// controls whether the account carries an encrypted nsec (a custodial
/// depositor) or none (an NIP-07/remote account). Returns the account's hex
/// pubkey.
async fn seed_linked_account(state: &AppState, seed: u8, handle: &str, deposit: bool) -> String {
    let actor = actor_from(seed);
    state.db.create_user(&actor, "free", handle).await.unwrap();
    state.db.set_handle(&actor, handle).await.unwrap();

    let kp = Keypair::generate();
    let pubkey_hex = kp.public_key_hex();
    let encrypted = deposit.then(|| vec![0xABu8; 48]);

    let conn = state.db.conn().await;
    db::link_account(
        &conn,
        &hex::encode(actor),
        &pubkey_hex,
        if deposit { "generated" } else { "nip07" },
        encrypted.as_deref(),
        None,
        None,
    )
    .unwrap();
    drop(conn);
    pubkey_hex
}

/// GET `/.well-known/nostr.json<query>` over the real router. Returns
/// `(status, cors_header, parsed_json_body)`.
async fn get_nip05(
    state: &Arc<AppState>,
    query: &str,
) -> (StatusCode, Option<String>, serde_json::Value) {
    let app = fauna_nest::build_router(state.clone());
    let req = Request::builder()
        .uri(format!("/.well-known/nostr.json{query}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let cors = resp
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, cors, json)
}

/// The headline: a linked handle resolves to its hex pubkey + the nest's own
/// relay hint, with CORS `*`.
#[tokio::test]
async fn nip05_serves_linked_pubkey_with_relay_hint() {
    let state = state_with_domain().await;
    let pubkey = seed_linked_account(&state, 0x11, "alice", true).await;

    let (status, cors, body) = get_nip05(&state, "?name=alice").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        cors.as_deref(),
        Some("*"),
        "NIP-05 must allow cross-origin: {body}"
    );
    assert_eq!(
        body["names"]["alice"].as_str(),
        Some(pubkey.as_str()),
        "names[alice] must be the hex pubkey: {body}"
    );
    assert_eq!(
        body["relays"][&pubkey][0].as_str(),
        Some(format!("wss://{DOMAIN}/nostr").as_str()),
        "relays hint must point at the nest's own relay: {body}"
    );
}

/// Identity ≠ agency: an NIP-07/remote account (no deposited nsec) is still
/// served — NIP-05 publishes an identity, it is not an agent act.
#[tokio::test]
async fn nip05_serves_non_depositor_account() {
    let state = state_with_domain().await;
    let pubkey = seed_linked_account(&state, 0x22, "bob", false).await;

    let (status, _cors, body) = get_nip05(&state, "?name=bob").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["names"]["bob"].as_str(),
        Some(pubkey.as_str()),
        "a linked non-depositor account is served (identity, not agency): {body}"
    );
}

/// NIP-05 names are case-insensitive; handles rest lowercase.
#[tokio::test]
async fn nip05_name_lookup_is_case_insensitive() {
    let state = state_with_domain().await;
    let pubkey = seed_linked_account(&state, 0x33, "carol", true).await;

    let (status, _cors, body) = get_nip05(&state, "?name=CAROL").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["names"]["carol"].as_str(),
        Some(pubkey.as_str()),
        "mixed-case query resolves the lowercase handle: {body}"
    );
}

/// An unknown name returns an empty `names` map (no enumeration) + CORS.
#[tokio::test]
async fn nip05_unknown_name_returns_empty_names() {
    let state = state_with_domain().await;
    seed_linked_account(&state, 0x44, "dave", true).await;

    let (status, cors, body) = get_nip05(&state, "?name=nobody").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cors.as_deref(), Some("*"));
    assert!(
        body["names"].as_object().is_some_and(|m| m.is_empty()),
        "unknown name must not leak any mapping: {body}"
    );
    assert!(
        body.get("relays").is_none(),
        "no relays for an empty answer: {body}"
    );
}

/// An absent `?name=` returns an empty `names` map — never a full dump.
#[tokio::test]
async fn nip05_absent_name_returns_empty_names() {
    let state = state_with_domain().await;
    seed_linked_account(&state, 0x55, "erin", true).await;

    let (status, _cors, body) = get_nip05(&state, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["names"].as_object().is_some_and(|m| m.is_empty()),
        "an unfiltered request must never dump all names: {body}"
    );
}

/// A handle that exists but has no linked Nostr account returns empty.
#[tokio::test]
async fn nip05_handle_without_nostr_account_returns_empty() {
    let state = state_with_domain().await;
    let actor = actor_from(0x66);
    state.db.create_user(&actor, "free", "frank").await.unwrap();
    state.db.set_handle(&actor, "frank").await.unwrap();
    // No link_account.

    let (status, _cors, body) = get_nip05(&state, "?name=frank").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["names"].as_object().is_some_and(|m| m.is_empty()),
        "a handle with no Nostr link is not a NIP-05 identity: {body}"
    );
}
