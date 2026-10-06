#![cfg(feature = "nostr")]
//! N2 — real-client relay conformance harness.
//!
//! Every in-repo relay test so far drives the endpoint with our *own*
//! `fauna_bridge_nostr` types — which share every wire assumption with the
//! relay. This suite instead drives a bound `/nostr` listener with
//! **`nostr-sdk`** (the rust-nostr client real apps embed), so a divergence
//! between what the relay speaks and what a real Nostr client expects fails
//! here instead of in a user's Damus/Amethyst session:
//!
//!  * NIP-11: the info document is fetched the way real clients fetch it —
//!    `Accept: application/nostr+json` against the *WebSocket URI* (NIP-11's
//!    "same URI" rule), not our internal `/nostr/info` convenience route;
//!  * NIP-42: AUTH with the relay URL exactly as the client normalized it
//!    (scheme `ws://`, explicit port — nothing like the hand-built
//!    `wss://<domain>/nostr` strings the unit tests use);
//!  * authed EVENT publish, REQ replay, and live-subscription delivery;
//!  * unauthed NIP-59 gift-wrap deposit + recipient-gated readback;
//!  * NIP-50 search; NIP-09 kind-5 delete.
//!
//! The dep is a dev-dependency only, and its **source is never read** — the
//! project's dep-source rule; scanners vetted the tree (2026-07-20).
//! Only compiled under `--features nostr`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use fauna_bridge_nostr::signing::Keypair;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::{self, db};
use fauna_nest::routes::AppState;

/// A relay bound on an ephemeral port, its `config.nest.domain` set to the
/// bound `host:port` (production sets the deployment domain the same way; the
/// NIP-42 relay-tag check needs to know the host clients dial).
struct BoundRelay {
    state: Arc<AppState>,
    url: String,
}

async fn spawn_relay() -> BoundRelay {
    spawn_relay_with_heartbeat(fauna_nest::ws::WsHeartbeatPolicy::default()).await
}

async fn spawn_relay_with_heartbeat(heartbeat: fauna_nest::ws::WsHeartbeatPolicy) -> BoundRelay {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let base = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&base).await;
    let mut config = (*base.config).clone();
    config.nest.domain = Some(format!("{addr}"));
    let state = Arc::new(AppState {
        config: Arc::new(config),
        ws: Arc::new(fauna_nest::ws::WsState::with_heartbeat(heartbeat)),
        ..base
    });

    let router = nostr::routes().with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    BoundRelay {
        state,
        url: format!("ws://{addr}/nostr"),
    }
}

/// Link a custodial account (deposited nsec encrypted under the state's own
/// nest key) and return the user's Nostr keypair (our shared type — the test
/// hands its secret to `nostr-sdk` as hex, exactly like a user importing a key
/// into a real client).
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

/// NIP-11: a real client resolves the info document from the WebSocket URI
/// itself with `Accept: application/nostr+json` (NIP-11's same-URI rule).
/// `/nostr/info` existing as well is fine; this fetch is the conformance
/// surface.
#[tokio::test]
async fn nip11_served_on_the_websocket_uri() {
    let relay = spawn_relay().await;
    link_custodial(&relay.state, "actor-nip11").await;

    let http_url = relay.url.replacen("ws://", "http://", 1);
    let resp = reqwest::Client::new()
        .get(&http_url)
        .header("Accept", "application/nostr+json")
        .send()
        .await
        .expect("NIP-11 fetch");
    assert_eq!(
        resp.status(),
        200,
        "GET <ws-uri> with Accept: application/nostr+json must serve NIP-11"
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.starts_with("application/nostr+json"),
        "NIP-11 content-type, got {ct:?}"
    );
    let info: serde_json::Value = resp.json().await.unwrap();
    let nips = info["supported_nips"].as_array().expect("supported_nips");
    for nip in [1, 9, 11, 17, 40, 42, 45, 50, 59] {
        assert!(
            nips.iter().any(|n| n == nip),
            "supported_nips must list {nip}: {nips:?}"
        );
    }
    assert_eq!(info["limitation"]["auth_required"], false);
    assert_eq!(info["limitation"]["restricted_writes"], true);
}

/// **The peer-population proof for the server heartbeat on `/nostr`**
/// (`transport.md` § Connection lifecycle).
///
/// Every other WS surface on this nest faces software we ship, so "the peer
/// answers a Ping" is ours to know. `/nostr` faces arbitrary third-party
/// clients, and RFC 6455's *"the Pong is mandatory"* is exactly the kind of
/// spec-says-so reasoning that produced the UA-less ActivityPub interop outage.
/// So the relay does not assume it — this test establishes it, against the same
/// real `nostr-sdk` client real apps embed.
///
/// The shape matters: the client is left **completely idle** across several
/// compressed liveness windows. It sends no REQ, no EVENT, nothing — the exact
/// case a bare inbound-idle timeout would kill, and the case a real subscriber
/// sits in for hours waiting on live events. Only its WebSocket stack's
/// below-application Pong keeps it alive.
///
/// The assertion is the client's `RelayStatus`, not "the connection still
/// works": a real client **reconnects and resubscribes automatically**, so a
/// functional check alone passes even when the relay reaped the session — the
/// failure would be invisible here and show up in production as sessions that
/// churn every liveness window. `attempts()` is *also* wrong on its own and was
/// tried first: the reconnect is backed off, so a reaped client still reads
/// `attempts() == 1` seconds later while sitting `Disconnected`. Both are
/// asserted, but the status is the one with teeth — verified by canary (ping
/// interval pushed past the liveness window ⇒ this test fails).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_nostr_client_survives_many_idle_liveness_windows() {
    use nostr_sdk::prelude::*;

    // Compressed cadence; the *ratio* mirrors production (timeout = 2× interval).
    let relay = spawn_relay_with_heartbeat(fauna_nest::ws::WsHeartbeatPolicy {
        ping_interval: Duration::from_millis(200),
        liveness_timeout: Duration::from_millis(400),
    })
    .await;
    let user_kp = link_custodial(&relay.state, "actor-heartbeat").await;
    let keys = Keys::parse(&hex::encode(user_kp.secret_bytes())).unwrap();

    let client = Client::builder()
        .authenticator(SignerAuthenticator::new(keys.clone()))
        .build();
    client.add_relay(&relay.url).await.unwrap();
    client.connect().and_wait(Duration::from_secs(10)).await;

    let handle = client
        .relay(&relay.url)
        .await
        .expect("relay handle")
        .expect("relay is in the pool");
    let attempts_before = handle.stats().attempts();
    assert_eq!(
        handle.status(),
        RelayStatus::Connected,
        "precondition: the client must be connected before the idle spell"
    );

    // Idle for many windows. This is not a settle-sleep standing in for a state
    // assertion (convention 14) — elapsed idle time *is* the condition under
    // test, and the `# sleep-ok` equivalent applies: the whole question is what
    // the relay does to a connection that stays quiet.
    tokio::time::sleep(Duration::from_secs(3)).await; // ≈7 liveness windows

    assert_eq!(
        handle.status(),
        RelayStatus::Connected,
        "the relay reaped a real nostr client that was merely idle (it is now \
         {:?} after ~7 liveness windows). Either the client's WebSocket stack does not \
         answer a server Ping — in which case `/nostr` must ping-but-never-reap, and \
         `transport.md` § Connection lifecycle must say so — or the heartbeat is mis-wired.",
        handle.status()
    );
    assert_eq!(
        handle.stats().attempts(),
        attempts_before,
        "the connection must be the original one, never a silent reconnect papering over \
         a reap"
    );

    // And it is still a working session, not just an unbroken socket.
    let event = EventBuilder::new(Kind::TextNote, "still here after all those idle windows")
        .finalize(&keys)
        .unwrap();
    let out = tokio::time::timeout(Duration::from_secs(10), client.send_event(&event))
        .await
        .expect("publish within 10s")
        .expect("EVENT accepted on the long-idle connection");
    assert!(
        !out.success.is_empty(),
        "an idle-but-responsive client must still be able to publish (failed: {:?})",
        out.failed
    );

    client.disconnect().await;
}

/// The matched negative of the test above: a relay client that answers *nothing*
/// is reaped, and its socket released.
///
/// This one deliberately does **not** use `nostr-sdk`. A real client reconnects
/// on its own, so it cannot express "vanished without a clean TCP close" — the
/// suspended VM, the dropped link, the killed process — and its reconnect would
/// paper over the very teardown under test. A raw socket that completes the
/// upgrade and is then never read from is that peer exactly, and EOF on the TCP
/// connection is the teardown observed directly.
#[tokio::test]
async fn a_nostr_client_that_never_answers_a_ping_is_reaped() {
    use tokio::io::AsyncReadExt;

    let relay = spawn_relay_with_heartbeat(fauna_nest::ws::WsHeartbeatPolicy {
        ping_interval: Duration::from_millis(200),
        liveness_timeout: Duration::from_millis(400),
    })
    .await;
    // The relay surface is gated on a deposited key; without one every upgrade
    // is refused 503 and there is no connection to reap.
    link_custodial(&relay.state, "actor-reap").await;

    let (mut ws, _) = tokio_tungstenite::connect_async(&relay.url)
        .await
        .expect("relay upgrade should succeed");
    // From here the WebSocket is never polled: no Pong will ever be sent.
    let raw = ws.get_mut();

    let mut buf = [0u8; 256];
    let eof = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match raw.read(&mut buf).await {
                Ok(0) => return true, // the relay closed the dead link
                Ok(_) => continue,    // the AUTH challenge, then Pings we ignore
                Err(_) => return true,
            }
        }
    })
    .await;

    assert!(
        eof.is_ok(),
        "the relay never released the socket of a client that answered no Ping for many \
         liveness windows"
    );
}

/// NIP-42 + NIP-01: a real client holding the user's key authenticates with
/// the relay URL *as it normalized it* and publishes; an anonymous real client
/// reads the note back (public reads) via REQ replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_client_auths_publishes_and_anon_reads_back() {
    use nostr_sdk::prelude::*;

    let relay = spawn_relay().await;
    let user_kp = link_custodial(&relay.state, "actor-auth").await;

    let keys = Keys::parse(&hex::encode(user_kp.secret_bytes())).unwrap();
    let client = Client::builder()
        .authenticator(SignerAuthenticator::new(keys.clone()))
        .build();
    client.add_relay(&relay.url).await.unwrap();
    client.connect().await;

    let event = EventBuilder::new(Kind::TextNote, "a real client wrote this")
        .finalize(&keys)
        .unwrap();
    let out = tokio::time::timeout(Duration::from_secs(10), client.send_event(&event))
        .await
        .expect("publish within 10s")
        .expect("EVENT accepted after NIP-42 auth");
    assert!(
        !out.success.is_empty(),
        "the relay must accept the authed EVENT (failed: {:?})",
        out.failed
    );

    // Anonymous readback — public reads need no auth (the outbox-relay point).
    // `Client::connect()` returns without waiting for the socket, so a lone
    // immediate fetch can race the connection — retry within a bound, like a
    // real client's refresh loop.
    let anon = Client::default();
    anon.add_relay(&relay.url).await.unwrap();
    anon.connect().await;
    let filter = Filter::new()
        .kinds([Kind::TextNote])
        .authors([keys.public_key()]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let events = loop {
        let events = anon
            .fetch_events(filter.clone())
            .timeout(Duration::from_secs(5))
            .await
            .expect("anon REQ replay");
        if !events.is_empty() || tokio::time::Instant::now() > deadline {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(events.len(), 1, "anon reader must see the public note");
    assert_eq!(events.first().unwrap().content, "a real client wrote this");

    client.disconnect().await;
    anon.disconnect().await;
}

/// NIP-59/NIP-17: an external real client deposits a gift wrap for a local
/// depositor **unauthenticated**; readback is recipient-gated — the recipient's
/// real client is told `auth-required:` (NIP-42's documented CLOSED flow),
/// auto-authenticates, and gets the wrap; an anonymous reader gets nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gift_wrap_deposit_and_recipient_gated_readback() {
    use nostr_sdk::prelude::*;

    let relay = spawn_relay().await;
    // A real 32-byte-hex actor id (not the friendly "actor-*" strings the
    // other tests in this file use) — `process_gift_wrap_inbound`'s seal seam
    // decodes `actor_id` via `fauna_core::hex32::decode` before it can reach
    // the seal-key check, so a non-hex id fails closed there silently.
    let actor_hex = hex::encode([0xa9u8; 32]);
    let user_kp = link_custodial(&relay.state, &actor_hex).await;
    let user_keys = Keys::parse(&hex::encode(user_kp.secret_bytes())).unwrap();
    let user_pk = user_keys.public_key();

    // A real onboarded custodial account also has a D2/MLS seal key on file
    // (provisioned at onboarding); without one the S8.9 seam fails closed and
    // silently drops the DM by design (`inbound_gift_wrap_without_seal_key_stores_nothing`,
    // `sync_worker.rs`) — a realistic harness must seed one to exercise the
    // seal-succeeds path a real client's wrap actually takes.
    let msek = [0x42u8; 32];
    common::seed_recipient_seal_key(&relay.state.db, &[0xa9u8; 32], &msek).await;

    // An external sender — NOT a local account, never authenticates.
    let sender_keys = Keys::generate();
    let sender = Client::builder()
        .authenticator(SignerAuthenticator::new(sender_keys.clone()))
        .build();
    sender.add_relay(&relay.url).await.unwrap();
    sender.connect().await;

    // NIP-17 requires the wrapped rumor itself be kind 14 (private DM) with a
    // `p` tag naming the recipient — `nip17::unwrap_gift_wrap` rejects any
    // other rumor kind, so a plain kind-1 rumor (the pre-existing shape here)
    // never reaches the seal seam at all.
    let rumor = EventBuilder::new(Kind::Custom(14), "sealed for your eyes")
        .tag(Tag::public_key(user_pk))
        .finalize_unsigned(sender_keys.public_key());
    let wrap = GiftWrapBuilder::new(user_pk, rumor)
        .finalize_async(&sender_keys)
        .await
        .expect("build gift wrap");
    let out = tokio::time::timeout(Duration::from_secs(10), sender.send_event(&wrap))
        .await
        .expect("deposit within 10s")
        .expect("unauthed gift-wrap deposit accepted");
    assert!(
        !out.success.is_empty(),
        "the relay must accept the unauthed 1059 deposit (failed: {:?})",
        out.failed
    );

    let wrap_filter = Filter::new().kinds([Kind::GiftWrap]).pubkey(user_pk);

    // Anonymous reader: nothing — and no way to tell a gated wrap from none.
    let anon = Client::default();
    anon.add_relay(&relay.url).await.unwrap();
    anon.connect().await;
    let events = anon
        .fetch_events(wrap_filter.clone())
        .timeout(Duration::from_secs(5))
        .await
        .expect("anon fetch");
    assert_eq!(events.len(), 0, "gift wraps must never serve anonymously");

    // The same leak probed PAST the `CLOSED auth-required:` trigger: a
    // kindless filter never names kind 1059, so it reaches the store's
    // serving gate itself — the wrap must still not surface (the canary run
    // proved the explicit-kind assert above alone is shadowed by the CLOSED
    // signal and misses a broken serving gate).
    let kindless = Filter::new().pubkey(user_pk);
    let events = anon
        .fetch_events(kindless)
        .timeout(Duration::from_secs(5))
        .await
        .expect("anon kindless fetch");
    assert!(
        events.iter().all(|e| e.kind != Kind::GiftWrap),
        "a kindless anon REQ must never surface a gift wrap"
    );

    // The recipient's client: relay says auth-required, the client
    // auto-authenticates (NIP-42) and the wrap serves.
    let recipient = Client::builder()
        .authenticator(SignerAuthenticator::new(user_keys.clone()))
        .build();
    recipient.add_relay(&relay.url).await.unwrap();
    recipient.connect().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let events = loop {
        let events = recipient
            .fetch_events(wrap_filter.clone())
            .timeout(Duration::from_secs(5))
            .await
            .expect("recipient fetch");
        if !events.is_empty() || tokio::time::Instant::now() > deadline {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(
        events.len(),
        1,
        "the NIP-42-authed recipient must receive the wrap"
    );
    assert_eq!(events.first().unwrap().kind, Kind::GiftWrap);

    // The seal-seam leg: `handle_gift_wrap_inbox` awaits the shared S8.9 seam
    // (`sync_worker::process_gift_wrap_inbound`) before acking the deposit
    // (`relay_endpoint.rs`), so by the time `send_event` above returned, a
    // REAL nostr-sdk-built wrap — not just our own `fauna_bridge_nostr`
    // fixtures — must already have been deposited into the bridged
    // conversation family through the Nostr leg. Read back the way the user's
    // app reads it: `fauna.bridges.conversation.inbox.fetch` (the family's
    // user kind). The probe checks the SERVED bytes (the vacuous-green
    // trap: they must exist, must not embed the plaintext, and must open to
    // it) — matching `inbound_gift_wrap_dm_seals_at_rest`'s rigor.
    let actor = [0xa9u8; 32];
    let mut b = fauna_nest::rpc_router::RpcRouter::builder();
    fauna_nest::bridged_conversation_handlers::register_bridged_conversation_handlers(&mut b);
    let router = b.build();
    let reply = common::dispatch(
        &router,
        relay.state.clone(),
        actor,
        fauna_protocol::bridged_conversations::KIND_INBOX_FETCH,
        common::encode(&fauna_protocol::bridged_conversations::InboxFetchRequest::default()),
    )
    .await
    .expect("fauna.bridges.conversation.inbox.fetch");
    let inbox: fauna_protocol::bridged_conversations::InboxFetchReply =
        fauna_protocol::decode_strict(&reply).unwrap();
    assert_eq!(
        inbox.messages.len(),
        1,
        "a real-client gift wrap must reach the bridged family, not just the deposit"
    );
    let row = &inbox.messages[0];
    assert_eq!(
        (row.bridge_id.as_str(), row.outbound, row.sender.as_str()),
        ("nostr", false, sender_keys.public_key().to_hex().as_str()),
        "an inbound row on the Nostr leg, sent by the seal's authenticated key"
    );
    let sealed_bytes = row.sealed_content.clone();
    assert!(
        !sealed_bytes
            .windows("sealed for your eyes".len())
            .any(|w| w == b"sealed for your eyes"),
        "the stored sealed bytes must not embed the plaintext"
    );
    let opened = common::open_recipient_record(&sealed_bytes, &msek);
    assert_eq!(opened, b"sealed for your eyes");

    sender.disconnect().await;
    anon.disconnect().await;
    recipient.disconnect().await;
}

/// NIP-42's on-demand flow, pinned at the raw wire: a client that does NOT
/// eagerly auth (nostr-sdk auths on challenge, so it can't exercise this) and
/// REQs kind-1059 unauthenticated must get `CLOSED` with the machine-readable
/// `auth-required:` prefix — not a silent empty EOSE it would render as "no
/// DMs". A kindless REQ stays a normal public read (no CLOSED).
#[tokio::test]
async fn unauthed_gift_wrap_req_gets_closed_auth_required() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let relay = spawn_relay().await;
    link_custodial(&relay.state, "actor-ondemand").await;

    let (mut ws, _) = tokio_tungstenite::connect_async(&relay.url).await.unwrap();

    async fn next_relevant(
        ws: &mut (
                 impl StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin
             ),
    ) -> serde_json::Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("relay reply within 5s")
                .expect("stream open")
                .expect("ws ok");
            let v: serde_json::Value =
                serde_json::from_str(msg.into_text().unwrap().as_str()).unwrap();
            if v[0] != "AUTH" {
                return v;
            }
        }
    }

    // Explicit kind-1059 REQ → CLOSED auth-required:
    ws.send(WsMessage::Text(
        r#"["REQ","dm-inbox",{"kinds":[1059]}]"#.into(),
    ))
    .await
    .unwrap();
    let reply = next_relevant(&mut ws).await;
    assert_eq!(reply[0], "CLOSED", "got {reply}");
    assert_eq!(reply[1], "dm-inbox");
    assert!(
        reply[2].as_str().unwrap().starts_with("auth-required:"),
        "machine-readable prefix, got {reply}"
    );

    // Kindless REQ → normal public read: EOSE, no CLOSED.
    ws.send(WsMessage::Text(r#"["REQ","public",{}]"#.into()))
        .await
        .unwrap();
    loop {
        let reply = next_relevant(&mut ws).await;
        match reply[0].as_str().unwrap() {
            "EVENT" => continue,
            "EOSE" => break,
            other => panic!("kindless unauthed REQ must replay publicly, got {other}: {reply}"),
        }
    }

    // COUNT with kind 1059 → same CLOSED signal.
    ws.send(WsMessage::Text(
        r#"["COUNT","dm-count",{"kinds":[1059]}]"#.into(),
    ))
    .await
    .unwrap();
    let reply = next_relevant(&mut ws).await;
    assert_eq!(reply[0], "CLOSED", "got {reply}");
    assert!(
        reply[2].as_str().unwrap().starts_with("auth-required:"),
        "got {reply}"
    );
}

/// NIP-50: a real client's `search` filter serves from the relay-internal FTS.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nip50_search_from_a_real_client() {
    use nostr_sdk::prelude::*;

    let relay = spawn_relay().await;
    let user_kp = link_custodial(&relay.state, "actor-search").await;
    let keys = Keys::parse(&hex::encode(user_kp.secret_bytes())).unwrap();

    let client = Client::builder()
        .authenticator(SignerAuthenticator::new(keys.clone()))
        .build();
    client.add_relay(&relay.url).await.unwrap();
    client.connect().await;
    for text in ["the quick brown fox", "an unrelated note"] {
        let ev = EventBuilder::new(Kind::TextNote, text)
            .finalize(&keys)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), client.send_event(&ev))
            .await
            .expect("publish within 10s")
            .expect("EVENT accepted");
    }

    let anon = Client::default();
    anon.add_relay(&relay.url).await.unwrap();
    anon.connect().await;
    let filter = Filter::new().kinds([Kind::TextNote]).search("brown");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let events = loop {
        let events = anon
            .fetch_events(filter.clone())
            .timeout(Duration::from_secs(5))
            .await
            .expect("search fetch");
        if !events.is_empty() || tokio::time::Instant::now() > deadline {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(events.len(), 1, "search must match exactly the fox note");
    assert_eq!(events.first().unwrap().content, "the quick brown fox");

    client.disconnect().await;
    anon.disconnect().await;
}

/// NIP-09: a real client's kind-5 deletion removes the referenced note from
/// replay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kind5_delete_from_a_real_client() {
    use nostr_sdk::prelude::*;

    let relay = spawn_relay().await;
    let user_kp = link_custodial(&relay.state, "actor-del").await;
    let keys = Keys::parse(&hex::encode(user_kp.secret_bytes())).unwrap();

    let client = Client::builder()
        .authenticator(SignerAuthenticator::new(keys.clone()))
        .build();
    client.add_relay(&relay.url).await.unwrap();
    client.connect().await;
    let note = EventBuilder::new(Kind::TextNote, "delete me")
        .finalize(&keys)
        .unwrap();
    let note_id = note.id;
    tokio::time::timeout(Duration::from_secs(10), client.send_event(&note))
        .await
        .expect("publish within 10s")
        .expect("EVENT accepted");

    let delete = EventDeletionRequest::new()
        .ids([note_id])
        .finalize(&keys)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), client.send_event(&delete))
        .await
        .expect("delete within 10s")
        .expect("kind-5 accepted");

    let anon = Client::default();
    anon.add_relay(&relay.url).await.unwrap();
    anon.connect().await;
    let filter = Filter::new().kinds([Kind::TextNote]).id(note_id);
    // Poll until the connection is live (an empty result here is ambiguous
    // between "not connected yet" and "deleted", so give it the full window
    // and assert the *stable* outcome).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut last = None;
    while tokio::time::Instant::now() < deadline {
        let events = anon
            .fetch_events(filter.clone())
            .timeout(Duration::from_secs(5))
            .await
            .expect("fetch");
        last = Some(events);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        last.expect("at least one fetch").len(),
        0,
        "a deleted note must not replay"
    );

    client.disconnect().await;
    anon.disconnect().await;
}

/// S6 — the third-party NIP-46 interop leg (`nostr.md` § The nest as the
/// user's NIP-46 signer): rust-nostr's real NIP-46 client (`nostr-connect`,
/// what Amethyst-class apps embed via the SDK) consumes a minted
/// `bunker://…?relay=…&secret=…` invite over the live relay — connect with the
/// one-time secret, `get_public_key` returning the USER pubkey (not the
/// signer's), and `sign_event` under the deposited nsec. Every in-repo bunker
/// test drives our own nip46 types; this is the loop closed with a client we
/// don't control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nip46_third_party_client_connects_and_signs() {
    use nostr_connect::prelude::*;

    let relay = spawn_relay().await;
    let user_kp = link_custodial(&relay.state, "actor-nip46").await;

    // Mint the invite exactly as `fauna.nostr.bunker.create_invite` does.
    let inv = {
        let now = fauna_core::data::Timestamp::now_secs() as u64;
        let conn = relay.state.db.conn().await;
        fauna_nest::nostr::bunker::create_invite(&conn, "actor-nip46", now).unwrap()
    };
    let bunker_uri = format!(
        "bunker://{}?relay={}&secret={}",
        inv.signer_pubkey, relay.url, inv.secret
    );

    let uri = NostrConnectUri::parse(&bunker_uri).expect("parse bunker URI");
    let app_keys = Keys::generate();
    let signer =
        NostrConnect::new(uri, app_keys, Duration::from_secs(10), None).expect("nip46 client");

    let user_pk = tokio::time::timeout(Duration::from_secs(15), signer.get_public_key_async())
        .await
        .expect("get_public_key within 15s")
        .expect("get_public_key over the wire");
    assert_eq!(
        user_pk.to_hex(),
        user_kp.public_key_hex(),
        "get_public_key must return the USER pubkey, not the signer's"
    );
    assert_ne!(
        user_pk.to_hex(),
        inv.signer_pubkey,
        "signer identity stays distinct from the user identity"
    );

    let unsigned =
        EventBuilder::new(Kind::TextNote, "signed by my own nest").finalize_unsigned(user_pk);
    let signed = tokio::time::timeout(Duration::from_secs(15), signer.sign_event_async(unsigned))
        .await
        .expect("sign_event within 15s")
        .expect("sign_event over the wire");
    assert_eq!(signed.pubkey, user_pk);
    assert_eq!(signed.content, "signed by my own nest");
    signed.verify().expect("real-client-side signature verify");
}

/// NIP-44 cross-implementation pins, both directions. The original fauna
/// nip44 round-tripped against itself but diverged from the spec in three
/// ways (conversation-key derive, message-key derive + cipher, padding) and
/// rejected every real client's ciphertext — S6 caught it as a bunker
/// `get_public_key` timeout. These pins keep the two implementations honest
/// at the crypto layer, where a self-round-trip can never fail.
#[tokio::test]
async fn nip44_cross_impl_bidirectional() {
    use nostr_sdk::prelude::*;

    let a = Keys::generate();
    let b = Keys::generate();
    let mut a_pub = [0u8; 32];
    a_pub.copy_from_slice(&hex::decode(a.public_key().to_hex()).unwrap());
    let mut b_pub = [0u8; 32];
    b_pub.copy_from_slice(&hex::decode(b.public_key().to_hex()).unwrap());

    // Their encrypt → our decrypt.
    let theirs = nostr_nips::nips::nip44::encrypt(
        a.secret_key(),
        &b.public_key(),
        "their encrypt, our decrypt",
        nostr_nips::nips::nip44::Version::V2,
    )
    .unwrap();
    let opened = fauna_bridge_nostr::nip44::nip44_decrypt(
        &b.secret_key().to_secret_bytes(),
        &a_pub,
        &theirs,
    )
    .expect("our nip44 must open a real client's ciphertext");
    assert_eq!(opened, "their encrypt, our decrypt");

    // Our encrypt → their decrypt.
    let ours = fauna_bridge_nostr::nip44::nip44_encrypt(
        &a.secret_key().to_secret_bytes(),
        &b_pub,
        "our encrypt, their decrypt",
    )
    .unwrap();
    let opened = nostr_nips::nips::nip44::decrypt(b.secret_key(), &a.public_key(), &ours)
        .expect("a real client must open our ciphertext");
    assert_eq!(opened, "our encrypt, their decrypt");
}

/// NIP-04 cross-implementation pins, both directions — the bunker answers
/// legacy clients in NIP-04, so this leg needs the same honesty check.
#[tokio::test]
async fn nip04_cross_impl_bidirectional() {
    use nostr_sdk::prelude::*;

    let a = Keys::generate();
    let b = Keys::generate();
    let a_kp =
        fauna_bridge_nostr::signing::Keypair::from_secret_bytes(a.secret_key().to_secret_bytes())
            .unwrap();
    let b_kp =
        fauna_bridge_nostr::signing::Keypair::from_secret_bytes(b.secret_key().to_secret_bytes())
            .unwrap();
    let mut a_pub = [0u8; 32];
    a_pub.copy_from_slice(&hex::decode(a.public_key().to_hex()).unwrap());
    let mut b_pub = [0u8; 32];
    b_pub.copy_from_slice(&hex::decode(b.public_key().to_hex()).unwrap());

    // Their encrypt → our decrypt.
    let theirs = nostr_nips::nips::nip04::encrypt(
        a.secret_key(),
        &b.public_key(),
        "their nip04, our decrypt",
    )
    .unwrap();
    let opened = fauna_bridge_nostr::nip04::decrypt(&b_kp, &a_pub, &theirs)
        .expect("our nip04 must open a real client's ciphertext");
    assert_eq!(opened, "their nip04, our decrypt");

    // Our encrypt → their decrypt.
    let ours =
        fauna_bridge_nostr::nip04::encrypt(&a_kp, &b_pub, "our nip04, their decrypt").unwrap();
    let opened = nostr_nips::nips::nip04::decrypt(b.secret_key(), &a.public_key(), &ours)
        .expect("a real client must open our nip04 ciphertext");
    assert_eq!(opened, "our nip04, their decrypt");
}

/// Live-subscription delivery: an anonymous subscriber holds a REQ open; a
/// note published afterwards arrives as a live event (not just in replay).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_subscription_delivers_after_eose() {
    use nostr_sdk::prelude::*;

    let relay = spawn_relay().await;
    let user_kp = link_custodial(&relay.state, "actor-live").await;
    let keys = Keys::parse(&hex::encode(user_kp.secret_bytes())).unwrap();

    let anon = Client::default();
    anon.add_relay(&relay.url).await.unwrap();
    anon.connect().await;
    let filter = Filter::new()
        .kinds([Kind::TextNote])
        .authors([keys.public_key()]);
    anon.subscribe(filter).await.unwrap();
    // `subscribe` only guarantees the REQ was *sent*. Complete one more
    // round-trip on the same connection — messages on one socket are handled
    // in order, so once this REQ/EOSE finishes, the live subscription above is
    // registered and the publish below cannot race past it.
    let _ = anon
        .fetch_events(Filter::new().kinds([Kind::Metadata]))
        .timeout(Duration::from_secs(5))
        .await
        .expect("sync round-trip");

    // Take the notification receiver BEFORE publishing — it is a broadcast
    // subscription, and an event delivered before the receiver exists is
    // simply gone.
    let mut notifications = anon.notifications();

    let writer = Client::builder()
        .authenticator(SignerAuthenticator::new(keys.clone()))
        .build();
    writer.add_relay(&relay.url).await.unwrap();
    writer.connect().await;
    let event = EventBuilder::new(Kind::TextNote, "live delivery")
        .finalize(&keys)
        .unwrap();
    writer.send_event(&event).await.expect("publish");
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(n) = futures_util::StreamExt::next(&mut notifications).await {
            if let ClientNotification::Event { event, .. } = n
                && event.content == "live delivery"
            {
                return true;
            }
        }
        false
    })
    .await
    .expect("live event within 10s");
    assert!(got, "the live subscription must deliver the note");

    writer.disconnect().await;
    anon.disconnect().await;
}

/// NIP-57 category-2 acceptance, over a **real WebSocket** — the wiring pin.
///
/// The unit-level pins in `conformance_nostr_zap_trust_gate.rs` call
/// `handle_zap_receipt_inbox` directly, so they would all stay green if the
/// kind-9735 arm were deleted from the EVENT dispatch. This test is what
/// makes that deletion fail: it drives the production relay route end to end
/// and asserts the receipt is accepted **unauthenticated** — which is only
/// possible through the carve-out, since a zap receipt is authored by the
/// payee's LNURL server and the generic owner-only gate refuses it
/// `restricted:` (that refusal is exactly why this half of `nostr.md` § The
/// relay event store category (2) stood unbuilt since ratification).
#[tokio::test]
async fn unauthed_zap_receipt_is_accepted_only_when_the_signer_is_designated() {
    use fauna_bridge_nostr::types::{Tag, UnsignedEvent};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let relay = spawn_relay().await;
    let actor_hex = hex::encode([0x11u8; 32]);
    let payee = link_custodial(&relay.state, &actor_hex).await;

    // Build a real, signature-valid receipt from `signer` for the payee.
    let receipt_from = |signer: &Keypair, target: &str| {
        let now = fauna_core::data::Timestamp::now_secs() as u64;
        let description = serde_json::json!({
            "kind": 9734,
            "pubkey": "d".repeat(64),
            "tags": [["p", payee.public_key_hex()]],
            "content": "",
        })
        .to_string();
        signer.sign_event(UnsignedEvent {
            pubkey: signer.public_key_bytes(),
            created_at: now,
            kind: 9735,
            tags: vec![
                Tag::new(vec!["p".into(), payee.public_key_hex()]),
                Tag::new(vec!["e".into(), target.to_string()]),
                Tag::new(vec!["bolt11".into(), "lnbc210n1pjfake".into()]),
                Tag::new(vec!["description".into(), description]),
            ],
            content: String::new(),
        })
    };

    async fn next_ok(
        ws: &mut (
                 impl StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin
             ),
    ) -> serde_json::Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("relay reply within 5s")
                .expect("stream open")
                .expect("ws ok");
            let v: serde_json::Value =
                serde_json::from_str(msg.into_text().unwrap().as_str()).unwrap();
            if v[0] == "OK" {
                return v;
            }
        }
    }

    let signer = Keypair::generate();
    let (mut ws, _) = tokio_tungstenite::connect_async(&relay.url).await.unwrap();

    // 1. Undesignated signer → refused, and refused as `restricted:` rather
    //    than `auth-required:`, proving the carve-out (not the generic
    //    unauthenticated gate) is what handled it.
    let undesignated = receipt_from(&signer, "post-1");
    ws.send(WsMessage::Text(
        serde_json::json!(["EVENT", undesignated])
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let reply = next_ok(&mut ws).await;
    assert_eq!(reply[2], false, "undesignated must be refused, got {reply}");
    assert!(
        reply[3].as_str().unwrap().starts_with("restricted:"),
        "the zap carve-out handled it, got {reply}"
    );

    // 2. Designate the signer, then resend an equivalent receipt. The zapped
    //    event must be the payee's OWN, held on this box (the subject
    //    binding), so store a real payee-authored note as the subject.
    let payee_note = {
        use fauna_bridge_nostr::types::UnsignedEvent;
        let note = payee.sign_event(UnsignedEvent {
            pubkey: payee.public_key_bytes(),
            created_at: fauna_core::data::Timestamp::now_secs() as u64,
            kind: 1,
            tags: vec![],
            content: "a zappable note".into(),
        });
        let conn = relay.state.db.conn().await;
        fauna_nest::nostr::store::store_event(&conn, &note, false).expect("store note");
        note.id
    };
    {
        let conn = relay.state.db.conn().await;
        db::add_zap_signer(&conn, &actor_hex, &signer.public_key_hex(), "Alby").unwrap();
    }
    let designated = receipt_from(&signer, &payee_note);
    ws.send(WsMessage::Text(
        serde_json::json!(["EVENT", designated]).to_string().into(),
    ))
    .await
    .unwrap();
    let reply = next_ok(&mut ws).await;
    assert_eq!(
        reply[2], true,
        "a designated signer's receipt is accepted unauthenticated, got {reply}"
    );

    // The accepted receipt is both stored and counted — category (2) means
    // the event store, and the trust gate means the accounting table.
    let conn = relay.state.db.conn().await;
    assert_eq!(db::get_zap_total(&conn, &payee_note).unwrap(), (21_000, 1));
    assert_eq!(
        db::get_zap_total(&conn, "post-1").unwrap(),
        (0, 0),
        "the refused receipt left nothing behind"
    );
}

/// `nostr.md` § Replying to and quoting a nostr note — the real-client leg: a
/// user's reply to a swept note is derived by the nest's create-side arm,
/// signed at the custodial position, stored on the nest's own relay and handed
/// to the outbound channel; a real rust-nostr client reads it back and ITS OWN
/// tag parser (not ours) sees NIP-10 marked `root` + `reply` tags naming the
/// swept thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_derived_reply_reads_back_with_marked_tags_in_a_real_client() {
    use fauna_core::data::{ContentHash, Post, PostBody, Reference, Timestamp};
    use fauna_core::identity::ActorId;
    use nostr_sdk::prelude::*;

    // A relay whose outbound channel (what the sync worker drains) is observed
    // here, with the nest key the custodial seal uses as its signing key.
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    fauna_nest::nostr::init_db(&db)
        .await
        .expect("init nostr tables");
    let base = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&base).await;
    let mut config = (*base.config).clone();
    config.nest.domain = Some(format!("{addr}"));
    let nest_signing_key = Some(base.nest_identity.signing_key.clone());
    let state = Arc::new(AppState {
        config: Arc::new(config),
        nostr: fauna_nest::state::NostrState {
            sync_tx: tx,
            ..fauna_nest::state::NostrState::default()
        },
        nest_signing_key,
        ..base
    });
    let router = fauna_nest::nostr::routes().with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let relay_url = format!("ws://{addr}/nostr");

    let author = [0x61u8; 32];
    let user_kp = link_custodial(&state, &hex::encode(author)).await;

    // A swept thread: its root, and the note being replied to (mid-thread).
    let stranger = fauna_bridge_nostr::signing::Keypair::generate();
    let sign = |tags: Vec<fauna_bridge_nostr::types::Tag>, content: &str| {
        stranger.sign_event(fauna_bridge_nostr::types::UnsignedEvent {
            pubkey: stranger.public_key_bytes(),
            created_at: 1_700_000_000,
            kind: 1,
            tags,
            content: content.into(),
        })
    };
    let root = sign(vec![], "the thread root");
    let parent = sign(
        vec![fauna_bridge_nostr::types::Tag::new(vec![
            "e".into(),
            root.id.clone(),
            "".into(),
            "root".into(),
        ])],
        "a swept reply",
    );
    let parent_local = [0x5au8; 32];
    {
        let conn = state.db.conn().await;
        fauna_nest::nostr::store::store_event(&conn, &parent, false).unwrap();
        fauna_nest::nostr::db::insert_event_map(
            &conn,
            &hex::encode(parent_local),
            &parent.id,
            &parent.pubkey,
            "inbound",
        )
        .unwrap();
    }

    let post = Post {
        author: ActorId(author),
        created_at: Timestamp(1_710_892_800_000_000),
        body: PostBody::Text {
            content: "my reply".into(),
            facets: vec![],
        },
        references: vec![Reference::Reply {
            post_id: ContentHash::from_digest_raw(parent_local),
        }],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let body = fauna_core::encoding::canonical_encode(&post).unwrap();
    fauna_nest::nostr::publish::create_publish_inner(&state, author, [0x62u8; 32], &body)
        .await
        .expect("derive + publish");
    let sent = rx
        .try_recv()
        .expect("the reply is handed to the outbound channel");

    let anon = Client::default();
    anon.add_relay(&relay_url).await.unwrap();
    anon.connect().await;
    let user = PublicKey::from_hex(&user_kp.public_key_hex()).unwrap();
    let filter = Filter::new().kinds([Kind::TextNote]).authors([user]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let events = loop {
        let events = anon
            .fetch_events(filter.clone())
            .timeout(Duration::from_secs(5))
            .await
            .expect("REQ replay");
        if !events.is_empty() || tokio::time::Instant::now() > deadline {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let read = events
        .first()
        .expect("the derived reply rests on the nest's relay");
    assert_eq!(read.id.to_hex(), sent.event.id);
    assert_eq!(read.content, "my reply");

    let mut markers = Vec::new();
    // NIP-10 `e` tags read off the wire shape (`["e", <id>, <relay>, <marker>]`) —
    // nostr 0.45 dropped its generic standardized-tag view.
    for tag in read.tags.iter() {
        let t = tag.as_slice();
        if t.first().map(String::as_str) == Some("e") {
            markers.push((t[1].clone(), t.get(3).cloned()));
        }
    }
    assert_eq!(
        markers,
        vec![
            (root.id.clone(), Some("root".to_string())),
            (parent.id.clone(), Some("reply".to_string())),
        ],
        "rust-nostr must parse NIP-10 marked root + reply tags"
    );
    anon.disconnect().await;
}
