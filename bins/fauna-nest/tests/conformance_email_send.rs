//! Integration round-trip for `fauna.email.send` (Layer-3 outbound
//! submission). Companion to `conformance_email_filters.rs` — both
//! handlers register through the same `register_email_handlers` entry
//! point and reach `CacheDb` directly (no `BridgeProviderRegistry`).
//!
//! Authority for the wire types: `libs/fauna-protocol/src/email.rs`
//! § fauna.email.send. Marathon collapse — T7 + T8 + T9-email-send +
//! T10-email-send-route landed in one commit, after the per-session
//! marathon
//! framing established that the HTTP twin had no real consumer worth
//! preserving (Linux's call site was a stub; the Rust bridge-daemon's
//! was dormant cutover-prep, gutted in the same commit; the Go mail
//! bridge already routes outbound via `fauna.bridges.
//! enqueue_outbound_mail`).

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    db::CacheDb, email_handlers, email_handlers::SEND_RATE_LIMIT_PER_HOUR, routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    decode_strict as decode,
    email::{SendEmailReply, SendEmailRequest},
    encode_canonical,
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    email_handlers::register_email_handlers(&mut b);
    (b.build(), state)
}

async fn router_with_domain(domain: &str) -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.add_mail_domain(domain, true, "testing", "self_signed", None, None)
        .await
        .unwrap();
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    email_handlers::register_email_handlers(&mut b);
    (b.build(), state)
}

fn encode_req(req: &SendEmailRequest) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

fn message_with_from(from: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: bob@example.com\r\nSubject: hi\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\nHello.",
    )
    .into_bytes()
}

// ── happy paths ─────────────────────────────────────────────────

#[tokio::test]
async fn send_single_remote_recipient_queues_outbound() {
    // No deployment domain → every recipient is remote → goes onto
    // the outbound queue.
    let (router, state) = router_with_db_only().await;
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec!["bob@example.com".into()],
        raw_rfc5322: message_with_from("alice@example.org"),
    };
    let reply_bytes = dispatch(
        &router,
        state,
        [1u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect("send ok");
    let reply: SendEmailReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.local_delivered, 0, "no local domain → nothing local");
    assert_eq!(reply.remote_queued, 1, "one row queued");
    assert!(
        reply.remote_errors.is_empty(),
        "no errors: {:?}",
        reply.remote_errors
    );
}

#[tokio::test]
async fn send_multi_remote_recipients_queues_each() {
    let (router, state) = router_with_db_only().await;
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec![
            "bob@example.com".into(),
            "carol@other.example".into(),
            "dave@third.example".into(),
        ],
        raw_rfc5322: message_with_from("alice@example.org"),
    };
    let reply_bytes = dispatch(
        &router,
        state,
        [2u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect("send ok");
    let reply: SendEmailReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.remote_queued, 3, "three rows queued");
    assert!(reply.remote_errors.is_empty());
}

#[tokio::test]
async fn send_passes_recipients_through_verbatim() {
    // The shared `fauna_mail::routing::merge_recipients` helper
    // doesn't dedup (the legacy HTTP twin's behavior — preserved here
    // for parity; per-address dedup would be a behavior change that
    // affects HTTP-twin callers too, scoped out of the T7+T8 collapse).
    // A caller submitting the same address twice gets two queue rows.
    let (router, state) = router_with_db_only().await;
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec![
            "bob@example.com".into(),
            "bob@example.com".into(),
            "carol@example.com".into(),
        ],
        raw_rfc5322: message_with_from("alice@example.org"),
    };
    let reply_bytes = dispatch(
        &router,
        state,
        [3u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect("send ok");
    let reply: SendEmailReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.remote_queued, 3, "no dedup — three rows queued");
}

// ── validation ──────────────────────────────────────────────────

#[tokio::test]
async fn send_rejects_empty_recipients() {
    let (router, state) = router_with_db_only().await;
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: Vec::new(),
        raw_rfc5322: message_with_from("alice@example.org"),
    };
    let err = dispatch(
        &router,
        state,
        [4u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect_err("empty recipients rejected");
    assert_eq!(err.code, "fauna.email.invalid_params");
}

#[tokio::test]
async fn send_rejects_malformed_recipient_address() {
    let (router, state) = router_with_db_only().await;
    let req = SendEmailRequest {
        extra: Default::default(),
        // No @ — partition_recipients rejects.
        recipients: vec!["not-an-address".into()],
        raw_rfc5322: message_with_from("alice@example.org"),
    };
    let err = dispatch(
        &router,
        state,
        [5u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect_err("bad address rejected");
    assert_eq!(err.code, "fauna.email.invalid_params");
}

// ── from-handle verification (when deployment domain is set) ────

#[tokio::test]
async fn send_with_matching_domain_rejects_unverified_handle() {
    // Deployment domain is "fauna.test"; the From: address matches the
    // domain but the actor has no handle registered for "alice", so the
    // handler must refuse. Mirrors the HTTP twin's `forbidden(...)`
    // arm at `email_routes.rs` § from-address verification.
    let (router, state) = router_with_domain("fauna.test").await;
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec!["bob@example.com".into()],
        raw_rfc5322: message_with_from("alice@fauna.test"),
    };
    let err = dispatch(
        &router,
        state,
        [6u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect_err("unverified handle rejected");
    assert_eq!(err.code, "fauna.email.permission_denied");
}

#[tokio::test]
async fn send_with_off_domain_from_skips_handle_check() {
    // From: domain does not match the deployment domain → no handle
    // verification (the deployment isn't claiming authority over the
    // From: domain). Outbound queue still accepts the message.
    let (router, state) = router_with_domain("fauna.test").await;
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec!["bob@example.com".into()],
        raw_rfc5322: message_with_from("alice@somewhere.else"),
    };
    let reply_bytes = dispatch(
        &router,
        state,
        [7u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect("off-domain send ok");
    let reply: SendEmailReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.remote_queued, 1);
}

#[tokio::test]
async fn send_over_the_ceiling_is_refused_with_the_typed_too_large_code() {
    // The typed first-party sibling of the SMTP perimeter's 552: a raw message
    // over `effective_max_raw_message_bytes` is refused with the shared
    // `MESSAGE_TOO_LARGE_CODE` so a client renders "too large" rather than a raw
    // transport failure. A tiny admin `max_message_bytes` override makes a small
    // message trip it (mirrors the import admission test) — same shared rule.
    let (router, state) = router_with_db_only().await;
    state
        .db
        .put_spam_policy(fauna_nest::db::mail_policy::SpamPolicyOverrides {
            max_message_bytes: Some(64),
            ..Default::default()
        })
        .await
        .unwrap();
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec!["bob@example.com".into()],
        raw_rfc5322: message_with_from("alice@example.org"), // well over 64 bytes
    };
    assert!(
        req.raw_rfc5322.len() > 64,
        "the fixture must exceed the ceiling"
    );
    let err = dispatch(
        &router,
        state,
        [9u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect_err("an over-ceiling send must be refused, not queued");
    assert_eq!(
        err.code,
        fauna_protocol::email::MESSAGE_TOO_LARGE_CODE,
        "the refusal must be the typed too-large code, not a generic failure"
    );
}

// ── replay semantics ────────────────────────────────────────────

#[tokio::test]
async fn send_kind_forbids_replay() {
    // Outbound delivery is the canonical "rare dangerous op" — replay
    // on connection recovery could double-send to remote MX. The
    // forbid_replay flag pins the dispatch semantics at the call site.
    let (router, _state) = router_with_db_only().await;
    let meta = router
        .kind_meta("fauna.email.send")
        .expect("kind registered");
    assert!(meta.forbid_replay, "fauna.email.send must forbid replay");
    assert_eq!(
        meta.default_deadline,
        std::time::Duration::from_secs(30),
        "30 s deadline envelopes per-recipient routing + queue insert",
    );
}

// ── outbound metering: the cap counts the CALLER, not the header ─────────

#[tokio::test]
async fn the_hourly_ceiling_binds_the_actor_not_a_rotating_from() {
    // The per-hour outbound ceiling used to count rows by
    // `original_sender` — the caller-supplied `From:` string. An **off-domain**
    // `From:` bypasses the handle gate by design (`mail-app-surface.md`
    // § Sender-handle verification: the deployment "isn't authoritative for
    // them"), so a fresh string each message was a fresh counter each message,
    // and the ceiling was defeated by editing a header. The counter must name
    // the authenticated actor instead (`mail-mass-mailing.md` § How the
    // per-list cap separates from per-actor: "Regular one-to-one mail
    // (`fauna.email.send` …) consumes the per-actor counters").
    //
    // Rotating the `From:` on every message is the whole point — a test that
    // reuses one `From:` passes against the broken code and proves nothing.
    let (router, state) = router_with_db_only().await;
    let actor = [21u8; 32];

    for i in 0..SEND_RATE_LIMIT_PER_HOUR {
        let req = SendEmailRequest {
            extra: Default::default(),
            recipients: vec!["bob@example.com".into()],
            raw_rfc5322: message_with_from(&format!("rotating-{i}@somewhere.else")),
        };
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.email.send",
            encode_req(&req),
        )
        .await
        .unwrap_or_else(|e| panic!("send {i} within the ceiling must be allowed: {}", e.code));
    }

    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec!["bob@example.com".into()],
        raw_rfc5322: message_with_from("one-more@somewhere.else"),
    };
    let err = dispatch(&router, state, actor, "fauna.email.send", encode_req(&req))
        .await
        .expect_err("the message past the hourly ceiling must be refused");
    assert_eq!(
        err.code, "fauna.email.rate_limited",
        "a fresh off-domain From: must not mint a fresh counter",
    );
}

#[tokio::test]
async fn a_second_actor_gets_its_own_hourly_allowance() {
    // The other half of keying on the actor: one actor exhausting the ceiling
    // must not refuse a *different* actor's first message. Pins that the fix
    // narrowed the key rather than making the ceiling deployment-wide.
    let (router, state) = router_with_db_only().await;

    for i in 0..SEND_RATE_LIMIT_PER_HOUR {
        let req = SendEmailRequest {
            extra: Default::default(),
            recipients: vec!["bob@example.com".into()],
            raw_rfc5322: message_with_from(&format!("rotating-{i}@somewhere.else")),
        };
        dispatch(
            &router,
            state.clone(),
            [22u8; 32],
            "fauna.email.send",
            encode_req(&req),
        )
        .await
        .expect("the loud actor's own sends are within its ceiling");
    }

    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec!["carol@example.com".into()],
        raw_rfc5322: message_with_from("quiet@somewhere.else"),
    };
    let reply_bytes = dispatch(
        &router,
        state,
        [23u8; 32],
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect("a second actor's first send must not inherit the first actor's count");
    let reply: SendEmailReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.remote_queued, 1);
}

#[tokio::test]
async fn the_daily_recipient_quota_is_the_one_raw_smtp_submission_consumes() {
    // `mail-mass-mailing.md` § How the per-list cap separates from per-actor:
    // "Regular one-to-one mail (`fauna.email.send` / authenticated raw-SMTP
    // submission) consumes the per-actor counters." Until 2026-08-23 only the
    // SMTP half of that sentence was true, so a user could dual-stream — spend the
    // day quota over port 465/587 AND keep sending over this RPC, which
    // consumed nothing.
    //
    // A small admin override makes the quota reachable in a test (the catalog
    // default is 1000 recipients/day); the point is that the RPC reads the
    // SAME nest-side counter and the SAME `.effective()` overlay the bridge's
    // `check_submission_quota` does, not a second parallel accounting.
    let (router, state) = router_with_db_only().await;
    state
        .db
        .put_submission_policy(fauna_nest::db::mail_policy::SubmissionPolicyOverrides {
            max_per_day: Some(3),
            ..Default::default()
        })
        .await
        .unwrap();
    let actor = [24u8; 32];

    // Three recipients, one message: exactly the allowance.
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec![
            "a@example.com".into(),
            "b@example.com".into(),
            "c@example.com".into(),
        ],
        raw_rfc5322: message_with_from("alice@somewhere.else"),
    };
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect("a send exactly at the daily allowance is allowed");
    let reply: SendEmailReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.remote_queued, 3);

    // The fourth recipient of the day is over it — and a rotated `From:`
    // changes nothing, because the quota is keyed on the actor.
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec!["d@example.com".into()],
        raw_rfc5322: message_with_from("someone-else@somewhere.else"),
    };
    let err = dispatch(&router, state, actor, "fauna.email.send", encode_req(&req))
        .await
        .expect_err("past the daily recipient quota must be refused");
    assert_eq!(err.code, "fauna.email.rate_limited");
}

#[tokio::test]
async fn the_sender_address_cap_still_bounds_one_from_across_actors() {
    // The subordinate half of the two-key cap. Keying on the actor alone would
    // let N actors each spend a full hourly allowance forging the SAME
    // off-domain `From:` — the handle gate does not reach off-domain
    // addresses, so nothing else bounds one forged address deployment-wide.
    // The `original_sender` count is kept for exactly that, so it is pinned
    // here rather than left to rot as dead code.
    let (router, state) = router_with_db_only().await;
    let forged = "ceo@bank.example";

    for i in 0..SEND_RATE_LIMIT_PER_HOUR {
        // A DIFFERENT actor every message, so the per-actor ceiling never
        // engages and only the per-sender one can refuse.
        let mut actor = [30u8; 32];
        actor[0] = i as u8;
        actor[1] = (i >> 8) as u8;
        let req = SendEmailRequest {
            extra: Default::default(),
            recipients: vec!["bob@example.com".into()],
            raw_rfc5322: message_with_from(forged),
        };
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.email.send",
            encode_req(&req),
        )
        .await
        .unwrap_or_else(|e| panic!("send {i} is each actor's first: {}", e.code));
    }

    let mut fresh_actor = [31u8; 32];
    fresh_actor[0] = 0xff;
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients: vec!["bob@example.com".into()],
        raw_rfc5322: message_with_from(forged),
    };
    let err = dispatch(
        &router,
        state,
        fresh_actor,
        "fauna.email.send",
        encode_req(&req),
    )
    .await
    .expect_err("one forged From: must stay bounded across actors");
    assert_eq!(err.code, "fauna.email.rate_limited");
}
