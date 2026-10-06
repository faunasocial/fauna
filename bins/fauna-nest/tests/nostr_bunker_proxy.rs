#![cfg(feature = "nostr")]
//! Tier_3 focused tests for the NIP-46 bunker **proxy-delegation leg** (spec
//! P2.6 / R10 (account-data-plane.md § The ratified decisions); `docs/goal/ui/nostr.md` § The bridging gate → Phase 2). The
//! full app→public→head→public→app round-trip over two live WebSocket relays is
//! the deferred tier_3 T6 (the sibling `conformance_nostr_proxy_delegation.rs`
//! or a follow-on); this file pins the three mechanism pieces directly:
//!
//!  * **Part A — public-box ephemeral fall-through.** A keyless *serving* box
//!    (holds a `nostr_push` pairing, no local deposit) transports a kind-24133
//!    for a signer it doesn't host: broadcast to live subscribers, never
//!    stored. Without the pairing it declines (`None`) and the relay is 503.
//!  * **Part B — the shared bunker core the head runs.** `execute_bunker_request`
//!    turns a real encrypted request into a valid signer-authored, app-encrypted
//!    response — the exact core the head publishes back to the peer relay.
//!  * **Part C — relay-hint resolution.** `preferred_public_relay_url` prefers
//!    the `nostr_push` peer's `wss://…/nostr` (the `create_invite` connect
//!    string + the NIP-65/10050 self-advertisement both consume it), else `None`
//!    so the caller keeps its own-domain fallback.
//!
//! Only compiled under `--features nostr`.

use std::sync::Arc;

use fauna_bridge_nostr::nip01::RelayMessage;
use fauna_bridge_nostr::nip44;
use fauna_bridge_nostr::nip46::build_response_json;
use fauna_bridge_nostr::signing::{Keypair, verify_event};
use fauna_bridge_nostr::types::{Event, Tag, UnsignedEvent};
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::relay_endpoint::handle_bunker_request;
use fauna_nest::nostr::{self, bunker, db};
use fauna_nest::routes::AppState;
use fauna_protocol::pair::capability::NOSTR_PUSH;

/// A fresh in-memory `AppState` with the `nostr_*` tables created.
async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let state = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
    Arc::new(state)
}

/// Link a custodial account (deposited nsec) and return the user's keypair.
async fn link_custodial(state: &AppState, actor_hex: &str) -> Keypair {
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let kp = Keypair::generate();
    let encrypted = encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).unwrap();
    let conn = state.db.conn().await;
    db::link_account(
        &conn,
        actor_hex,
        &kp.public_key_hex(),
        "generated",
        Some(&encrypted),
        None,
        None,
    )
    .unwrap();
    drop(conn);
    kp
}

/// Mint an invite (registers the account's bunker signer).
async fn invite(state: &AppState, actor_hex: &str) -> bunker::BunkerInvite {
    let conn = state.db.conn().await;
    bunker::create_invite(&conn, actor_hex, now_secs()).unwrap()
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// Build a kind-24133 request event as an app would: NIP-44-encrypted request
/// JSON to the signer pubkey, `p`-tag naming the signer, signed by the app.
fn request_event(app: &Keypair, signer_pubkey_hex: &str, request_json: &str) -> Event {
    let mut signer_pk = [0u8; 32];
    signer_pk.copy_from_slice(&hex::decode(signer_pubkey_hex).unwrap());
    let content = nip44::nip44_encrypt(&app.secret_bytes(), &signer_pk, request_json).unwrap();
    let unsigned = UnsignedEvent {
        pubkey: app.public_key_bytes(),
        created_at: now_secs(),
        kind: 24133,
        tags: vec![Tag::new(vec!["p".into(), signer_pubkey_hex.to_string()])],
        content,
    };
    app.sign_event(unsigned)
}

fn rpc(id: &str, method: &str, params: &[&str]) -> String {
    serde_json::to_string(&serde_json::json!({
        "id": id, "method": method, "params": params,
    }))
    .unwrap()
}

/// Decrypt the app-facing response event content.
fn open_response(app: &Keypair, signer_pubkey_hex: &str, response: &Event) -> String {
    let mut signer_pk = [0u8; 32];
    signer_pk.copy_from_slice(&hex::decode(signer_pubkey_hex).unwrap());
    nip44::nip44_decrypt(&app.secret_bytes(), &signer_pk, &response.content).unwrap()
}

/// Add a `nostr_push` pairing for `actor` pointing at `peer_https_base`.
async fn add_nostr_push_pairing(state: &AppState, actor: &[u8], peer_https_base: &str) {
    state
        .db
        .store_pairing(
            actor,
            &[0xAAu8; 32],
            &[NOSTR_PUSH.to_string()],
            None,
            Some(peer_https_base),
            None,
        )
        .await
        .unwrap();
}

/// Authorize an app via the shared execute core's connect flow; returns the app
/// keypair (now an `active` connection).
async fn connect_app_core(state: &AppState, inv: &bunker::BunkerInvite) -> Keypair {
    let app = Keypair::generate();
    let req = request_event(
        &app,
        &inv.signer_pubkey,
        &rpc("c1", "connect", &[&inv.signer_pubkey, &inv.secret]),
    );
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let conn = state.db.conn().await;
    let resp =
        bunker::execute_bunker_request(&conn, &nest_key, &inv.signer_pubkey, &req, now_secs())
            .expect("connect executes");
    drop(conn);
    assert_eq!(
        open_response(&app, &inv.signer_pubkey, &resp),
        build_response_json("c1", Ok("ack")),
        "connect must ack"
    );
    app
}

// ── Part A — public-box ephemeral fall-through ─────────────────────────

#[tokio::test]
async fn part_a_paired_public_box_broadcasts_unregistered_24133_and_never_stores() {
    let state = state().await;

    // A keyless public *serving* box: no local deposit, but a `nostr_push`
    // pairing → serving is pairing-derived-available (agency stays unavailable).
    let actor = [0x33u8; 32];
    add_nostr_push_pairing(&state, &actor, "https://head.example").await;
    assert!(
        nostr::nostr_serving_available(&state).await,
        "a nostr_push pairing makes the relay serve"
    );
    assert!(
        !nostr::nostr_bridging_available(&state).await,
        "serving ≠ agency: no deposit, no agent acts"
    );

    // A validly-signed 24133 for a signer this box does NOT host (bunker signer
    // rows live only on the head). The public box has no registered signers.
    let app = Keypair::generate();
    let signer = Keypair::generate().public_key_hex();
    let event = request_event(&app, &signer, &rpc("x", "ping", &[]));

    let mut rx = state.nostr.relay_tx.subscribe();
    let ok = handle_bunker_request(&state, &event)
        .await
        .expect("a proxy serving face handles (Some) the ephemeral transport");
    assert!(
        matches!(ok, RelayMessage::Ok { accepted: true, .. }),
        "{ok:?}"
    );

    // Broadcast to the live subscriber…
    let broadcast = rx.try_recv().expect("the 24133 must be broadcast");
    let parsed: Event = serde_json::from_str(&broadcast.event_json).unwrap();
    assert_eq!(parsed.id, event.id, "the exact request is fanned out");

    // …and NOT stored (kind 24133 is ephemeral, 20000–29999).
    let conn = state.db.conn().await;
    let stored: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM nostr_events WHERE id = ?1",
            [&event.id],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    assert_eq!(stored, 0, "ephemeral 24133 must never be stored");
}

#[tokio::test]
async fn part_a_response_direction_24133_also_broadcasts_on_the_public_box() {
    // The head's signer-authored response `p`-tags the APP (not a signer), so it
    // too misses local membership and must ride the same ephemeral fall-through
    // back to the app's open REQ.
    let state = state().await;
    let actor = [0x34u8; 32];
    add_nostr_push_pairing(&state, &actor, "https://head.example").await;

    let signer = Keypair::generate();
    let app_pubkey = Keypair::generate().public_key_hex();
    // A response is a 24133 authored by the signer key, p-tagging the app.
    let unsigned = UnsignedEvent {
        pubkey: signer.public_key_bytes(),
        created_at: now_secs(),
        kind: 24133,
        tags: vec![Tag::new(vec!["p".into(), app_pubkey])],
        content: "opaque response ciphertext".into(),
    };
    let response = signer.sign_event(unsigned);

    let mut rx = state.nostr.relay_tx.subscribe();
    let ok = handle_bunker_request(&state, &response)
        .await
        .expect("the response direction is transported too");
    assert!(matches!(ok, RelayMessage::Ok { accepted: true, .. }));
    let broadcast = rx.try_recv().expect("the response must be broadcast");
    let parsed: Event = serde_json::from_str(&broadcast.event_json).unwrap();
    assert_eq!(parsed.id, response.id);
}

#[tokio::test]
async fn part_a_keyless_box_without_pairing_declines_and_relay_is_unavailable() {
    let state = state().await;
    // No deposit, no pairing → the relay 503s and the carve-out declines.
    assert!(
        !nostr::nostr_serving_available(&state).await,
        "no deposit + no pairing → relay unavailable (503)"
    );
    let app = Keypair::generate();
    let signer = Keypair::generate().public_key_hex();
    let event = request_event(&app, &signer, &rpc("x", "ping", &[]));
    assert!(
        handle_bunker_request(&state, &event).await.is_none(),
        "an ordinary box declines (None) → generic auth-required fall-through"
    );
}

#[tokio::test]
async fn part_a_unsigned_probe_is_rejected_before_the_fall_through() {
    // F4 blindness is preserved: the signature check runs ahead of the
    // pairing/membership branch, so an unsigned probe never reaches the
    // ephemeral broadcast even on a proxy face.
    let state = state().await;
    let actor = [0x35u8; 32];
    add_nostr_push_pairing(&state, &actor, "https://head.example").await;

    let app = Keypair::generate();
    let signer = Keypair::generate().public_key_hex();
    let mut event = request_event(&app, &signer, &rpc("x", "ping", &[]));
    event.sig = "00".repeat(64); // invalidate

    let mut rx = state.nostr.relay_tx.subscribe();
    let ok = handle_bunker_request(&state, &event).await.unwrap();
    assert!(
        matches!(
            ok,
            RelayMessage::Ok {
                accepted: false,
                ..
            }
        ),
        "unsigned probe must be rejected, {ok:?}"
    );
    assert!(
        rx.try_recv().is_err(),
        "an unsigned probe must NOT be broadcast"
    );
}

// ── Part B — the shared bunker core the head runs ──────────────────────

#[tokio::test]
async fn part_b_head_core_builds_valid_signed_responses() {
    let state = state().await;
    let user_kp = link_custodial(&state, "actor-head").await;
    let inv = invite(&state, "actor-head").await;
    let app = connect_app_core(&state, &inv).await;

    let nest_key = state.nest_identity.signing_key.to_bytes();

    // ping → a signer-authored, verifying 24133 that decrypts to the NIP-46 result.
    let req = request_event(&app, &inv.signer_pubkey, &rpc("p1", "ping", &[]));
    let conn = state.db.conn().await;
    let response =
        bunker::execute_bunker_request(&conn, &nest_key, &inv.signer_pubkey, &req, now_secs())
            .expect("the shared core builds a response");
    drop(conn);
    assert_eq!(response.kind, 24133);
    assert_eq!(
        response.pubkey, inv.signer_pubkey,
        "authored by the dedicated signer key"
    );
    assert!(verify_event(&response), "response must verify");
    assert_eq!(
        open_response(&app, &inv.signer_pubkey, &response),
        build_response_json("p1", Ok("pong"))
    );
    // The response `p`-tags the app (so it routes back to the app's REQ).
    assert_eq!(
        response
            .tags
            .iter()
            .find(|t| t.name() == Some("p"))
            .and_then(|t| t.value()),
        Some(app.public_key_hex().as_str())
    );

    // sign_event → the response carries an event signed under the USER key.
    let unsigned_json =
        r#"{"kind":1,"content":"signed via the proxy head","tags":[],"created_at":1700000000}"#;
    let sign_req = request_event(
        &app,
        &inv.signer_pubkey,
        &rpc("s1", "sign_event", &[unsigned_json]),
    );
    let conn = state.db.conn().await;
    let sign_resp =
        bunker::execute_bunker_request(&conn, &nest_key, &inv.signer_pubkey, &sign_req, now_secs())
            .unwrap();
    drop(conn);
    let plaintext = open_response(&app, &inv.signer_pubkey, &sign_resp);
    let v: serde_json::Value = serde_json::from_str(&plaintext).unwrap();
    assert_eq!(v["id"], "s1");
    let signed: Event = serde_json::from_str(v["result"].as_str().unwrap()).unwrap();
    assert_eq!(signed.kind, 1);
    assert_eq!(signed.content, "signed via the proxy head");
    assert_eq!(
        signed.pubkey,
        user_kp.public_key_hex(),
        "signed under the deposited USER key, not the signer key"
    );
    assert!(verify_event(&signed));
}

#[tokio::test]
async fn part_b_head_core_answers_an_unauthorized_app_without_a_silent_drop() {
    // An app that never connected is unauthorized — the core still round-trips a
    // signed error response (never a silent drop).
    let state = state().await;
    link_custodial(&state, "actor-noauth").await;
    let inv = invite(&state, "actor-noauth").await;
    let stranger = Keypair::generate();

    let req = request_event(&stranger, &inv.signer_pubkey, &rpc("p9", "ping", &[]));
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let conn = state.db.conn().await;
    let response =
        bunker::execute_bunker_request(&conn, &nest_key, &inv.signer_pubkey, &req, now_secs())
            .expect("an unauthorized request still builds an error response");
    drop(conn);
    assert_eq!(
        open_response(&stranger, &inv.signer_pubkey, &response),
        build_response_json("p9", Err("unauthorized"))
    );
}

// ── Part C — relay-hint resolution ─────────────────────────────────────

#[tokio::test]
async fn part_c_preferred_relay_prefers_nostr_push_peer_else_none() {
    let state = state().await;
    let actor = [0x44u8; 32];
    let actor_hex = hex::encode(actor);

    // No pairing → None (the caller keeps its own-domain fallback — the
    // pre-Phase-2 behavior for a head that is its own public face).
    assert_eq!(
        nostr::relays::preferred_public_relay_url(&state, &actor_hex).await,
        None
    );

    // A `nostr_push` pairing carrying an https base → the peer's `wss://…/nostr`.
    add_nostr_push_pairing(&state, &actor, "https://public.example").await;
    assert_eq!(
        nostr::relays::preferred_public_relay_url(&state, &actor_hex)
            .await
            .as_deref(),
        Some("wss://public.example/nostr"),
    );

    // A pairing WITHOUT the `nostr_push` capability does not qualify.
    let other = [0x45u8; 32];
    state
        .db
        .store_pairing(
            &other,
            &[0xCCu8; 32],
            &["namespace_sync".to_string()],
            None,
            Some("https://nope.example"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        nostr::relays::preferred_public_relay_url(&state, &hex::encode(other)).await,
        None,
        "a non-nostr_push pairing is not a relay-hint source"
    );
}
