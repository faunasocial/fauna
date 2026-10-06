//! Integration test — the `fauna.email.send` handler emits the
//! `fauna.bridges.outbound_ready` drain-nudge push to the **MTA-role**
//! bridge when (and only when) it enqueues a remote-recipient row.
//!
//! This is the outbound twin of `segments_changed_push.rs`'s
//! `ingest_handler_emits_mail_received_push` (the inbound arrival push):
//! it nudges the Go MTA's outbound worker to drain promptly instead of
//! waiting up to one `fetch_outbound_due` poll cycle. The push is a
//! best-effort latency optimization — the poll is the correctness
//! backstop — and is filtered to `BridgeRole::Mta` (the MDA has no
//! outbound worker), unlike `config_changed` which fans to every bridge.
//!
//! Drives the registered handler through `RpcRouter` (the same path the
//! WS dispatcher takes in production) and reads the emit off
//! `WsState::subscribe` directly — the integration target is the emit
//! side, not the WS framing path.

mod common;
use common::approve_bridge;
use common::provision_recipient;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::email_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::email::{SendEmailReply, SendEmailRequest};
use fauna_protocol::{Frame, PushEvent, decode_frame, decode_strict as decode, encode_canonical};
use tokio::sync::mpsc;

/// Build an `AppState` (optionally with a deployment primary mail domain) plus a
/// router carrying the `fauna.email.send` handler. The domain, when present, is
/// a real `local_domains` row — the production source `primary_mail_domain`
/// reads (the legacy boot-time `state.email.domain` seam was removed).
async fn build(domain: Option<&str>) -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
    if let Some(domain) = domain {
        db.add_mail_domain(domain, true, "testing", "self_signed", None, None)
            .await
            .unwrap();
    }
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    email_handlers::register_email_handlers(&mut b);
    (b.build(), state)
}

fn send_req(recipients: &[&str], from: &str) -> Bytes {
    let raw = format!(
        "From: {from}\r\nTo: {}\r\nSubject: hi\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=UTF-8\r\n\r\nHello.",
        recipients.join(", "),
    )
    .into_bytes();
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: recipients.iter().map(|r| r.to_string()).collect(),
        raw_rfc5322: raw,
    };
    Bytes::from(encode_canonical(&req).expect("encode req").to_vec())
}

async fn drive_send(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    payload: Bytes,
) -> SendEmailReply {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta("fauna.email.send")
        .expect("send kind registered");
    let bytes = (meta.handler)(state, actor, payload)
        .await
        .expect("send ok");
    decode(&bytes).expect("decode reply")
}

/// Wait up to `timeout` for a single `BridgeOutboundReady` push on `rx`;
/// panic on any other push kind. `None` ⇒ nothing arrived in the window.
async fn recv_outbound_ready(rx: &mut mpsc::Receiver<Bytes>, timeout: Duration) -> Option<()> {
    let bytes = match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(b)) => b,
        Ok(None) | Err(_) => return None,
    };
    let frame = decode_frame(&bytes).expect("decode frame");
    let push = match frame {
        Frame::Push(p) => p,
        other => panic!("expected Push frame, got {other:?}"),
    };
    match PushEvent::from_push(&push.kind, push.payload) {
        PushEvent::BridgeOutboundReady(_) => Some(()),
        other => panic!("expected BridgeOutboundReady push, got {}", other.kind()),
    }
}

/// A `fauna.email.send` with a remote recipient enqueues one outbound row
/// and emits exactly one `fauna.bridges.outbound_ready` push to the
/// approved MTA bridge — and **not** to an approved MDA bridge (the MDA
/// has no outbound worker; the emit is role-filtered).
#[tokio::test]
async fn email_send_emits_outbound_ready_to_mta_only() {
    let (router, state) = build(None).await; // no domain → recipient is remote
    let mta = [0x11u8; 32];
    let mda = [0x22u8; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;
    approve_bridge(&state.db, &mda, BridgeRole::Mda, &[0x99u8; 32]).await;

    // Subscribe both bridges before the send so the emit lands in-buffer.
    let (_mta_conn, mut mta_rx) = state.ws.subscribe(mta);
    let (_mda_conn, mut mda_rx) = state.ws.subscribe(mda);

    let reply = drive_send(
        &router,
        state.clone(),
        [1u8; 32],
        send_req(&["bob@example.com"], "alice@example.org"),
    )
    .await;
    assert_eq!(reply.remote_queued, 1, "one remote row queued");

    assert!(
        recv_outbound_ready(&mut mta_rx, Duration::from_secs(2))
            .await
            .is_some(),
        "the MTA bridge must receive the outbound_ready nudge",
    );
    assert!(
        recv_outbound_ready(&mut mda_rx, Duration::from_millis(300))
            .await
            .is_none(),
        "the MDA bridge must NOT receive the outbound_ready nudge (no outbound worker)",
    );
}

/// A purely-local send (in-domain recipient, no remote enqueue) emits no
/// `outbound_ready` push — the nudge fires only on a non-empty remote
/// enqueue.
///
/// The recipient must be genuinely **resolvable**, which is why this seeds
/// `provision_recipient`. An in-domain address the resolver rejects does not
/// stay local: it leaves for the outbound queue (see
/// `unresolvable_in_domain_recipient_goes_remote_and_nudges` below), so a
/// fixture that skipped the provisioning would be asserting the opposite of
/// what this test is named for. It did, until 2026-08-23 — the assertion read
/// `remote_queued == 0` against an unprovisioned `bob@fauna.test` and had been
/// red on `origin/main`.
#[tokio::test]
async fn local_only_send_emits_no_outbound_ready() {
    let (router, state) = build(Some(common::TEST_DOMAIN)).await;
    let mta = [0x33u8; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;
    let (_mta_conn, mut mta_rx) = state.ws.subscribe(mta);
    // Exact alias + published pubkey: the pair that makes `bob@` resolve to a
    // Mailbox and the seal succeed.
    provision_recipient(&state, [0xb0u8; 32], "bob", [0x5eu8; 32]).await;

    // Off-domain From skips the handle check; the recipient is in-domain →
    // local-only → no remote enqueue.
    let reply = drive_send(
        &router,
        state.clone(),
        [2u8; 32],
        send_req(&["bob@fauna.test"], "alice@somewhere.else"),
    )
    .await;
    assert_eq!(
        reply.local_delivered, 1,
        "the provisioned in-domain recipient must be delivered locally",
    );
    assert_eq!(reply.remote_queued, 0, "nothing queued for remote delivery");

    assert!(
        recv_outbound_ready(&mut mta_rx, Duration::from_millis(300))
            .await
            .is_none(),
        "a purely-local send must not emit outbound_ready",
    );
}

/// The ruled counterpart, and the reason the test above needs provisioning:
/// an in-domain address this deployment has no alias for is **not** delivered
/// locally and **not** an error — it leaves local delivery for the outbound
/// queue, so our own MX perimeter re-decides with the same resolver and emits
/// the bounce. The push therefore fires, exactly as for a remote recipient.
///
/// Uniform across all five resolver reject reasons and shared with
/// `submit_outbound`'s in-domain partition; the two doors move together or not
/// at all. Owner: `mail-aliases.md` § Per-alias rate-cap → *Second consumer,
/// deliberately left uniform* (ruled 2026-08-23). Nothing pinned this arm
/// before, which is how the sibling test above sat red on a premise production
/// had deliberately changed.
#[tokio::test]
async fn unresolvable_in_domain_recipient_goes_remote_and_nudges() {
    let (router, state) = build(Some(common::TEST_DOMAIN)).await;
    let mta = [0x44u8; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;
    let (_mta_conn, mut mta_rx) = state.ws.subscribe(mta);
    // Deliberately NO provisioning: `nobody@` resolves to Reject 550 "User
    // unknown" (no exact alias, no catch-all).

    let reply = drive_send(
        &router,
        state.clone(),
        [3u8; 32],
        send_req(&["nobody@fauna.test"], "alice@somewhere.else"),
    )
    .await;
    assert_eq!(
        reply.local_delivered, 0,
        "an unknown in-domain address is not delivered locally",
    );
    assert_eq!(
        reply.remote_queued, 1,
        "an unknown in-domain address goes onto the outbound queue, where our \
         own MX re-decides — not `remote_errors`, not a silent drop",
    );
    assert!(
        reply.remote_errors.is_empty(),
        "a resolver *reject* is not a per-recipient error: {:?}",
        reply.remote_errors,
    );

    assert!(
        recv_outbound_ready(&mut mta_rx, Duration::from_secs(2))
            .await
            .is_some(),
        "the enqueue is real, so the MTA nudge must fire",
    );
}
