//! Integration test — pushes the mail-ingest + compaction paths emit.
//!
//! `fauna.segments.changed` fires when:
//!
//! 1. The production `fauna.bridges.ingest_inbound_mail` handler causes
//!    a rotation (bucket change for an actor with an already-open
//!    segment) → a single `SegmentChange::Finalized` push for the
//!    closed segment id is emitted via `WsState::notify_push`.
//! 2. `CompactionWorker::run_once` rewrites a bucket → one
//!    `SegmentChange::CompactedIn` push for the new segment (if any
//!    survivors) plus one `SegmentChange::CompactedOut` push for each
//!    consumed input.
//!
//! `fauna.mail.received` fires on every newly-stored inbound message
//! (the per-record arrival push, distinct from the segment-lifecycle
//! `segments.changed`) — `ingest_handler_emits_mail_received_push` below.
//! Because ingest now co-emits it alongside any rotation `segments
//! .changed`, `drain_segments_changed` skips it (see there).
//!
//! Drives `WsState::subscribe` directly (rather than upgrading a real
//! WebSocket) — the integration target here is the emit side, not the
//! WS framing path (already covered by `tests/push_event_typed.rs`).

mod common;
use common::{approve_bridge, sealed};

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::segments::mail::append_record;
use fauna_nest::segments::{CompactScope, CompactionTrigger, CompactionWorker};
use fauna_protocol::bridge_routing::{
    AuthVerdicts, DkimVerdict, DmarcVerdict, IngestInboundMailRequest, PublicMailMetadata,
    SpfVerdict,
};
use fauna_protocol::push_events::{MailReceivedPayload, SegmentChange, SegmentsChangedPayload};
use fauna_protocol::{Frame, PushEvent, decode_frame, encode_canonical};
use fauna_segment_store::SegmentManager;
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Wrap a byte literal as an at-rest sealed carrier for a direct
/// `append_record` call (S6.12b). These tests exercise segment rotation /
/// finalization mechanics, not seal genuineness, so `carried_at_rest_unchecked`
/// satisfies the typed gate without minting a real HPKE seal.
fn at_rest(bytes: Vec<u8>) -> fauna_mls::wrapped_blob::SealedRecordBytes {
    fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(bytes)
}

/// Build an `AppState` with `mail_segments` pinned to `tmp` (rather than
/// the per-process tempdir `for_test` defaults to). The RPC router is
/// wired with `register_bridge_routing_handlers` so the rotation test
/// can drive the `fauna.bridges.ingest_inbound_mail` handler end-to-end.
/// Returns `(tmp, state)`; drop the tempdir last.
fn build_state() -> (TempDir, Arc<AppState>) {
    let tmp = TempDir::new().expect("tmp");
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        register_bridge_routing_handlers(&mut b);
        b.build()
    });
    let mut state = AppState::for_test(db.clone());
    state.mail_segments = Arc::new(SegmentManager::new(tmp.path().to_path_buf(), "mail"));
    state.rpc_router = rpc_router;
    (tmp, Arc::new(state))
}

/// Build an MTA ingest request with the given recipient + body + timestamp.
fn ingest_req(target: &[u8; 32], body: &[u8], timestamp: i64) -> IngestInboundMailRequest {
    let sealed_body = sealed(body);
    let body_len = sealed_body.len() as u32;
    IngestInboundMailRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: target.to_vec(),
        encrypted_body: sealed_body,
        encrypted_index_hint: sealed(b"index-hint"),
        public_metadata: PublicMailMetadata {
            timestamp,
            ciphertext_size: body_len,
            sender_domain: "example.com".into(),
        },
        verdicts: AuthVerdicts {
            dkim: DkimVerdict::Pass,
            spf: SpfVerdict::Pass,
            dmarc: DmarcVerdict::Pass,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Drive `fauna.bridges.ingest_inbound_mail` through the RPC router —
/// the same path the WS dispatcher takes in production. Returns the raw
/// reply bytes on success.
async fn drive_ingest(
    state: &Arc<AppState>,
    bridge_actor: [u8; 32],
    req: &IngestInboundMailRequest,
) -> Bytes {
    let meta = state
        .rpc_router
        .kind_meta("fauna.bridges.ingest_inbound_mail")
        .expect("ingest kind registered");
    let payload = Bytes::from(encode_canonical(req).expect("encode req").to_vec());
    (meta.handler)(state.clone(), bridge_actor, payload)
        .await
        .expect("handler ok")
}

/// Drain the `mpsc::Receiver<Bytes>` returned by `WsState::subscribe`
/// for exactly `expect_count` `SegmentsChanged` push events and return
/// them. Times out after 2 s; panics with diagnostic context on
/// shortfall or off-kind events.
async fn drain_segments_changed(
    rx: &mut mpsc::Receiver<Bytes>,
    expect_count: usize,
) -> Vec<SegmentsChangedPayload> {
    let mut out = Vec::with_capacity(expect_count);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while out.len() < expect_count {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let bytes = match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(b)) => b,
            Ok(None) => panic!(
                "push channel closed after {} of {} events",
                out.len(),
                expect_count,
            ),
            Err(_) => panic!(
                "timeout waiting for push event {}/{}",
                out.len() + 1,
                expect_count,
            ),
        };
        let frame = decode_frame(&bytes).expect("decode frame");
        let push = match frame {
            Frame::Push(p) => p,
            other => panic!("expected Push frame, got {other:?}"),
        };
        let event = PushEvent::from_push(&push.kind, push.payload);
        match event {
            PushEvent::SegmentsChanged(p) => out.push(p),
            // Mail ingest co-emits a per-record `fauna.mail.received`
            // arrival push alongside the segment-lifecycle push; it's not
            // what this drainer collects, so skip without counting it.
            PushEvent::MailReceived(_) => {}
            other => panic!("expected SegmentsChanged push, got {}", other.kind()),
        }
    }
    out
}

/// Drain the receiver for exactly `expect_count` `MailReceived` arrival
/// pushes (skipping any `SegmentsChanged` the same ingest may co-emit).
/// Times out after 2 s; panics with context on shortfall or off-kind.
async fn drain_mail_received(
    rx: &mut mpsc::Receiver<Bytes>,
    expect_count: usize,
) -> Vec<MailReceivedPayload> {
    let mut out = Vec::with_capacity(expect_count);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while out.len() < expect_count {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let bytes = match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(b)) => b,
            Ok(None) => panic!(
                "push channel closed after {} of {} mail.received events",
                out.len(),
                expect_count,
            ),
            Err(_) => panic!(
                "timeout waiting for mail.received push {}/{}",
                out.len() + 1,
                expect_count,
            ),
        };
        let frame = decode_frame(&bytes).expect("decode frame");
        let push = match frame {
            Frame::Push(p) => p,
            other => panic!("expected Push frame, got {other:?}"),
        };
        let event = PushEvent::from_push(&push.kind, push.payload);
        match event {
            PushEvent::MailReceived(p) => out.push(p),
            PushEvent::SegmentsChanged(_) => {}
            other => panic!("expected MailReceived push, got {}", other.kind()),
        }
    }
    out
}

/// End-to-end: drive a real `fauna.bridges.ingest_inbound_mail` call
/// through the RPC router, with the actor's first segment pre-finalized
/// so the next append rotates (instead of relying on the handler's
/// `now_epoch_millis()` for bucket derivation, which would put both
/// appends in the same wall-clock bucket and skip rotation).
///
/// This exercises the production push-emit code path in
/// `bridge_routing_handlers::persist_inbound_mail_request` —
/// `MailInsertOutcome.finalized` flowing into `state.ws.notify_push`.
#[tokio::test]
async fn ingest_handler_emits_finalized_push_after_pre_rotate() {
    let (_tmp, state) = build_state();
    let target = [0xAAu8; 32];

    // Provision MLS pubkey + approve an MTA-class bridge — the
    // ingest_inbound_mail handler refuses to persist without these.
    common::seed_recipient_seal_key(&state.db, &target, &common::FIXTURE_MSEK).await;
    let bridge = [0x11u8; 32];
    approve_bridge(&state.db, &bridge, BridgeRole::Mta, &[0x99u8; 32]).await;

    // Drive a first ingest end-to-end so a segment is open for `target`.
    // After this returns, seg 1 is the actor's open segment in whichever
    // wall-clock bucket the handler resolved (handler uses
    // now_epoch_millis() to derive received_at + bucket — *not* the
    // request timestamp, which is the SMTP sender-side timestamp).
    let req_a = ingest_req(&target, b"sealed-a", 1_715_000_000);
    let _ = drive_ingest(&state, bridge, &req_a).await;

    // Manually finalize the open segment via the manager — this leaves
    // the next handler-driven append to *re-open* the actor's segment.
    // But the next ingest's `now_epoch_millis()` bucket equals the prior
    // one (consecutive calls within the same minute), so reopening
    // doesn't rotate from a previously-open segment in another bucket.
    //
    // To force the rotation, finalize seg 1 (it's already in the
    // manifest), then poison the manager's *in-memory* per-actor state
    // by inserting a sentinel append in a different bucket. Then the
    // next handler-driven ingest will land in the now-current
    // wall-clock bucket → rotate → finalized = Some(prior_seg).
    append_record(
        &state.mail_segments,
        &state.db,
        &target,
        &at_rest(b"sentinel".to_vec()),
        &at_rest(b"".to_vec()),
        common::floor(1_000_000_000_000), // far in the past → different bucket
    )
    .await
    .expect("seed open in another bucket");
    // After this seed, the manager's in-memory open segment for `target`
    // is in the year-2001 bucket. The next handler-driven ingest uses
    // now_epoch_millis() → a 2026+ bucket → rotation.

    // Subscribe AFTER the prep work so only the rotation-driven push is
    // in the receiver's buffer.
    let (_conn, mut rx) = state.ws.subscribe(target);

    let req_b = ingest_req(&target, b"sealed-b", 1_715_000_001);
    let _ = drive_ingest(&state, bridge, &req_b).await;

    let pushes = drain_segments_changed(&mut rx, 1).await;
    assert_eq!(pushes.len(), 1);
    let p = &pushes[0];
    assert_eq!(p.kind, "mail");
    assert_eq!(p.actor_id, hex::encode(target));
    assert_eq!(p.change, SegmentChange::Finalized);
    // The closed seg id is whatever the sentinel-append produced —
    // segment numbering started at 1 for req_a, and the sentinel
    // produced seg 2 (different bucket → rotation). So seg 2 is the
    // one finalized when req_b lands.
    assert_eq!(p.segment_id, 2, "the sentinel-bucket segment just closed");
}

/// A single fresh inbound ingest emits exactly one `fauna.mail.received`
/// arrival push to the recipient actor — the per-record push that drives
/// the client's prompt inbox fetch and the custodian pull's prompt
/// pass. (A first-ever ingest opens seg 1 and finalizes nothing, so no
/// `segments.changed` accompanies it.) A subsequent idempotent retry of
/// the same message must NOT re-emit — the push fires only on a genuinely
/// new insert.
#[tokio::test]
async fn ingest_handler_emits_mail_received_push() {
    let (_tmp, state) = build_state();
    let target = [0x5Au8; 32];

    common::seed_recipient_seal_key(&state.db, &target, &common::FIXTURE_MSEK).await;
    let bridge = [0x12u8; 32];
    approve_bridge(&state.db, &bridge, BridgeRole::Mta, &[0x99u8; 32]).await;

    // Subscribe before the ingest so the arrival push lands in the buffer.
    let (_conn, mut rx) = state.ws.subscribe(target);

    let req = ingest_req(&target, b"sealed-body", 1_715_000_000);
    let _ = drive_ingest(&state, bridge, &req).await;

    let pushes = drain_mail_received(&mut rx, 1).await;
    assert_eq!(pushes.len(), 1);
    assert_eq!(
        pushes[0].actor_id,
        hex::encode(target),
        "mail.received push carries the recipient actor id"
    );

    // Idempotent retry of the SAME message → inserted=false → no re-emit.
    let _ = drive_ingest(&state, bridge, &req).await;
    if let Ok(Some(bytes)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
        let frame = decode_frame(&bytes).expect("decode frame");
        if let Frame::Push(p) = frame {
            let ev = PushEvent::from_push(&p.kind, p.payload);
            assert!(
                !matches!(ev, PushEvent::MailReceived(_)),
                "idempotent retry must not re-emit fauna.mail.received"
            );
        }
    }
}

#[tokio::test]
async fn manager_append_outcome_carries_finalized_seg_id() {
    // Manager-level companion to the handler-driven test above: a
    // bare `append_record` call returns AppendOutcome.finalized
    // matching the just-closed segment id when rotation fires. This is
    // the contract that `db.insert_inbound_mail` and downstream
    // handlers depend on.
    let (_tmp, state) = build_state();
    let actor = [0xA0u8; 32];

    let first = append_record(
        &state.mail_segments,
        &state.db,
        &actor,
        &at_rest(b"a".to_vec()),
        &at_rest(b"".to_vec()),
        common::floor(1_715_000_000_000),
    )
    .await
    .expect("first append");
    assert!(
        first.finalized.is_none(),
        "first-ever append, nothing to close"
    );
    assert_eq!(first.seg_id, 1);

    let second = append_record(
        &state.mail_segments,
        &state.db,
        &actor,
        &at_rest(b"b".to_vec()),
        &at_rest(b"".to_vec()),
        common::floor(1_718_000_000_000),
    )
    .await
    .expect("rotation append");
    assert_eq!(
        second.finalized,
        Some(1),
        "rotation closed seg 1 → AppendOutcome.finalized = Some(1)"
    );
    assert_eq!(second.seg_id, 2);
}

/// Helper: append `n` records for `actor` in a single bucket
/// (received_at fixed) via the manager directly (no need to drive the
/// handler — this only seeds state for the compaction test).
async fn append_n(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    first: u8,
    n: usize,
    ts_ms: i64,
) -> Vec<fauna_cbor::Cid> {
    let mut cids = Vec::with_capacity(n);
    for i in 0..n {
        // Unique body per record — identical bytes would dedup under
        // content-hash identity. The returned cids are the DERIVED filing
        // identities (callers tombstone by them; no literal rids exist).
        let outcome = append_record(
            &state.mail_segments,
            &state.db,
            actor,
            &at_rest(format!("sealed-body-{first}-{i}").into_bytes()),
            &at_rest(b"sealed-hint".to_vec()),
            common::floor(ts_ms),
        )
        .await
        .expect("append");
        cids.push(outcome.cid);
    }
    cids
}

#[tokio::test]
async fn compaction_with_all_inputs_tombstoned_emits_only_compacted_out() {
    let (_tmp, state) = build_state();
    let actor = [0xBBu8; 32];

    // Two appends in the same bucket → seg 1 with two records.
    let cids = append_n(&state, &actor, 1, 2, 1_712_000_000_000).await;
    state
        .mail_segments
        .finalize_open(&actor)
        .await
        .expect("finalize");
    // Tombstone both records → 100 % fraction, above the Manual
    // threshold (0 %). Every input record is dead → no surviving
    // output segment.
    for cid in &cids {
        state
            .db
            .segment_records_mark_tombstoned(&actor, "mail", 1, cid)
            .await
            .expect("tombstone");
    }

    let (_conn, mut rx) = state.ws.subscribe(actor);

    let worker = CompactionWorker::new(state.clone(), Duration::from_secs(60));
    let report = worker
        .run_once(
            CompactScope {
                kind: Some("mail".into()),
                scope_id: Some(actor),
            },
            CompactionTrigger::Manual,
        )
        .await
        .expect("run_once");
    assert!(report.acquired_lock);
    assert_eq!(report.errors, 0);

    // Only a CompactedOut for seg 1 — no survivors → no CompactedIn.
    let pushes = drain_segments_changed(&mut rx, 1).await;
    assert_eq!(pushes.len(), 1);
    let out = &pushes[0];
    assert_eq!(out.kind, "mail");
    assert_eq!(out.actor_id, hex::encode(actor));
    assert_eq!(out.segment_id, 1);
    assert_eq!(out.change, SegmentChange::CompactedOut);
}

#[tokio::test]
async fn compaction_with_survivors_emits_in_and_out() {
    let (_tmp, state) = build_state();
    let actor = [0xCCu8; 32];

    // Seg 1: 3 records; tombstone 1 (33 % > 0 % Manual threshold).
    let cids = append_n(&state, &actor, 1, 3, 1_712_000_000_000).await;
    state
        .mail_segments
        .finalize_open(&actor)
        .await
        .expect("finalize");
    state
        .db
        .segment_records_mark_tombstoned(&actor, "mail", 1, &cids[0])
        .await
        .expect("tombstone one");

    let (_conn, mut rx) = state.ws.subscribe(actor);

    let worker = CompactionWorker::new(state.clone(), Duration::from_secs(60));
    let report = worker
        .run_once(
            CompactScope {
                kind: Some("mail".into()),
                scope_id: Some(actor),
            },
            CompactionTrigger::Manual,
        )
        .await
        .expect("run_once");
    assert!(report.acquired_lock);
    assert_eq!(report.errors, 0);
    assert_eq!(report.segments_rewritten, 1);

    // Compaction produced a new seg 2 (CompactedIn) and consumed seg 1
    // (CompactedOut). Two events expected; ordering is implementation-
    // defined — sort by change kind for the assert.
    let mut pushes = drain_segments_changed(&mut rx, 2).await;
    pushes.sort_by_key(|p| match p.change {
        SegmentChange::CompactedIn => 0,
        SegmentChange::CompactedOut => 1,
        _ => 2,
    });
    assert_eq!(pushes.len(), 2);
    assert_eq!(pushes[0].change, SegmentChange::CompactedIn);
    assert_eq!(pushes[0].segment_id, 2, "new compacted segment id");
    assert_eq!(pushes[0].kind, "mail");
    assert_eq!(pushes[1].change, SegmentChange::CompactedOut);
    assert_eq!(pushes[1].segment_id, 1, "consumed input segment id");
    assert_eq!(pushes[1].kind, "mail");
}
