#![cfg(feature = "nostr")]
//! Tier_3 integration tests for the NIP-46 bunker (`docs/goal/ui/nostr.md`
//! § The nest as the user's NIP-46 signer). Drives the relay carve-out
//! (`handle_bunker_request`) with a real `AppState` + real `nostr_*` tables,
//! the same shape as `nostr_gift_wrap_inbox.rs`, proving the properties the
//! `bunker.rs` unit tests cannot:
//!
//!  * **F4 pin:** an *unsigned* kind-24133 probe at a registered vs an
//!    unregistered signer pubkey gets **byte-identical** rejections —
//!    signer-registry membership is never decidable without a valid signature;
//!  * a validly-signed request at an **unregistered** signer falls through to
//!    the generic (auth-required/restricted) path — indistinguishable from any
//!    other non-local event;
//!  * the full connect → sign_event round-trip over the carve-out: request
//!    broadcast per ephemeral semantics, response event signed by the signer
//!    key, encrypted to the app, riding `relay_tx`;
//!  * revocation/idle-expiry are per-request (no cached authority survives);
//!  * the per-signer rate limit binds;
//!  * bunker state survives a nest restart (same DB, fresh `AppState`).
//!
//! Only compiled under `--features nostr`.

use std::sync::Arc;

use fauna_bridge_nostr::nip01::RelayMessage;
use fauna_bridge_nostr::nip44;
use fauna_bridge_nostr::nip46::{EncryptionScheme, build_response_json};
use fauna_bridge_nostr::signing::{Keypair, verify_event};
use fauna_bridge_nostr::types::{Event, Tag, UnsignedEvent};
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::relay_endpoint::handle_bunker_request;
use fauna_nest::nostr::{self, bunker, db};
use fauna_nest::routes::AppState;

/// A fresh in-memory `AppState` with the `nostr_*` tables created and the
/// default `NostrState` (its bunker limiter enabled).
async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let state = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
    Arc::new(state)
}

/// Link a custodial account (deposited nsec encrypted under the state's own
/// nest key) and return the user's Nostr keypair.
async fn link_custodial(state: &AppState, actor_hex: &str) -> Keypair {
    // The `users` row the signer reads the account's authority from: a
    // roster app is served only while the account stands (suspended or
    // locked out, every request is `unauthorized`).
    let mut actor = [0u8; 32];
    actor.copy_from_slice(&hex::decode(actor_hex).unwrap());
    state.db.create_user(&actor, "free", "test").await.unwrap();
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

/// Mint an invite for `actor_hex` via the S2 control-plane core.
async fn invite(state: &AppState, actor_hex: &str) -> bunker::BunkerInvite {
    let now = now_secs();
    let conn = state.db.conn().await;
    bunker::create_invite(&conn, actor_hex, now).unwrap()
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// A deterministic 32-byte actor id as the hex the nostr tables key by.
fn actor(seed: u8) -> String {
    hex::encode([seed; 32])
}

/// Build a kind-24133 request event as the app would: NIP-44-encrypted
/// request JSON to the signer pubkey, p-tag naming the signer, signed by the
/// app keypair.
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

/// Drive one request through the carve-out and expect it handled (Some).
async fn drive(state: &Arc<AppState>, event: &Event) -> RelayMessage {
    handle_bunker_request(state, event)
        .await
        .expect("event addressed to a registered signer must be handled")
}

/// Decrypt the app-facing response event content.
fn open_response(app: &Keypair, signer_pubkey_hex: &str, response: &Event) -> String {
    let mut signer_pk = [0u8; 32];
    signer_pk.copy_from_slice(&hex::decode(signer_pubkey_hex).unwrap());
    nip44::nip44_decrypt(&app.secret_bytes(), &signer_pk, &response.content).unwrap()
}

/// Connect an app over the carve-out; returns the app keypair.
async fn connect_app(state: &Arc<AppState>, inv: &bunker::BunkerInvite) -> Keypair {
    let app = Keypair::generate();
    let req = request_event(
        &app,
        &inv.signer_pubkey,
        &rpc("c1", "connect", &[&inv.signer_pubkey, &inv.secret]),
    );
    let ok = drive(state, &req).await;
    assert!(
        matches!(ok, RelayMessage::Ok { accepted: true, .. }),
        "connect not accepted: {ok:?}"
    );
    app
}

#[tokio::test]
async fn f4_unsigned_probe_is_membership_blind() {
    let state = state().await;
    link_custodial(&state, &actor(0x01)).await;
    let inv = invite(&state, &actor(0x01)).await;

    // Two identical probes, differing ONLY in the target pubkey: one at the
    // real registered signer, one at a pubkey that is no signer. Both carry a
    // garbage signature.
    let app = Keypair::generate();
    let make_probe = |target_hex: &str| {
        let mut e = request_event(&app, target_hex, &rpc("x", "ping", &[]));
        e.sig = "00".repeat(64); // invalidate: an unsigned probe
        e
    };
    let unregistered = Keypair::generate().public_key_hex();
    let probe_registered = make_probe(&inv.signer_pubkey);
    let probe_unregistered = make_probe(&unregistered);

    let r1 = handle_bunker_request(&state, &probe_registered).await;
    let r2 = handle_bunker_request(&state, &probe_unregistered).await;

    // Both must be handled the same way (rejected pre-membership) and the
    // rejection payloads must be identical apart from the event id.
    let msg = |r: &Option<RelayMessage>| match r {
        Some(RelayMessage::Ok {
            accepted, message, ..
        }) => (*accepted, message.clone()),
        other => panic!("unsigned probe must get an OK-reject, got {other:?}"),
    };
    assert_eq!(
        msg(&r1),
        msg(&r2),
        "an unsigned probe must not distinguish a registered signer pubkey"
    );
    assert!(!msg(&r1).0, "unsigned probe must be rejected");
}

#[tokio::test]
async fn valid_signature_at_unregistered_signer_falls_through() {
    let state = state().await;
    link_custodial(&state, &actor(0x02)).await;
    invite(&state, &actor(0x02)).await;

    let app = Keypair::generate();
    let stranger = Keypair::generate().public_key_hex();
    let event = request_event(&app, &stranger, &rpc("x", "ping", &[]));
    // Validly signed, but the p-tag names no registered signer: the carve-out
    // must decline (None) so the generic auth-required/restricted path answers
    // — indistinguishable from any other non-local event.
    assert!(handle_bunker_request(&state, &event).await.is_none());

    // A 24133 with no p tag at all also falls through.
    let unsigned = UnsignedEvent {
        pubkey: app.public_key_bytes(),
        created_at: now_secs(),
        kind: 24133,
        tags: vec![],
        content: "junk".into(),
    };
    let no_p = app.sign_event(unsigned);
    assert!(handle_bunker_request(&state, &no_p).await.is_none());
}

#[tokio::test]
async fn connect_then_sign_event_round_trip_over_relay_tx() {
    let state = state().await;
    let user_kp = link_custodial(&state, &actor(0x03)).await;
    let inv = invite(&state, &actor(0x03)).await;

    let mut live_rx = state.nostr.relay_tx.subscribe();
    let app = connect_app(&state, &inv).await;

    // The connect produced a broadcast request (ephemeral semantics) and a
    // broadcast response; drain until we find the signer-authored response.
    let mut connect_response = None;
    while let Ok(ev) = live_rx.try_recv() {
        let parsed: Event = serde_json::from_str(&ev.event_json).unwrap();
        if parsed.pubkey == inv.signer_pubkey {
            connect_response = Some(parsed);
        }
    }
    let connect_response = connect_response.expect("connect response must ride relay_tx");
    assert_eq!(connect_response.kind, 24133);
    assert!(verify_event(&connect_response), "response must verify");
    assert_eq!(
        open_response(&app, &inv.signer_pubkey, &connect_response),
        build_response_json("c1", Ok("ack")),
    );

    // sign_event through the carve-out.
    let unsigned_json =
        r#"{"kind":1,"content":"hello via bunker","tags":[],"created_at":1700000000}"#;
    let req = request_event(
        &app,
        &inv.signer_pubkey,
        &rpc("s1", "sign_event", &[unsigned_json]),
    );
    let ok = drive(&state, &req).await;
    assert!(matches!(ok, RelayMessage::Ok { accepted: true, .. }));

    let mut sign_response = None;
    while let Ok(ev) = live_rx.try_recv() {
        let parsed: Event = serde_json::from_str(&ev.event_json).unwrap();
        if parsed.pubkey == inv.signer_pubkey {
            sign_response = Some(parsed);
        }
    }
    let sign_response = sign_response.expect("sign_event response must ride relay_tx");
    let plaintext = open_response(&app, &inv.signer_pubkey, &sign_response);
    let v: serde_json::Value = serde_json::from_str(&plaintext).unwrap();
    assert_eq!(v["id"], "s1");
    let signed: Event = serde_json::from_str(v["result"].as_str().unwrap()).unwrap();
    assert_eq!(signed.kind, 1);
    assert_eq!(signed.content, "hello via bunker");
    // Signed under the USER key — not the signer key.
    assert_eq!(signed.pubkey, user_kp.public_key_hex());
    assert!(verify_event(&signed));

    // The p-tag of the response names the app.
    let p = sign_response
        .tags
        .iter()
        .find(|t| t.name() == Some("p"))
        .unwrap();
    assert_eq!(p.value(), Some(app.public_key_hex().as_str()));
}

#[tokio::test]
async fn revoke_refuses_next_request() {
    let state = state().await;
    link_custodial(&state, &actor(0x04)).await;
    let inv = invite(&state, &actor(0x04)).await;
    let app = connect_app(&state, &inv).await;

    // Revoke via the S2 core (what fauna.nostr.bunker.revoke calls).
    let conn = state.db.conn().await;
    let id = bunker::list_apps(&conn, &actor(0x04)).unwrap()[0].id;
    assert!(bunker::revoke_app(&conn, &actor(0x04), id).unwrap());
    drop(conn);

    let req = request_event(&app, &inv.signer_pubkey, &rpc("p1", "ping", &[]));
    let ok = drive(&state, &req).await;
    // The request event itself is accepted transport-wise; the NIP-46 error
    // rides the encrypted response. Assert the response says unauthorized.
    assert!(matches!(ok, RelayMessage::Ok { accepted: true, .. }));
    let mut live_rx = state.nostr.relay_tx.subscribe();
    // Re-drive with a live subscription to capture the response.
    let req2 = request_event(&app, &inv.signer_pubkey, &rpc("p2", "ping", &[]));
    drive(&state, &req2).await;
    let mut response = None;
    while let Ok(ev) = live_rx.try_recv() {
        let parsed: Event = serde_json::from_str(&ev.event_json).unwrap();
        if parsed.pubkey == inv.signer_pubkey {
            response = Some(parsed);
        }
    }
    let response = response.expect("error response must still ride relay_tx");
    assert_eq!(
        open_response(&app, &inv.signer_pubkey, &response),
        build_response_json("p2", Err("unauthorized")),
    );
}

#[tokio::test]
async fn per_signer_rate_limit_binds() {
    let state = state().await;
    link_custodial(&state, &actor(0x05)).await;
    let inv = invite(&state, &actor(0x05)).await;
    let app = connect_app(&state, &inv).await;

    let mut limited = false;
    for i in 0..(bunker::BUNKER_REQS_PER_MINUTE + 2) {
        let req = request_event(
            &app,
            &inv.signer_pubkey,
            &rpc(&format!("r{i}"), "ping", &[]),
        );
        let ok = drive(&state, &req).await;
        if let RelayMessage::Ok {
            accepted: false,
            message,
            ..
        } = &ok
        {
            assert!(message.starts_with("rate-limited:"), "{message}");
            limited = true;
            break;
        }
    }
    assert!(
        limited,
        "the per-signer limiter must bind within the budget"
    );
}

#[tokio::test]
async fn bunker_state_survives_restart() {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let state1 = AppState::for_test(db.clone());
    fauna_nest::test_support::seat_own_deployment_seed(&state1).await;
    let state1 = Arc::new(state1);
    link_custodial(&state1, &actor(0x06)).await;
    let inv = invite(&state1, &actor(0x06)).await;
    let app = connect_app(&state1, &inv).await;

    // "Restart": a fresh AppState over the same DB — but the nest identity
    // key must survive too, or every at-rest secret is unreadable. Reuse it.
    let state2 = Arc::new(AppState {
        nest_identity: state1.nest_identity.clone(),
        ..AppState::for_test(db)
    });
    let req = request_event(&app, &inv.signer_pubkey, &rpc("p1", "ping", &[]));
    let mut live_rx = state2.nostr.relay_tx.subscribe();
    let ok = drive(&state2, &req).await;
    assert!(matches!(ok, RelayMessage::Ok { accepted: true, .. }));
    let mut response = None;
    while let Ok(ev) = live_rx.try_recv() {
        let parsed: Event = serde_json::from_str(&ev.event_json).unwrap();
        if parsed.pubkey == inv.signer_pubkey {
            response = Some(parsed);
        }
    }
    let response = response.expect("post-restart response must ride relay_tx");
    assert_eq!(
        open_response(&app, &inv.signer_pubkey, &response),
        build_response_json("p1", Ok("pong")),
    );
}

/// The full-stack leg: a real bound listener, a real WebSocket upgrade, and
/// the live-REQ delivery path (fan-out filter matching) that every
/// handler-driven test above bypasses. The app side is the crate's own
/// `RelayClient` — send the EVENT, then collect both the transport `OK` and
/// the signer-authored response off the same socket (they interleave: the
/// carve-out broadcasts the response before the OK is written).
#[tokio::test]
async fn full_ws_end_to_end_connect_and_sign() {
    use fauna_bridge_nostr::nip01::ClientMessage;
    use fauna_bridge_nostr::relay_client::RelayClient;
    use fauna_bridge_nostr::types::Filter;

    async fn send_and_await_response(
        client: &mut RelayClient,
        event: Event,
        signer_pubkey: &str,
    ) -> (bool, Event) {
        client.send(&ClientMessage::Event(event)).await.unwrap();
        let mut ok_accepted = None;
        let mut response = None;
        while ok_accepted.is_none() || response.is_none() {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(5), client.recv())
                .await
                .expect("relay reply within 5s")
                .expect("recv ok")
                .expect("socket open");
            match msg {
                RelayMessage::Ok { accepted, .. } => ok_accepted = Some(accepted),
                RelayMessage::Event { event, .. } if event.pubkey == signer_pubkey => {
                    response = Some(event)
                }
                _ => {} // AUTH challenge, EOSE, the app's own broadcast request
            }
        }
        (ok_accepted.unwrap(), response.unwrap())
    }

    let state = state().await;
    let user_kp = link_custodial(&state, &actor(0x07)).await;
    let inv = invite(&state, &actor(0x07)).await;

    let router = axum::Router::new()
        .route(
            "/nostr",
            axum::routing::get(fauna_nest::nostr::relay_endpoint::ws_handler),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let app = Keypair::generate();
    let mut client = RelayClient::connect(
        &format!("ws://{addr}/nostr"),
        // The relay under test is this in-process nest on loopback.
        fauna_bridge_nostr::relay_client::RelayDialPolicy::PublicOrLoopback,
    )
    .await
    .expect("ws connect");

    // The app's live subscription for signer responses — standard NIP-46.
    client
        .subscribe(
            "nip46",
            vec![Filter {
                kinds: Some(vec![24133]),
                authors: Some(vec![inv.signer_pubkey.clone()]),
                ..Default::default()
            }],
        )
        .await
        .unwrap();

    // connect
    let ev = request_event(
        &app,
        &inv.signer_pubkey,
        &rpc("c1", "connect", &[&inv.signer_pubkey, &inv.secret]),
    );
    let (accepted, response) = send_and_await_response(&mut client, ev, &inv.signer_pubkey).await;
    assert!(accepted, "connect EVENT must be OK true");
    assert_eq!(
        open_response(&app, &inv.signer_pubkey, &response),
        build_response_json("c1", Ok("ack")),
    );

    // sign_event
    let unsigned_json = r#"{"kind":1,"content":"signed over a real socket","tags":[]}"#;
    let ev = request_event(
        &app,
        &inv.signer_pubkey,
        &rpc("s1", "sign_event", &[unsigned_json]),
    );
    let (accepted, response) = send_and_await_response(&mut client, ev, &inv.signer_pubkey).await;
    assert!(accepted);
    let plaintext = open_response(&app, &inv.signer_pubkey, &response);
    let v: serde_json::Value = serde_json::from_str(&plaintext).unwrap();
    assert_eq!(v["id"], "s1");
    let signed: Event = serde_json::from_str(v["result"].as_str().unwrap()).unwrap();
    assert_eq!(signed.pubkey, user_kp.public_key_hex());
    assert_eq!(signed.content, "signed over a real socket");
    assert!(verify_event(&signed));
}

#[tokio::test]
async fn nip04_legacy_scheme_answered_in_kind() {
    let state = state().await;
    link_custodial(&state, &actor(0x08)).await;
    let inv = invite(&state, &actor(0x08)).await;

    // Connect with a NIP-04-encrypted request (legacy client).
    let app = Keypair::generate();
    let mut signer_pk = [0u8; 32];
    signer_pk.copy_from_slice(&hex::decode(&inv.signer_pubkey).unwrap());
    let content = fauna_bridge_nostr::nip04::encrypt(
        &app,
        &signer_pk,
        &rpc("c1", "connect", &[&inv.signer_pubkey, &inv.secret]),
    )
    .unwrap();
    let unsigned = UnsignedEvent {
        pubkey: app.public_key_bytes(),
        created_at: now_secs(),
        kind: 24133,
        tags: vec![Tag::new(vec!["p".into(), inv.signer_pubkey.clone()])],
        content,
    };
    let event = app.sign_event(unsigned);

    let mut live_rx = state.nostr.relay_tx.subscribe();
    let ok = drive(&state, &event).await;
    assert!(matches!(ok, RelayMessage::Ok { accepted: true, .. }));

    let mut response = None;
    while let Ok(ev) = live_rx.try_recv() {
        let parsed: Event = serde_json::from_str(&ev.event_json).unwrap();
        if parsed.pubkey == inv.signer_pubkey {
            response = Some(parsed);
        }
    }
    let response = response.expect("NIP-04 response must ride relay_tx");
    // The response is answered in the request's scheme: NIP-04's `?iv=`.
    assert_eq!(
        fauna_bridge_nostr::nip46::detect_scheme(&response.content),
        EncryptionScheme::Nip04
    );
    let plaintext =
        fauna_bridge_nostr::nip04::decrypt(&app, &signer_pk, &response.content).unwrap();
    assert_eq!(plaintext, build_response_json("c1", Ok("ack")));
}

/// Drive one request through the carve-out and return the signer's
/// decrypted response JSON.
async fn exchange(
    state: &Arc<AppState>,
    app: &Keypair,
    signer_pubkey: &str,
    request_json: &str,
) -> String {
    let mut live_rx = state.nostr.relay_tx.subscribe();
    let req = request_event(app, signer_pubkey, request_json);
    let ok = drive(state, &req).await;
    assert!(
        matches!(ok, RelayMessage::Ok { accepted: true, .. }),
        "request not accepted: {ok:?}"
    );
    let mut response = None;
    while let Ok(ev) = live_rx.try_recv() {
        let parsed: Event = serde_json::from_str(&ev.event_json).unwrap();
        if parsed.pubkey == signer_pubkey {
            response = Some(parsed);
        }
    }
    let response = response.expect("the signer's response must ride relay_tx");
    open_response(app, signer_pubkey, &response)
}

/// `connect` (the reconnect ack), `ping` and `sign_event` from a connected
/// roster app are all served, or all `unauthorized`.
async fn expect_roster(
    state: &Arc<AppState>,
    app: &Keypair,
    signer: &str,
    served: bool,
    why: &str,
) {
    let unsigned = r#"{"kind":1,"content":"roster","tags":[],"created_at":1700000000}"#;
    let ack = exchange(state, app, signer, &rpc("c", "connect", &[signer])).await;
    let ping = exchange(state, app, signer, &rpc("p", "ping", &[])).await;
    let sign = exchange(state, app, signer, &rpc("s", "sign_event", &[unsigned])).await;
    if served {
        assert_eq!(ack, build_response_json("c", Ok("ack")), "{why}: connect");
        assert_eq!(ping, build_response_json("p", Ok("pong")), "{why}: ping");
        let v: serde_json::Value = serde_json::from_str(&sign).unwrap();
        assert!(v["result"].is_string(), "{why}: sign_event: {sign}");
    } else {
        let refused = |id: &str| build_response_json(id, Err("unauthorized"));
        assert_eq!(ack, refused("c"), "{why}: connect");
        assert_eq!(ping, refused("p"), "{why}: ping");
        assert_eq!(sign, refused("s"), "{why}: sign_event");
    }
}

/// The account's authority reaches its own invite-roster apps: while the
/// account is locked out or suspended the signer answers a connected app's
/// `connect`, `ping` and `sign_event` — and a fresh invite secret's `connect`
/// — with the uniform `unauthorized`; the lock's lapse and the admin's
/// restore serve the same app, and the same still-pending secret, again with
/// nothing redone. The external-apps switch is never read for a roster app:
/// these are the user's own (`nostr.md` § The nest as the user's NIP-46
/// signer → *Connection model*).
#[tokio::test]
async fn a_suspended_or_locked_out_accounts_roster_apps_are_not_served() {
    let state = state().await;
    let account = [0x5A; 32];
    let actor_hex = actor(0x5A);
    link_custodial(&state, &actor_hex).await;
    let inv = invite(&state, &actor_hex).await;
    let app = connect_app(&state, &inv).await;
    let signer = inv.signer_pubkey.as_str();

    expect_roster(&state, &app, signer, true, "standing").await;

    // The external-apps switch pauses principals, never the user's own apps.
    state
        .db
        .set_atproto_external_apps_enabled(&account, false)
        .await
        .unwrap();
    expect_roster(&state, &app, signer, true, "switch OFF").await;

    // Locked out: nothing is served; a lapsed lock is no lock.
    let now = now_secs() as i64;
    state
        .db
        .set_locked_until(&account, Some(now + 3600))
        .await
        .unwrap();
    expect_roster(&state, &app, signer, false, "locked out").await;
    state
        .db
        .set_locked_until(&account, Some(now - 1))
        .await
        .unwrap();
    expect_roster(&state, &app, signer, true, "lock lapsed").await;

    // Suspended: nothing is served, a fresh secret's connect included — and
    // the secret stays pending, so the restore serves it with nothing redone.
    assert!(
        state
            .db
            .suspend_user_now(&account, "test", "test")
            .await
            .unwrap()
    );
    expect_roster(&state, &app, signer, false, "suspended").await;
    let pending = invite(&state, &actor_hex).await;
    let newcomer = Keypair::generate();
    let connect = rpc("n", "connect", &[signer, &pending.secret]);
    assert_eq!(
        exchange(&state, &newcomer, signer, &connect).await,
        build_response_json("n", Err("unauthorized")),
        "suspended: a pending secret's connect"
    );
    assert!(state.db.cancel_eviction(&account).await.unwrap());
    expect_roster(&state, &app, signer, true, "restored").await;
    assert_eq!(
        exchange(&state, &newcomer, signer, &connect).await,
        build_response_json("n", Ok("ack")),
        "restored: the same pending secret connects"
    );
}
