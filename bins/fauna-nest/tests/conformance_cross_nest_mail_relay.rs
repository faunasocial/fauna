//! **Slice 1 (public→private mail relay) — tier_3 capstone.** Two in-process
//! nests over the federation WS-RPC channel: a public relay nest `H` and a
//! paired private home nest `P`. Inbound (and Sent)
//! mail lands sealed in `H`'s `__mail`; `P` drains it over the new
//! `fauna.federation.sync.mail_pull` / `.mail_ack` channel kinds, appends the
//! verbatim sealed records to its own `__mail`, and acks — after which `H` holds
//! no readable copy. This is the wire-level proof of T1–T4 (the
//! `segments::mail`-level round-trip is the in-crate unit test
//! `segments::mail::tests::relay_round_trip_read_reseal_and_purge`).
//!
//! Carrier = channel only (Spec Y2 slice 5 retired the HTTP `nest-sync`
//! interim). Based on `conformance_federation_channel.rs::start_nest` — the
//! `for_test` router is empty, so the test must register the federation +
//! discovery handlers (the pool resolves a peer's `nest_id` from its URL via the
//! anon `fauna.nest.info` kind before dialing the channel).
//!
//! Goal: `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
//! § Inbound mail steps 4–6 + § Done definition.

#[cfg(not(target_os = "macos"))]
mod common;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_mail::segments::{MAIL_FLOOR_FORMAT_VERSION, MailFloorMetadata, MailRecordEnvelope};
use fauna_mls::wrapped_blob::{
    MailRecordEnvelope as SealedMailEnvelope, derive_recipient_hpke_keypair, seal_to_recipient,
};
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;

/// Spin a real in-process nest (its full router, incl. `/api/v1/federation/ws`)
/// on a loopback socket with a distinct nest identity. Copied verbatim from
/// `conformance_federation_channel.rs::start_nest`: `for_test`'s routers are
/// empty, so we register the federation handlers (the `mail_pull`/`mail_ack`
/// serving side) and the anon discovery handlers (peer `nest_id` resolution the
/// channel pool needs before dialing).
async fn start_nest() -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db)
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state)
}

/// Put `state` in the resolved NAT mode the private-side workers act under —
/// what the admin's `fauna.setup.nat_mode` commit sets on a real home box.
async fn go_private(state: &Arc<AppState>) {
    *state.node_mode.write().await = fauna_nest::config::NodeMode::Private;
}

/// Minimal `MailFloorMetadata` with only `received_at` meaningful (mirror of the
/// in-crate `segments::test_helpers::floor`, which is `pub(crate)` and so not
/// reachable from this integration test). `append_record` overwrites `seq`.
fn floor(received_at: i64) -> MailFloorMetadata {
    MailFloorMetadata {
        format_version: MAIL_FLOOR_FORMAT_VERSION,
        received_at,
        timestamp: received_at / 1000,
        ciphertext_size: 0,
        sender_domain: "example.com".to_string(),
        spam_disposition: "accept".to_string(),
        is_own_submission: false,
        spf: "pass".into(),
        dkim: "pass".into(),
        dmarc: "pass".into(),
        dmarc_policy: "reject".into(),
        arc: "pass".into(),
        spam_score: 0,
        seq: 0,
        continuation_role: fauna_mail::segments::CONTINUATION_ROLE_NORMAL,
        // Struct-update so the next additive floor field doesn't break this
        // fixture (the reason MailFloorMetadata carries a Default at all).
        ..Default::default()
    }
}

/// Seed one sealed mail record on `state`'s `__mail` for `actor` — the
/// public-relay-side `persist_decoded_inbound_mail` analogue, reduced to its
/// segment-store append (the relay carries already-sealed bytes; this test
/// seeds opaque "sealed" bytes since the public nest never opens them).
async fn seed_mail(state: &Arc<AppState>, actor: &[u8; 32], body: &str) -> [u8; 32] {
    fauna_nest::segments::mail::append_record(
        &state.mail_segments,
        &state.db,
        actor,
        // Opaque at-rest bytes — the public nest never opens them (S6.12b gate).
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            body.as_bytes().to_vec(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            format!("hint-{body}").into_bytes(),
        ),
        floor(1_715_000_000_000),
    )
    .await
    .expect("public-side inbound mail append")
    .cid
    .digest()
}

/// **The relay round-trip over the real channel.** `H` (public) holds three
/// sealed inbound records for `actor`; `P` (private) is paired with the
/// `mail_pull` capability. `P` pulls over `fauna.federation.sync.mail_pull`,
/// re-appends each verbatim, acks over `.mail_ack`. Assert: `P` serves the
/// verbatim sealed bytes (the MDA read path works on its own `__mail`), and `H`
/// holds no readable copy after the ack (the core user property, § Done
/// definition).
#[tokio::test]
async fn mail_relays_public_to_private_then_public_holds_no_readable_copy() {
    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let actor = [0x42u8; 32];
    let actor_hex = hex::encode(actor);

    // H pairs `actor` with P's verified nest, granting `mail_pull` — the relay
    // grant the `mail_pull_handler` gates on (keyed by the pulling nest's id,
    // which the channel verifies as `origin_nest_id`).
    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // Three sealed inbound records land on H (inbound + Sent share this path).
    // Their ids are the content-hash digests the appends derive.
    let bodies = ["sealed-one", "sealed-two", "sealed-three"];
    let mut rids: Vec<[u8; 32]> = Vec::new();
    for body in bodies {
        rids.push(seed_mail(&h_state, &actor, body).await);
    }

    // ── P drains H over the channel ──────────────────────────────────────────
    let reply = fauna_nest::federation_pool::originate_mail_pull(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &actor_hex,
        0,
    )
    .await
    .expect("channel mail_pull");
    assert_eq!(
        reply.records.len(),
        3,
        "all three records pull over the wire"
    );
    assert_eq!(
        reply.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "pull returns the public nest's per-actor monotonic seq, oldest first"
    );

    // P re-appends each verbatim (no re-seal — it forwards opaque sealed bytes).
    for rec in &reply.records {
        // Idempotency: the record is absent before the relay append. The wire
        // `record_id` is a 32-byte digest; the segment-store lookup keys on a
        // DAG-CBOR `Cid` (mirror `append_sealed_record`'s own conversion).
        let rid: [u8; 32] = rec
            .record_id
            .as_slice()
            .try_into()
            .expect("record_id is a 32-byte digest");
        let cid = fauna_cbor::Cid::from_digest_dag_cbor(rid);
        assert!(
            p_state
                .db
                .segment_records_lookup_record(&actor, "mail", &cid)
                .await
                .unwrap()
                .is_none(),
            "record absent on private nest before relay append"
        );
        let fwd_floor = MailFloorMetadata::decode(&rec.floor).expect("decode forwarded floor");
        let relayed = fauna_nest::segments::mail::append_sealed_record(
            &p_state.mail_segments,
            &p_state.db,
            &actor,
            &rec.envelope,
            fwd_floor,
        )
        .await
        .expect("private-side verbatim append");
        assert_eq!(
            relayed.cid, cid,
            "verbatim bytes re-derive the source's identity at the destination"
        );
    }
    p_state.mail_segments.flush(&actor).await.expect("flush P");

    // P serves the VERBATIM sealed envelope back (the MDA read path on its own
    // `__mail`); the decoded envelope is byte-identical to what H sealed.
    for (rid, body) in rids.iter().zip(bodies) {
        let got = fauna_nest::segments::mail::read_envelope(
            &p_state.mail_segments,
            &p_state.db,
            &actor,
            rid,
        )
        .await
        .expect("read private envelope")
        .expect("record present on private nest");
        let decoded = MailRecordEnvelope::decode(&got).expect("decode envelope");
        assert_eq!(
            decoded.encrypted_body,
            body.as_bytes(),
            "private nest holds the verbatim sealed body"
        );
    }

    // ── P acks → H tombstones + purges ───────────────────────────────────────
    let purged = fauna_nest::federation_pool::originate_mail_ack(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &actor_hex,
        reply.up_to,
    )
    .await
    .expect("channel mail_ack");
    assert_eq!(
        purged, 3,
        "the public nest tombstones all three acked records"
    );

    // The core user property: after ack the public relay holds no readable copy
    // (the relay cursor sees nothing; physical reclaim is the CompactionWorker's
    // job, covered by the segments-level retention test).
    let remaining = fauna_nest::segments::mail::read_after_seq(
        &h_state.mail_segments,
        &h_state.db,
        &actor,
        0,
        100,
    )
    .await
    .expect("read public nest after ack");
    assert!(
        remaining.is_empty(),
        "public relay nest holds no readable mail after ack + purge"
    );

    // A re-pull after the ack is empty too (idempotent drain — no duplicate
    // relay of already-acked mail).
    let repull = fauna_nest::federation_pool::originate_mail_pull(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &actor_hex,
        reply.up_to,
    )
    .await
    .expect("channel mail_pull re-poll");
    assert!(
        repull.records.is_empty(),
        "nothing left to pull after ack — the drain is idempotent"
    );
}

/// **The private-nest poll worker step (T5) end-to-end over the channel.**
/// Exercises the real `nest_sync_worker::relay_actor_mail` — the per-actor relay
/// the residential nest runs each cycle — not just the raw originators: pull →
/// dedup → verbatim append → contiguous ack, returning the advanced watermark.
/// A second call with the advanced cursor is a no-op (idempotent drain); a fresh
/// record then relays and advances the cursor again.
#[tokio::test]
async fn relay_actor_mail_worker_step_drains_dedups_and_advances_watermark() {
    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let actor = [0x33u8; 32];
    let actor_hex = hex::encode(actor);
    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // Two inbound records land on H.
    let rid_one = seed_mail(&h_state, &actor, "worker-one").await;
    let rid_two = seed_mail(&h_state, &actor, "worker-two").await;

    // Cycle 1: the worker drains both, acks, and advances the watermark to 2.
    let wm =
        fauna_nest::nest_sync_worker::relay_actor_mail(&p_state, &h_url, &actor, &actor_hex, 0)
            .await;
    assert_eq!(
        wm,
        Some(2),
        "worker relays both records and acks up to seq 2"
    );
    // Both verbatim on P.
    for rid in [rid_one, rid_two] {
        assert!(
            fauna_nest::segments::mail::read_envelope(
                &p_state.mail_segments,
                &p_state.db,
                &actor,
                &rid,
            )
            .await
            .unwrap()
            .is_some(),
            "relayed record present on private nest"
        );
    }
    // ...and PLACED in INBOX so the private nest's MDA serves them over IMAP —
    // not merely stored in `__mail`. The relay's `place_relayed_record` writes
    // the `bridge_imap_messages` placement the IMAP fetch path joins; without it
    // relayed mail is invisible to the MDA. `count_bridge_imap_mailbox` counts
    // through that exact join (`bridge_imap_messages ⋈ segment_records`). Both
    // records have a non-own-submission `accept` floor ⇒ INBOX.
    let (inbox_exists, _unseen) = p_state
        .db
        .count_bridge_imap_mailbox(&actor, "INBOX")
        .await
        .expect("count INBOX placements on the private nest");
    assert_eq!(
        inbox_exists, 2,
        "both relayed records are placed in INBOX (IMAP-visible), not just in __mail"
    );
    // H purged.
    assert!(
        fauna_nest::segments::mail::read_after_seq(
            &h_state.mail_segments,
            &h_state.db,
            &actor,
            0,
            100,
        )
        .await
        .unwrap()
        .is_empty(),
        "public nest purged after the worker acked"
    );

    // Cycle 2 with the advanced cursor: nothing new → no watermark advance.
    let wm2 =
        fauna_nest::nest_sync_worker::relay_actor_mail(&p_state, &h_url, &actor, &actor_hex, 2)
            .await;
    assert_eq!(wm2, None, "idempotent drain: no new mail, cursor unchanged");

    // A fresh record arrives; the worker relays it and advances to 3.
    let rid_three = seed_mail(&h_state, &actor, "worker-three").await;
    let wm3 =
        fauna_nest::nest_sync_worker::relay_actor_mail(&p_state, &h_url, &actor, &actor_hex, 2)
            .await;
    assert_eq!(wm3, Some(3), "new mail relays and advances the cursor");
    assert!(
        fauna_nest::segments::mail::read_envelope(
            &p_state.mail_segments,
            &p_state.db,
            &actor,
            &rid_three,
        )
        .await
        .unwrap()
        .is_some(),
        "the third record reached the private nest"
    );
}

/// **Capability gate over the wire.** A pairing without `mail_pull` (e.g. a
/// link that granted only the conv/namespace caps) cannot drain mail: the
/// `mail_pull_handler` returns `forbidden`, surfaced as a `PoolError`.
#[tokio::test]
async fn mail_pull_without_capability_is_forbidden() {
    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let actor = [0x77u8; 32];
    let actor_hex = hex::encode(actor);

    // Paired, but WITHOUT the `mail_pull` capability.
    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &["namespace_sync".to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let err = fauna_nest::federation_pool::originate_mail_pull(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &actor_hex,
        0,
    )
    .await
    .expect_err("a pairing lacking mail_pull cannot drain mail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("forbidden") || msg.contains("mail_pull"),
        "forbidden surfaces as a PoolError: {msg}"
    );
}

/// **The production worker cycle proves the two-row both-ends pairing model
/// (Slice 5), with the target taken from the row.** The other relay tests call
/// `relay_actor_mail`/`originate_mail_pull` directly with an explicit actor,
/// *bypassing* `run_sync_cycle`'s local-row gate — so the relay had never run
/// via its real entry point. This drives the actual cycle on a private nest
/// with no config-file pull target at all, and pins the home-with-public-relay
/// requirement that a working deploy needs a pairing row on **both** nests:
///
/// - On a nest whose resolved NAT mode is public the cycle does nothing.
/// - The **public** nest's pull-gate row alone (which authorizes `P` to pull)
///   does NOT make `P`'s worker fire: `P`'s own `nest_pairings` is empty, so
///   the cycle is `Processed(0)`.
/// - A local row on `P` that records no `nest_url` has no target: still
///   `Processed(0)`.
/// - Once the user links from `P` (a local pairing row on `P` carrying the
///   relay's `nest_url`, seeded by `fauna.pair.add` over the bearer connection
///   to `P`), the cycle is `Processed(1)` and relays the mail public→private
///   from that URL, after which `H` holds no readable copy.
#[tokio::test]
async fn worker_cycle_relays_only_after_both_pairing_rows_seeded() {
    use fauna_nest::nest_sync_worker::{SyncCycleOutcome, run_sync_cycle};

    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();
    let h_nest_id = h_state.nest_identity.public_key_bytes();

    let actor = [0x5au8; 32];

    // Public nest authorizes P to pull the actor's mail (the gate the
    // `mail_pull_handler` checks, keyed by P's verified nest id).
    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // Inbound mail lands sealed on H.
    let rid_c1 = seed_mail(&h_state, &actor, "cycle-one").await;
    let rid_c2 = seed_mail(&h_state, &actor, "cycle-two").await;

    let mut watermarks = std::collections::HashMap::new();
    let mut mail_watermarks = std::collections::HashMap::new();

    // ── A public nest's worker acts on nothing. ──
    assert_eq!(
        run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await,
        SyncCycleOutcome::NotPrivate
    );
    go_private(&p_state).await;

    // ── Cycle with ONLY the public-side row: P's pairing table is empty, so the
    // local-row gate finds nothing and the worker relays nothing. ──
    let outcome = run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await;
    assert_eq!(
        outcome,
        SyncCycleOutcome::Processed(0),
        "public-side authorization alone does not make the private worker fire"
    );
    assert!(
        fauna_nest::segments::mail::read_envelope(
            &p_state.mail_segments,
            &p_state.db,
            &actor,
            &rid_c1,
        )
        .await
        .unwrap()
        .is_none(),
        "no mail relayed while P has no local pairing row"
    );
    assert_eq!(
        fauna_nest::segments::mail::read_after_seq(
            &h_state.mail_segments,
            &h_state.db,
            &actor,
            0,
            100,
        )
        .await
        .unwrap()
        .len(),
        2,
        "both records still on the public nest (nothing pulled/acked)"
    );

    // ── A local row with no `nest_url` names no target: still nothing. ──
    p_state
        .db
        .store_pairing(
            &actor,
            &h_nest_id,
            &fauna_protocol::pair::default_self_sync(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await,
        SyncCycleOutcome::Processed(0),
        "a row with no nest_url has no target"
    );

    // ── The user links the public nest from P → a local pairing row on P
    // recording the relay's URL. The cycle relays from that URL. ──
    p_state
        .db
        .store_pairing(
            &actor,
            &h_nest_id,
            &fauna_protocol::pair::default_self_sync(),
            None,
            Some(&h_url),
            None,
        )
        .await
        .unwrap();

    let outcome = run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await;
    assert_eq!(
        outcome,
        SyncCycleOutcome::Processed(1),
        "one paired actor processed once P holds its own pairing row"
    );

    // Both records relayed verbatim to P via the real worker cycle.
    for rid in [rid_c1, rid_c2] {
        assert!(
            fauna_nest::segments::mail::read_envelope(
                &p_state.mail_segments,
                &p_state.db,
                &actor,
                &rid,
            )
            .await
            .unwrap()
            .is_some(),
            "record relayed to the private nest via the real worker cycle"
        );
    }
    // H purged after the worker acked — the no-readable-copy property.
    assert!(
        fauna_nest::segments::mail::read_after_seq(
            &h_state.mail_segments,
            &h_state.db,
            &actor,
            0,
            100,
        )
        .await
        .unwrap()
        .is_empty(),
        "public nest holds no readable copy after the worker cycle acked"
    );
}

/// **The relay purge removes the public box's IMAP placement, not just the
/// `__mail` segment (the no-readable/persistent-copy property, end-to-end on the
/// IMAP surface).** Slice 1's purge tombstoned the segment but left the
/// `bridge_imap_messages` placement row the MTA ingest created — so the public
/// relay box's MDA kept showing the message in INBOX (the `EXISTS` count reads
/// placement rows directly) even after the home box pulled + acked. The other
/// relay tests seed via `append_record` only (no placement), so they never
/// exercised this — exactly the gap tier_4 surfaced. This drives the real
/// `relay_actor_mail` against a public nest holding BOTH the segment AND an INBOX
/// placement (as a real MTA ingest produces) and asserts the placement is gone.
#[tokio::test]
async fn relay_purge_expunges_public_imap_placement() {
    use fauna_protocol::pair::capability::MAIL_PULL;

    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let actor = [0x6bu8; 32];
    let actor_hex = hex::encode(actor);
    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // Inbound lands on H the way the MTA produces it: a sealed `__mail` segment
    // AND an INBOX placement row (`place_inbound_mail`).
    let rid = seed_mail(&h_state, &actor, "placed-one").await;
    h_state
        .db
        .ensure_bridge_imap_mailboxes(&actor)
        .await
        .unwrap();
    h_state
        .db
        .place_inbound_mail(
            &actor,
            &rid,
            "INBOX",
            1_715_000_000,
            "",
            "example.com",
            true,
        )
        .await
        .unwrap();

    // Pre-relay: the public box's MDA shows the message in INBOX.
    assert_eq!(
        h_state
            .db
            .count_bridge_imap_mailbox(&actor, "INBOX")
            .await
            .unwrap()
            .0,
        1,
        "the inbound message is in the public box's INBOX before the relay"
    );

    // Relay: P pulls, stores, acks → H purges (segment tombstone + placement expunge).
    let wm =
        fauna_nest::nest_sync_worker::relay_actor_mail(&p_state, &h_url, &actor, &actor_hex, 0)
            .await;
    assert_eq!(wm, Some(1), "the record relays and acks");

    // Post-relay: the public relay box holds NO readable copy — the IMAP
    // placement is expunged, not merely the segment tombstoned.
    assert_eq!(
        h_state
            .db
            .count_bridge_imap_mailbox(&actor, "INBOX")
            .await
            .unwrap()
            .0,
        0,
        "the public relay box shows no message in INBOX after the relay purge"
    );
}

/// **A relay forwards the ORIGIN nest's receipt clock — never the sender's
/// `Date:` header.** The floor carries both facts: `received_at`, the
/// server-assigned instant the origin stamped, and `timestamp`, the sender's
/// own unauthenticated RFC 5322 `Date:` header. `message-segment-store.md`
/// § Invariants is why `received_at` survives the relay verbatim while
/// `stored_at` is overwritten locally, and `imap-server.md` § SEARCH is why it,
/// not the header, is what INTERNALDATE means.
///
/// The two are equal in every other fixture in this file, which is exactly why
/// this one sets them a decade apart: with them equal, a relay that forwarded
/// the wrong field would pass every existing assertion.
#[tokio::test]
async fn relayed_placement_keys_internaldate_on_the_origins_receipt_clock() {
    use fauna_protocol::pair::capability::MAIL_PULL;

    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let actor = [0x7cu8; 32];
    let actor_hex = hex::encode(actor);
    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // The origin received it at 1_715_000_000; the sender *claimed* a decade
    // earlier. Only one of the two may reach the private box's INBOX.
    const ORIGIN_RECEIPT_MS: i64 = 1_715_000_000_000;
    const SENDER_CLAIMED_DATE: i64 = 1_400_000_000;
    let mut origin_floor = floor(ORIGIN_RECEIPT_MS);
    origin_floor.timestamp = SENDER_CLAIMED_DATE;
    let rid = fauna_nest::segments::mail::append_record(
        &h_state.mail_segments,
        &h_state.db,
        &actor,
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"backdated-header".to_vec(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"hint-backdated-header".to_vec(),
        ),
        origin_floor,
    )
    .await
    .expect("public-side inbound mail append")
    .cid
    .digest();

    let wm =
        fauna_nest::nest_sync_worker::relay_actor_mail(&p_state, &h_url, &actor, &actor_hex, 0)
            .await;
    assert_eq!(wm, Some(1), "the record relays and acks");

    let placements = p_state
        .db
        .list_bridge_imap_messages(&actor, "INBOX")
        .await
        .unwrap();
    assert_eq!(placements.len(), 1, "the relayed record is placed in INBOX");
    assert_eq!(
        placements[0].2,
        ORIGIN_RECEIPT_MS / 1000,
        "the relayed placement's INTERNALDATE is the origin's receipt instant, \
         not the sender's claimed Date: header ({SENDER_CLAIMED_DATE})"
    );

    // And the sender's claim is not lost — it stays on the record, where a
    // client reads it as the message's own date.
    let (_env, relayed_floor) = fauna_nest::segments::mail::read_record_with_floor(
        &p_state.mail_segments,
        &p_state.db,
        &actor,
        &rid,
    )
    .await
    .unwrap()
    .expect("relayed record present on the private nest");
    assert_eq!(
        relayed_floor.timestamp, SENDER_CLAIMED_DATE,
        "the sender's own Date: header is preserved on the relayed record"
    );
    assert_eq!(
        relayed_floor.received_at, ORIGIN_RECEIPT_MS,
        "and the origin's receipt instant crosses the relay verbatim"
    );
}

/// Seed ONE **genuinely-sealed** inbound record on `state`'s `__mail` — the
/// MTA-ingest analogue for the Phase-3 tests below. Seals a real RFC-5322 body
/// (and a hint) to `pubkey` via `seal_to_recipient` (the exact Rust core the Go
/// MTA's `EncryptToRecipient` wraps), producing the encrypted-mode outer envelope
/// whose `encrypted_body` is the inner HPKE seal — exactly what a public
/// encrypted relay holds and forwards verbatim.
async fn seed_sealed_mail(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    pubkey: &[u8; 32],
    body: &[u8],
) -> [u8; 32] {
    let sealed = seal_to_recipient(body, pubkey)
        .expect("seal body")
        .to_canonical_bytes()
        .expect("encode sealed body");
    let sealed_hint = seal_to_recipient(b"index-hint", pubkey)
        .expect("seal hint")
        .to_canonical_bytes()
        .expect("encode sealed hint");
    fauna_nest::segments::mail::append_record(
        &state.mail_segments,
        &state.db,
        actor,
        // Genuine seals carried verbatim into the segment (S6.12b typed gate).
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(sealed),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(sealed_hint),
        floor(1_715_000_000_000),
    )
    .await
    .expect("public-side sealed inbound mail append")
    .cid
    .digest()
}

/// **Never lose the record.** A home box with no key material at all for this
/// actor stores the relayed record **verbatim (still sealed)**; the AUTH'd MDA
/// / the client opens it on read. Verbatim store is unconditional — there is
/// no storage-mode axis to gate it on any more
/// (`docs/goal/architecture/nest/storage-modes.md`), and no server-side
/// unseal-on-receipt path exists to fall back to (`actor_plaintext_msek`
/// doesn't exist as a custody concept at all).
#[tokio::test]
async fn home_without_any_key_material_stores_relayed_mail_sealed_verbatim() {
    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let actor = [0x52u8; 32];
    let actor_hex = hex::encode(actor);

    // No key material at all deposited on the home box for this actor.
    let msek = [0x88u8; 32];
    let (_secret, pubkey) = derive_recipient_hpke_keypair(&msek);

    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    const BODY: &[u8] = b"From: ext@x.test\r\nTo: a@home.test\r\nSubject: hi\r\n\r\nno key yet\r\n";
    let rid = seed_sealed_mail(&h_state, &actor, &pubkey, BODY).await;

    let wm =
        fauna_nest::nest_sync_worker::relay_actor_mail(&p_state, &h_url, &actor, &actor_hex, 0)
            .await;
    assert_eq!(wm, Some(1), "the record still relays (no loss)");

    // The stored record is the VERBATIM sealed inner envelope — not plaintext.
    let stored = fauna_nest::segments::mail::read_envelope(
        &p_state.mail_segments,
        &p_state.db,
        &actor,
        &rid,
    )
    .await
    .unwrap()
    .expect("the relayed record is present (never dropped)");
    let decoded = MailRecordEnvelope::decode(&stored).expect("decode outer envelope");
    assert_ne!(
        decoded.encrypted_body, BODY,
        "without the MSEK the body stays sealed, not plaintext"
    );
    assert!(
        SealedMailEnvelope::from_canonical_bytes(&decoded.encrypted_body).is_ok(),
        "the stored body is the verbatim inner HPKE seal (MDA opens it on read)"
    );
}

/// **The relay page is byte-budgeted** (`deployment-home-with-public-relay.md`
/// § Relay frame budget): the `mail_pull` reply must fit one 2 MiB WS frame,
/// so a page closes early BEFORE the record that would overflow — `up_to` is
/// the last record actually included, and the next pull resumes from there.
/// Three ~410 KB stored envelopes (~820 KB each on the wire): page 1 carries
/// exactly two, page 2 the third. No record is skipped and none is lost.
#[tokio::test]
async fn mail_pull_pages_close_early_on_the_frame_budget() {
    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let actor = [0x51u8; 32];
    let actor_hex = hex::encode(actor);
    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // A 420 KB opaque body stores as an ~860 KB v2 envelope on disk (seed_mail
    // mirrors the body into the hint, so the *stored* record is ~2× the body;
    // v2 storage payloads are byte strings, costing their own length plus a
    // small constant). The wire reply that carries it — `FedMailRecord`, whose
    // `Vec<u8>` fields are byte strings too (`serialization.md` § Canonical
    // IPLD dag-cbor, "Variable-length byte fields") — costs the stored size
    // plus framing: two ~860 KB records cost ~1.64 MiB on the wire, under the
    // ~1.94 MiB page budget; three cost ~2.46 MiB, over it.
    //
    // ⚠ Recalibrated twice. 2026-09-25: while `FedMailRecord`'s fields were
    // integer arrays (~2× their raw size on the wire), a later change
    // capped the federation dialer's inbound message size to the
    // same 2 MiB the budget targets, the budget learned the ~2× wire cost, and
    // this body shrank to 200 KB in step. 2026-10-01: the byte-string pass took
    // the ~2× back, so the body returns to 420 KB and "two fit, the third
    // doesn't" holds again — the record size is not what this test is about.
    // Kept under `MAIL_BODY_PART_CAP_BYTES` (1 MiB) so these stay single
    // records rather than continuation families.
    // Bodies must differ per record — identity is the content hash, so three
    // identical bodies would dedup into one record. One distinguishing suffix
    // byte keeps the page arithmetic intact.
    for i in 0..3u8 {
        let big_body = format!("{}{i}", "x".repeat(420 * 1024));
        seed_mail(&h_state, &actor, &big_body).await;
    }

    let page1 = fauna_nest::federation_pool::originate_mail_pull(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &actor_hex,
        0,
    )
    .await
    .expect("channel mail_pull page 1");
    assert_eq!(
        page1.records.len(),
        2,
        "the page closes early on the byte budget, not the 500-record count"
    );
    assert_eq!(
        page1.up_to, 2,
        "up_to is the last record actually included — the ack may never outrun the page"
    );

    let page2 = fauna_nest::federation_pool::originate_mail_pull(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &actor_hex,
        page1.up_to,
    )
    .await
    .expect("channel mail_pull page 2");
    assert_eq!(
        page2.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![3],
        "the next pull resumes exactly where the budgeted page closed — nothing skipped"
    );
}

/// **A single over-frame record freezes the pull instead of being skipped.**
/// The puller acks contiguously and the ack PURGES the source, so skipping a
/// record the cursor then passes would be irrecoverable mail loss. The honest
/// behavior is an empty page with `up_to` unmoved — the record behind the
/// over-frame one is deliberately NOT served.
///
/// **This is the generic page-budget bound every page builder has, and that is
/// exactly why it still needs a test.** Continuation writes mean a fresh append
/// can no longer *produce* an over-frame record — anything over
/// `MAIL_BODY_PART_CAP_BYTES` splits into frame-sized parts (that is rule 2 of the
/// relay frame budget). But an unvalidated write or a hostile peer can still put
/// one in the store, so a nest must keep meeting this shape safely. The seed
/// below therefore reproduces such a record deliberately, by turning the
/// continuation gate off for the append (test-only) — the only way this shape now
/// arises.
#[tokio::test]
async fn a_single_over_frame_record_freezes_the_pull_instead_of_skipping() {
    let (h_url, h_state) = start_nest().await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();

    let actor = [0x52u8; 32];
    let actor_hex = hex::encode(actor);
    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    // A body of exactly `MAIL_BODY_PART_CAP_BYTES` is not split (only a body
    // OVER the cap is), yet with the body mirrored into its hint it stores as one
    // ~2 MiB v2 envelope — over the relay frame budget
    // (`INLINE_MAIL_REQUEST_BUDGET_BYTES`) and well under the at-rest write
    // ceiling, so the append accepts it: exactly the stranded shape the frame
    // budget must not skip past.
    let over_frame_body =
        "y".repeat(fauna_mail::transport_limits::MAIL_BODY_PART_CAP_BYTES as usize);
    assert!(
        2 * over_frame_body.len()
            > fauna_mail::transport_limits::INLINE_MAIL_REQUEST_BUDGET_BYTES as usize,
        "the seeded record must be over the relay frame budget"
    );
    seed_mail(&h_state, &actor, &over_frame_body).await;
    seed_mail(&h_state, &actor, "small-after-big").await;

    let page = fauna_nest::federation_pool::originate_mail_pull(
        &p_state.federation_pool,
        &p_state,
        &h_url,
        &actor_hex,
        0,
    )
    .await
    .expect("channel mail_pull");
    assert!(
        page.records.is_empty(),
        "the over-frame head-of-line record freezes the page — never skipped"
    );
    assert_eq!(
        page.up_to, 0,
        "up_to stays at the cursor: an ack from this page can purge nothing"
    );
}

/// A public relay nest serving the self-signed floor TLS on `ip` — the shape of
/// the two-box docker witness's public box, whose pull target is its container
/// address on the docker bridge network (`https://<private-ip>:3000`), not a
/// loopback literal. Returns `(https_base, state)`.
#[cfg(not(target_os = "macos"))]
async fn start_tls_nest_on(ip: &str) -> (String, Arc<AppState>) {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec![ip.to_string()]).expect("floor cert");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let spki = fauna_nest::acme::spki_sha256_of_cert_der(cert_der.as_ref()).expect("leaf SPKI");
    let key_der: PrivateKeyDer<'static> =
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("server tls config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::from_seed(&secret)),
        // Floor-TLS channel binding: the hello signs the SPKI this listener serves.
        nest_signing_key: Some(SigningKey::from_bytes(&secret)),
        served_cert_spki: Some(Arc::new(common::FixedSpki(spki))),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(Arc::new(CacheDb::open_in_memory().unwrap()))
    });
    let listener = tokio::net::TcpListener::bind(format!("{ip}:0"))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(fauna_nest::serve_tls(
        listener,
        acceptor,
        fauna_nest::build_router(state.clone()).into_make_service(),
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));
    (format!("https://{addr}"), state)
}

/// **The address-guard exemption follows the admin, not the URL.** The
/// two-box docker witness pairs the home box with
/// `https://<public-container-ip>:3000` — an address on the docker bridge
/// network, as a real home box's relay may sit on a LAN or a VPN. The F7 SSRF
/// guard refuses every non-global https peer; the deployment's own topology is
/// exempt from that arm, and since the pull target
/// moved onto the pairing row the topology is what an **admin** of the private
/// nest records (`private-mode.md` § Pairing Flow, re-decided 2026-10-01). So:
/// a non-admin's row naming the private address is refused by the guard with
/// nothing pulled or acked, and the same row relays once its actor is an admin
/// — decided from the actor at the dial, never from a stored field.
///
/// `127.0.0.2` stands in for the private address: loopback, so reachable
/// in-process, but not the loopback literal the test carve-out admits, so it
/// meets the guard's global-address arm exactly as `172.18.0.x` does. macOS
/// does not configure `127.0.0.2`, so the test runs on Linux and Windows.
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn a_private_address_target_dials_only_for_an_admins_row() {
    use fauna_nest::nest_sync_worker::{SyncCycleOutcome, run_sync_cycle};

    let (h_url, h_state) = start_tls_nest_on("127.0.0.2").await;
    let (_p_url, p_state) = start_nest().await;
    let p_nest_id = p_state.nest_identity.public_key_bytes();
    let h_nest_id = h_state.nest_identity.public_key_bytes();
    let actor = [0x6bu8; 32];
    go_private(&p_state).await;

    h_state
        .db
        .store_pairing(
            &actor,
            &p_nest_id,
            &[fauna_protocol::pair::capability::MAIL_PULL.to_string()],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    p_state
        .db
        .store_pairing(
            &actor,
            &h_nest_id,
            &fauna_protocol::pair::default_self_sync(),
            None,
            Some(&h_url),
            None,
        )
        .await
        .unwrap();
    let rid = seed_mail(&h_state, &actor, "over-the-bridge-network").await;
    let pending_on_h = async || {
        fauna_nest::segments::mail::read_after_seq(
            &h_state.mail_segments,
            &h_state.db,
            &actor,
            0,
            100,
        )
        .await
        .unwrap()
        .len()
    };

    let mut watermarks = std::collections::HashMap::new();
    let mut mail_watermarks = std::collections::HashMap::new();

    // A non-admin's row: Supplied, so the SSRF guard refuses the private
    // address before anything is sent.
    let outcome = run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await;
    assert_eq!(outcome, SyncCycleOutcome::Processed(1));
    assert_eq!(
        p_state.federation_pool.peer_url_origin(&h_url),
        fauna_nest::federation_channel::PeerUrlOrigin::Supplied
    );
    assert_eq!(
        pending_on_h().await,
        1,
        "a non-admin's private URL stays refused (F7): nothing was pulled or acked"
    );
    assert!(
        mail_watermarks.is_empty(),
        "no cursor moved: the dial never happened"
    );

    // The same row once its actor is an admin of the private nest: the next
    // cycle reads the roster at the dial and relays.
    p_state.db.add_admin_actor(&actor).await.unwrap();
    run_sync_cycle(&p_state, &mut watermarks, &mut mail_watermarks).await;
    assert_eq!(
        p_state.federation_pool.peer_url_origin(&h_url),
        fauna_nest::federation_channel::PeerUrlOrigin::Configured
    );
    assert!(
        fauna_nest::segments::mail::read_envelope(
            &p_state.mail_segments,
            &p_state.db,
            &actor,
            &rid,
        )
        .await
        .unwrap()
        .is_some(),
        "the home box pulled the record from its admin's private-address relay"
    );
    assert_eq!(
        pending_on_h().await,
        0,
        "the relay holds no readable copy after the ack"
    );
}
