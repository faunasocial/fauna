//! End-to-end mail-segment-store round trip (Plan 2 T10).
//!
//! Drives the full pipeline: `ingest_inbound_mail` RPC handler →
//! `segments::mail` over `SegmentManager` → on-disk segment + `segment_records` →
//! `query_bridge_imap_messages` + `query_bridge_imap_index_segments` →
//! decoded `MailRecordEnvelope`. Catches wiring issues the per-task
//! unit tests in `src/` cannot:
//!
//! * `AppState` plumbing (the `mail_segments` field actually being
//!   threaded into every code path that needs it).
//! * RPC handler argument threading (the `register_*_handlers` call
//!   list + the kind-meta lookup actually resolving).
//! * The MTA-receive → MDA-fetch round trip — write goes in via the
//!   RPC handler, reads come out via the same query helpers the MDA
//!   bridge uses.
//!
//! The test drives the handler *through* the `RpcRouter` (rather than
//! calling the private `persist_inbound_mail_request` directly) so the
//! kind-string + dispatch table participates in the test surface.

mod common;
use common::{approve_bridge, sealed};

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    AuthVerdicts, DkimVerdict, DmarcVerdict, IngestInboundMailReply, IngestInboundMailRequest,
    PublicMailMetadata, SpfVerdict,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};

/// Build an `AppState` with the bridge-routing handlers actually
/// registered on the RPC router (the bare `AppState::for_test`
/// router is empty).
async fn fixture_state() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        register_bridge_routing_handlers(&mut b);
        b.build()
    });
    Arc::new(AppState {
        rpc_router,
        ..AppState::for_test(db)
    })
}

#[tokio::test]
async fn mta_ingest_to_mda_fetch_round_trip() {
    let state = fixture_state().await;

    // 1. Provision an MLS pubkey for the recipient (the handler refuses
    //    to persist a record for an actor without one — see the
    //    "recipient has not provisioned an MLS pubkey" check).
    let recipient: [u8; 32] = [0x77; 32];
    common::seed_recipient_seal_key(&state.db, &recipient, &common::FIXTURE_MSEK).await;

    // 2. Approve an MTA-class bridge — the kind requires an approved
    //    bridge service-user with the `mta` role.
    let bridge_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &bridge_actor, BridgeRole::Mta, &[0x99u8; 32]).await;

    // 3. Build the ingest payload — wire shape exactly as a real MTA
    //    would frame it.
    let body: Vec<u8> = sealed(b"sealed-body-payload");
    let hint: Vec<u8> = sealed(b"sealed-index-hint");
    let req = IngestInboundMailRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: recipient.to_vec(),
        encrypted_body: body.clone(),
        encrypted_index_hint: hint.clone(),
        public_metadata: PublicMailMetadata {
            timestamp: 1_715_000_000,
            ciphertext_size: body.len() as u32,
            sender_domain: "example.com".into(),
        },
        verdicts: AuthVerdicts {
            dkim: DkimVerdict::Pass,
            spf: SpfVerdict::Pass,
            dmarc: DmarcVerdict::Pass,
            ..Default::default()
        },
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());

    // 4. Drive the handler through the RPC router — same path the WS
    //    dispatcher takes in production.
    let meta = state
        .rpc_router
        .kind_meta("fauna.bridges.ingest_inbound_mail")
        .expect("ingest_inbound_mail kind registered");
    let reply_bytes = (meta.handler)(state.clone(), bridge_actor, payload)
        .await
        .expect("handler ok");
    let reply: IngestInboundMailReply = decode(&reply_bytes).expect("decode reply");
    assert_eq!(reply.message_id.len(), 32);
    let message_id: [u8; 32] = reply
        .message_id
        .as_slice()
        .try_into()
        .expect("32-byte message_id");

    // 5. `query_bridge_imap_messages` (MDA's FETCH metadata path) —
    //    one INBOX row with the byte_length surfaced as ciphertext_size.
    let metas = state
        .db
        .query_bridge_imap_messages(&recipient, "INBOX", None, None, None, None)
        .await
        .expect("query_bridge_imap_messages");
    assert_eq!(metas.len(), 1, "one INBOX placement row");
    let meta_row = &metas[0];
    assert_eq!(meta_row.message_id, message_id);

    // 6. `segment_records` mirror — the routing pointer landed under the
    //    RECIPIENT's scope. Ownership is the scope check itself now: the
    //    scope-agnostic actor lookup was retired by the record-identity
    //    cutover (message-segment-store.md § Record identity per kind), so
    //    "points back at the right actor" is expressed as "the row resolves
    //    under this actor's (scope, kind, cid) and no other".
    let mirror = state
        .db
        .segment_records_lookup_record(&recipient, "mail", &meta_row.record_cid)
        .await
        .expect("segment_records_lookup_record")
        .expect("segment_records row present under the recipient's scope");
    assert_eq!(
        mirror.segment_id, meta_row.segment_id,
        "mirror row and IMAP metadata row name the same segment"
    );
    // RFC822.SIZE is no longer a SQL mirror column (the interim
    // `segment_records.byte_length` was dropped); the size is
    // resolved through the CARv2 index keyed by `record_cid`
    // (`segments::record_sizes`), exactly as the MDA FETCH path does
    // (`bridge_imap_handlers::query_bridge_imap_messages` size projection).
    let sizes = fauna_nest::segments::record_sizes(
        &state.mail_segments,
        &recipient,
        &[(meta_row.segment_id, meta_row.record_cid)],
    )
    .await
    .expect("record_sizes");
    assert!(
        matches!(sizes.as_slice(), [Some(s)] if *s > 0),
        "record size > 0 via the CARv2 index, got {sizes:?}",
    );

    // 7. `query_bridge_imap_index_segments` — index-hint path the MDA
    //    uses to drive client-side decryption / labelling.
    let idx_rows = state
        .db
        .query_bridge_imap_index_segments(&state.mail_segments, &recipient, None, 0, None)
        .await
        .expect("query_bridge_imap_index_segments");
    assert_eq!(idx_rows.len(), 1, "one index-segment row");
    let idx = &idx_rows[0];
    assert_eq!(idx.message_id, message_id);
    assert_eq!(idx.mailbox, "INBOX");
    assert_eq!(
        idx.encrypted_index_hint, hint,
        "index hint decoded from the on-disk envelope",
    );

    // 8. Envelope round-trip — `read_envelope` opens the segment file,
    //    streams out the framed record, and yields bytes that decode
    //    back to the original sealed pieces.
    let env_bytes = fauna_nest::segments::mail::read_envelope(
        &state.mail_segments,
        &state.db,
        &recipient,
        &message_id[..],
    )
    .await
    .expect("read_envelope")
    .expect("envelope present");
    let envelope =
        fauna_mail::segments::MailRecordEnvelope::decode(&env_bytes).expect("decode envelope");
    assert_eq!(envelope.encrypted_body, body);
    assert_eq!(envelope.encrypted_index_hint, hint);
}
