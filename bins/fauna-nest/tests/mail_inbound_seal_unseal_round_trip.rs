//! End-to-end inbound-mail **seal → store → fetch → unseal** round trip.
//!
//! The user directive (`green-test-or-it-doesnt-work`) is that a feature
//! is unproven until a green test asserts the *observable end result*,
//! not an intermediate ACK. For inbound receive the end result is
//! **received mail is decryptable** — but the existing `250`-on-ingest
//! test (`test_mail_bridge_mta.py::test_inbound_mx_round_trip`) only
//! proves the ciphertext was produced and stored, never that it opens.
//!
//! Coverage existed only in disjoint halves, none asserting the whole
//! flow against the real nest:
//!   * `fauna-mls` unit `derived_keypair_round_trips_through_mail_seal`:
//!     MSEK→derive→seal→open byte-equal, but purely in-process.
//!   * `mail_segment_round_trip::mta_ingest_to_mda_fetch_round_trip`:
//!     ingest→segment-store→read preserves `encrypted_body` bytes — but
//!     with a *plaintext placeholder* body, read via the low-level
//!     `read_envelope` (not the MDA-facing WS-RPC), and never decrypted.
//!   * Go `decrypt_e2e_test.go`: FFI seal→FetchCiphertext→open, against
//!     a *mocked* nest.
//!
//! This test joins them at the `encrypted_body` seam and proves the
//! production data flow against the *real* nest handlers + segment store:
//!
//!   external MX → `EncryptToRecipient` (= the exact Rust `seal_to_recipient`
//!   the Go MTA calls via FFI, to the MSEK-derived recipient pubkey)
//!   → real `ingest_inbound_mail` handler (BridgeMta) → real segment
//!   store (which re-wraps the bridge envelope) → real
//!   `fetch_message_ciphertext` handler (BridgeMda, which must unwrap to
//!   the inner bridge envelope) → `unseal_mail_record` with the
//!   MSEK-derived secret → byte-equal with the sent RFC 5322 body.
//!
//! The recipient key is derived from a client-held MSEK exactly as
//! `enable_mail` does (`libs/fauna-client-mail-settings/src/machine.rs`);
//! the nest only ever sees the *public* half, the secret stays test-side
//! standing in for the user's client (a nest-side auto-provision would be
//! invariant-forbidden — see `key-material-hierarchy.md:110`).

mod common;
use common::approve_bridge;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::wrapped_blob::{
    MailRecordEnvelope, derive_recipient_hpke_keypair, seal_to_recipient, unseal_mail_record,
};
use fauna_nest::bridge_imap_handlers::register_bridge_imap_handlers;
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    AuthVerdicts, DkimVerdict, DmarcVerdict, FetchMessageCiphertextReply,
    FetchMessageCiphertextRequest, IngestInboundMailReply, IngestInboundMailRequest,
    PublicMailMetadata, SpfVerdict,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};

/// Router with both handler sets registered: `ingest_inbound_mail`
/// (BridgeMta, the MTA write path) lives in `bridge_routing_handlers`;
/// `fetch_message_ciphertext` (BridgeMda, the MDA read path) lives in
/// `bridge_imap_handlers`. The round trip needs both.
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    register_bridge_imap_handlers(&mut b);
    (b.build(), state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, fauna_protocol::RpcError> {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

#[tokio::test]
async fn inbound_mail_seals_stores_and_unseals_through_real_handlers() {
    let (router, state) = router_and_state().await;

    // Recipient: derive the production recipient-mail keypair from a
    // client-held MSEK exactly as `enable_mail` does. The nest only ever
    // sees the public half; the secret stays here, standing in for the
    // user's client.
    let recipient: [u8; 32] = [0x42; 32];
    let msek: [u8; 32] = [0x5e; 32];
    let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&msek);

    // Register the recipient pubkey (ingest refuses an actor with no
    // `actor_mls_pubkeys` row — the "recipient has not provisioned an
    // MLS pubkey" precondition).
    common::seed_recipient_seal_key(&state.db, &recipient, &msek).await;

    // Two approved bridges: the MTA writes (ingest), the MDA reads (fetch).
    let mta_actor: [u8; 32] = [0x11; 32];
    let mda_actor: [u8; 32] = [0x22; 32];
    approve_bridge(&state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;
    approve_bridge(&state.db, &mda_actor, BridgeRole::Mda, &[0x92; 32]).await;

    // The perimeter seal. `EncryptToRecipient` is a thin Go wrapper over
    // this exact `seal_to_recipient` Rust core (via FFI), so sealing here
    // reproduces the production ciphertext byte-for-byte.
    let body: &[u8] = b"From: External Sender <sender@external.test>\r\n\
        To: alice@local.test\r\n\
        Subject: seal/unseal round-trip\r\n\
        \r\n\
        Hello from the inbound seal/unseal round-trip test.\r\n";
    let sealed_body = seal_to_recipient(body, &recipient_pubkey)
        .expect("seal body")
        .to_canonical_bytes()
        .expect("encode sealed body envelope");
    // The index hint is sealed under the same key; not asserted here, but
    // sent so the ingest payload matches the production wire shape.
    let sealed_hint = seal_to_recipient(b"index-hint", &recipient_pubkey)
        .expect("seal hint")
        .to_canonical_bytes()
        .expect("encode sealed hint envelope");

    // ── MTA ingest — wire shape exactly as a real external-MX delivery ──
    let ingest_req = IngestInboundMailRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: recipient.to_vec(),
        encrypted_body: sealed_body.clone(),
        encrypted_index_hint: sealed_hint,
        public_metadata: PublicMailMetadata {
            timestamp: 1_715_000_000,
            ciphertext_size: sealed_body.len() as u32,
            sender_domain: "external.test".into(),
        },
        verdicts: AuthVerdicts {
            dkim: DkimVerdict::Pass,
            spf: SpfVerdict::Pass,
            dmarc: DmarcVerdict::Pass,
            ..Default::default()
        },
        ..Default::default()
    };
    let ingest_payload = Bytes::from(
        encode_canonical(&ingest_req)
            .expect("encode ingest")
            .to_vec(),
    );
    let ingest_reply_bytes = dispatch(
        &router,
        state.clone(),
        mta_actor,
        "fauna.bridges.ingest_inbound_mail",
        ingest_payload,
    )
    .await
    .expect("MTA ingest must succeed");
    let ingest_reply: IngestInboundMailReply =
        decode(&ingest_reply_bytes).expect("decode ingest reply");
    assert_eq!(
        ingest_reply.message_id.len(),
        32,
        "server-assigned 32-byte id"
    );
    let message_id = ingest_reply.message_id;

    // ── MDA fetch — the real read surface a bridge uses to serve BODY[] ──
    let fetch_req = FetchMessageCiphertextRequest {
        actor_id: recipient.to_vec(),
        message_id: message_id.clone(),
    };
    let fetch_payload = Bytes::from(encode_canonical(&fetch_req).expect("encode fetch").to_vec());
    let fetch_reply_bytes = dispatch(
        &router,
        state.clone(),
        mda_actor,
        "fauna.bridges.fetch_message_ciphertext",
        fetch_payload,
    )
    .await
    .expect("MDA fetch must succeed");
    let fetch_reply: FetchMessageCiphertextReply =
        decode(&fetch_reply_bytes).expect("decode fetch reply");
    let fetched_body = match fetch_reply {
        FetchMessageCiphertextReply::Found {
            encrypted_body,
            ciphertext_size,
            ..
        } => {
            assert!(ciphertext_size > 0, "reported ciphertext_size is populated");
            encrypted_body
        }
        FetchMessageCiphertextReply::NotFound => {
            panic!("stored message must be fetchable by its owner")
        }
    };

    // The nest re-wraps the bridge envelope in a segment envelope on
    // store; `fetch_message_ciphertext` must hand back the *inner* bridge
    // envelope, verbatim — i.e. exactly what the MTA sealed.
    assert_eq!(
        fetched_body, sealed_body,
        "fetched ciphertext is the verbatim sealed bridge envelope (segment re-wrap unwrapped)"
    );

    // ── The assertion that actually proves receive works ──
    // The recipient opens the stored ciphertext with the MSEK-derived
    // secret and recovers the original RFC 5322 bytes.
    let envelope =
        MailRecordEnvelope::from_canonical_bytes(&fetched_body).expect("parse fetched envelope");
    let opened = unseal_mail_record(&envelope, &recipient_secret)
        .expect("recipient opens stored mail with the MSEK-derived secret");
    assert_eq!(
        opened.as_slice(),
        body,
        "received mail decrypts byte-for-byte back to the sent body"
    );

    // Negative control — gives the assertion above teeth. A different
    // MSEK's secret must NOT open it, otherwise the open could have
    // passed for a reason unrelated to correct targeting.
    let (wrong_secret, _) = derive_recipient_hpke_keypair(&[0xAB; 32]);
    assert!(
        unseal_mail_record(&envelope, &wrong_secret).is_err(),
        "a non-recipient secret must fail to open the stored ciphertext"
    );
}
