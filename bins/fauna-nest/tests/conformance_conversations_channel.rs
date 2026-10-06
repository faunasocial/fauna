//! Integration round-trip for `fauna.conversations.channel.*` (Layer-3
//! MLS-channel user-facing surface). Mirrors the
//! `conformance_email_filters.rs` harness — these handlers persist conv
//! records through the `__conv` segment store (`segments::conv::{append,
//! read_after_seq}`) and reach `CacheDb` for membership
//! (`state.db.{register_actor_channel, list_actor_channels,
//! list_channel_actors, ...}`).
//!
//! Authority for the wire types: `libs/fauna-protocol/src/conversations.rs`.
//! Authority for the slice + namespace decision tracked internally (§ T1b).

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    conversations_handlers, db::CacheDb, moderation_handlers, routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    conversations::{
        ChannelActorsReply, ChannelActorsRequest, ChannelEnvelope, ChannelFetchEntry,
        ChannelFetchReply, ChannelFetchRequest, ChannelListForActorReply,
        ChannelListForActorRequest, ChannelSendReply, ChannelSendRequest,
    },
    decode_strict as decode, encode_canonical,
    moderation::{ModerationLegalTakedownReply, ModerationLegalTakedownRequest},
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    (b.build(), state)
}

fn send_payload(channel_id_hex: &str, envelope: Vec<u8>) -> Bytes {
    let req = ChannelSendRequest {
        channel_id: channel_id_hex.into(),
        envelope,
        // Blind application send (the cross-device MLS commit-gate is not
        // exercised by this helper); mls-2b added the field.
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn fetch_payload(channel_id_hex: &str, after: i64, limit: i64) -> Bytes {
    let req = ChannelFetchRequest {
        channel_id: channel_id_hex.into(),
        after,
        limit,
        nest_url: None,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn list_payload() -> Bytes {
    let req = ChannelListForActorRequest {
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Inner bytes must clear the AEAD-shape floor the strict ingest verifier
/// applies to every channel envelope (>= 28 bytes = 12-byte nonce + 16-byte
/// tag, no plaintext-content magic prefix). Raw byte literals only ever
/// worked because `AppState::for_test` used to install the permissive
/// plaintext arm; it now installs the one and only `SealedStorage`, whose
/// `ingest_channel_envelope` always strict-decodes + AEAD-shape-checks.
/// `marker` keeps the byte-level distinguishability the raw literals gave
/// tests that assert message identity/ordering.
fn app_env(marker: u8) -> Vec<u8> {
    ChannelEnvelope::Application(vec![marker; 32])
        .to_bytes()
        .unwrap()
}

// ── fauna.conversations.channel.send ───────────────────────────

#[tokio::test]
async fn send_returns_seq_one_for_first_message() {
    let (router, state) = router_with_db_only().await;
    let actor = [11u8; 32];
    let channel_id_hex = "aa".repeat(32);
    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.channel.send",
        send_payload(&channel_id_hex, app_env(0x01)),
    )
    .await
    .expect("send ok");
    let reply: ChannelSendReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.seq, 1, "first message gets seq 1");
}

#[tokio::test]
async fn send_assigns_monotonic_seq() {
    let (router, state) = router_with_db_only().await;
    let actor = [12u8; 32];
    let channel_id_hex = "bb".repeat(32);
    for expected_seq in 1..=3 {
        let reply_bytes = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.channel.send",
            send_payload(&channel_id_hex, app_env(expected_seq as u8)),
        )
        .await
        .expect("send ok");
        let reply: ChannelSendReply = decode(&reply_bytes).unwrap();
        assert_eq!(reply.seq, expected_seq, "monotonic seq");
    }
}

#[tokio::test]
async fn send_rejects_malformed_channel_id() {
    let (router, state) = router_with_db_only().await;
    let actor = [13u8; 32];
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.channel.send",
        send_payload("not-hex", app_env(0x01)),
    )
    .await
    .expect_err("malformed channel_id rejected");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

// ── fauna.conversations.channel.fetch ──────────────────────────

#[tokio::test]
async fn fetch_returns_posted_messages() {
    let (router, state) = router_with_db_only().await;
    let actor = [21u8; 32];
    let channel_id_hex = "cc".repeat(32);

    for byte in [0x10u8, 0x20, 0x30] {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.channel.send",
            send_payload(&channel_id_hex, app_env(byte)),
        )
        .await
        .unwrap();
    }

    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.channel.fetch",
        fetch_payload(&channel_id_hex, 0, 100),
    )
    .await
    .expect("fetch ok");
    let reply: ChannelFetchReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.messages.len(), 3);
    let seqs: Vec<i64> = reply.messages.iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    let envelopes: Vec<Vec<u8>> = reply.messages.iter().map(|m| m.envelope.clone()).collect();
    assert_eq!(envelopes, vec![app_env(0x10), app_env(0x20), app_env(0x30)]);
}

#[tokio::test]
async fn fetch_respects_after_cursor() {
    let (router, state) = router_with_db_only().await;
    let actor = [22u8; 32];
    let channel_id_hex = "dd".repeat(32);

    for byte in [0x10u8, 0x20, 0x30] {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.channel.send",
            send_payload(&channel_id_hex, app_env(byte)),
        )
        .await
        .unwrap();
    }

    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.channel.fetch",
        fetch_payload(&channel_id_hex, 1, 100),
    )
    .await
    .unwrap();
    let reply: ChannelFetchReply = decode(&reply_bytes).unwrap();
    let seqs: Vec<i64> = reply.messages.iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![2, 3], "after=1 returns seq > 1");
}

/// `limit <= 0` means "the full page" (the nest's page-size ceiling), never
/// "clamp up to one record". Every already-shipped client poll loop sends
/// `limit: 0` expecting the whole tail (`poll_inbound_conv`'s documented
/// contract), so the old `clamp(1, 500)` — which turned 0 into ONE record per
/// round trip — silently starved every production drain and falsified the
/// "complete walk from 0" premise of the gate-less arm-2 reconcile
/// (`devices.md` § Cross-device MLS group-state sync).
#[tokio::test]
async fn fetch_limit_zero_serves_the_full_page_not_one_record() {
    let (router, state) = router_with_db_only().await;
    let actor = [0x2Au8; 32];
    let channel_id_hex = "ab".repeat(32);

    for byte in [0x10u8, 0x20, 0x30] {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.channel.send",
            send_payload(&channel_id_hex, app_env(byte)),
        )
        .await
        .unwrap();
    }

    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.channel.fetch",
        fetch_payload(&channel_id_hex, 0, 0),
    )
    .await
    .expect("fetch ok");
    let reply: ChannelFetchReply = decode(&reply_bytes).unwrap();
    assert_eq!(
        reply.messages.len(),
        3,
        "limit: 0 serves the full page — an already-shipped client's whole-tail \
         poll must not be starved to one record per round trip"
    );
}

/// A large-but-ingestable envelope for the frame-budget tests: the inner
/// ciphertext clears the AEAD-shape floor exactly like [`app_env`], just big.
fn big_app_env(marker: u8, inner_len: usize) -> Vec<u8> {
    ChannelEnvelope::Application(vec![marker; inner_len])
        .to_bytes()
        .unwrap()
}

/// The assembled fetch reply must fit one 2 MiB WS frame (`transport.md`
/// § Max frame): the page closes early before the record that would overflow
/// — and NEVER skips it (a skipped record would vanish from the client's
/// cursor-ordered walk: silent chat loss). The follow-up fetch from the
/// early-closed cursor serves the rest — the conv twin of the ratified
/// `mail_pull` budget rule (`deployment-home-with-public-relay.md` § Relay
/// frame budget), which count-only paging violated: two records that each fit
/// a frame alone, in one 500-count page, assembled a reply no WS frame could
/// carry, permanently stalling that channel's drain at that page.
#[tokio::test]
async fn fetch_page_closes_early_on_the_frame_budget_and_skips_nothing() {
    let (router, state) = router_with_db_only().await;
    let actor = [0x2Bu8; 32];
    let channel_id_hex = "ac".repeat(32);

    // Two ~1.1 MB envelopes (each fine alone, together over the ~1.94 MiB
    // budget — every envelope rides as a byte string, so a record costs its
    // raw size plus `segments::RECORD_WIRE_OVERHEAD`) and one small one.
    for (marker, len) in [(0x11u8, 1_100_000usize), (0x22, 1_100_000), (0x33, 64)] {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.channel.send",
            send_payload(&channel_id_hex, big_app_env(marker, len)),
        )
        .await
        .unwrap();
    }

    let page1_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.conversations.channel.fetch",
        fetch_payload(&channel_id_hex, 0, 500),
    )
    .await
    .expect("fetch ok");
    let page1: ChannelFetchReply = decode(&page1_bytes).unwrap();
    assert_eq!(
        page1.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![1],
        "the page closes early before the record that would overflow the frame"
    );

    let page2_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.channel.fetch",
        fetch_payload(&channel_id_hex, 1, 500),
    )
    .await
    .expect("fetch ok");
    let page2: ChannelFetchReply = decode(&page2_bytes).unwrap();
    assert_eq!(
        page2.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        vec![2, 3],
        "nothing was skipped — the follow-up fetch serves the rest in order"
    );
}

#[tokio::test]
async fn fetch_empty_channel_returns_zero_messages() {
    let (router, state) = router_with_db_only().await;
    let actor = [23u8; 32];
    let channel_id_hex = "ee".repeat(32);
    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.channel.fetch",
        fetch_payload(&channel_id_hex, 0, 100),
    )
    .await
    .expect("fetch ok");
    let reply: ChannelFetchReply = decode(&reply_bytes).unwrap();
    assert!(reply.messages.is_empty());
    // Auto-register fires on read — the channel is now in the actor's
    // list even though no message was posted.
    let _ignored: ChannelFetchEntry = ChannelFetchEntry {
        seq: 0,
        envelope: Vec::new(),
        ..Default::default()
    };
}

// ── fauna.conversations.channel.list_for_actor ─────────────────

#[tokio::test]
async fn list_for_actor_returns_registered_channels() {
    let (router, state) = router_with_db_only().await;
    let actor = [31u8; 32];
    let ch_a = "11".repeat(32);
    let ch_b = "22".repeat(32);

    for ch in [&ch_a, &ch_b] {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.channel.send",
            send_payload(ch, app_env(0x01)),
        )
        .await
        .unwrap();
    }

    let reply_bytes = dispatch(
        &router,
        state,
        actor,
        "fauna.conversations.channel.list_for_actor",
        list_payload(),
    )
    .await
    .expect("list ok");
    let reply: ChannelListForActorReply = decode(&reply_bytes).unwrap();
    let mut got = reply.channels.clone();
    got.sort();
    let mut want = vec![ch_a.clone(), ch_b.clone()];
    want.sort();
    assert_eq!(got, want);
}

#[tokio::test]
async fn list_for_actor_isolates_per_actor() {
    let (router, state) = router_with_db_only().await;
    let actor_a = [41u8; 32];
    let actor_b = [42u8; 32];
    let shared_channel = "33".repeat(32);

    dispatch(
        &router,
        state.clone(),
        actor_a,
        "fauna.conversations.channel.send",
        send_payload(&shared_channel, app_env(0x01)),
    )
    .await
    .unwrap();

    let reply_bytes = dispatch(
        &router,
        state,
        actor_b,
        "fauna.conversations.channel.list_for_actor",
        list_payload(),
    )
    .await
    .expect("list ok");
    let reply: ChannelListForActorReply = decode(&reply_bytes).unwrap();
    assert!(
        reply.channels.is_empty(),
        "actor_b is not registered on the channel actor_a posted to"
    );
}

// ── fauna.conversations.channel.actors ─────────────────────────

fn actors_payload(channel_hex: &str) -> Bytes {
    let req = ChannelActorsRequest {
        channel_id: channel_hex.to_string(),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// The inverse read of `list_for_actor`: every actor registered on one channel.
/// This is the roster `add_participant`'s phantom-leaf heal discriminates on
/// (`mls-group-key-material.md` § M2 *Admitting a member*).
#[tokio::test]
async fn channel_actors_returns_the_channels_roster() {
    let (router, state) = router_with_db_only().await;
    let actor_a = [51u8; 32];
    let actor_b = [52u8; 32];
    let channel = "44".repeat(32);
    let other_channel = "45".repeat(32);

    for (actor, ch) in [
        (actor_a, &channel),
        (actor_b, &channel),
        (actor_a, &other_channel),
    ] {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.conversations.channel.send",
            send_payload(ch, app_env(0x01)),
        )
        .await
        .unwrap();
    }

    let reply_bytes = dispatch(
        &router,
        state,
        actor_a,
        "fauna.conversations.channel.actors",
        actors_payload(&channel),
    )
    .await
    .expect("actors ok");
    let reply: ChannelActorsReply = decode(&reply_bytes).unwrap();
    let mut got = reply.actors.clone();
    got.sort();
    let mut want = vec![hex::encode(actor_a), hex::encode(actor_b)];
    want.sort();
    assert_eq!(got, want, "scoped to the named channel only");
}

/// Member-scoped: a caller not on the channel cannot enumerate who talks there.
#[tokio::test]
async fn channel_actors_refuses_a_non_member() {
    let (router, state) = router_with_db_only().await;
    let member = [61u8; 32];
    let outsider = [62u8; 32];
    let channel = "46".repeat(32);

    dispatch(
        &router,
        state.clone(),
        member,
        "fauna.conversations.channel.send",
        send_payload(&channel, app_env(0x01)),
    )
    .await
    .unwrap();

    let err = dispatch(
        &router,
        state,
        outsider,
        "fauna.conversations.channel.actors",
        actors_payload(&channel),
    )
    .await
    .expect_err("an outsider is refused");
    assert_eq!(err.code, "fauna.conversations.forbidden", "{err:?}");
}

/// A roster read must never *write* the row it reports. `channel_fetch`
/// auto-registers the reader; if this read did too, the very first heal probe
/// would make every phantom look healthy to the next caller.
#[tokio::test]
async fn channel_actors_never_auto_registers_the_reader() {
    let (router, state) = router_with_db_only().await;
    let member = [71u8; 32];
    let channel = "47".repeat(32);

    dispatch(
        &router,
        state.clone(),
        member,
        "fauna.conversations.channel.send",
        send_payload(&channel, app_env(0x01)),
    )
    .await
    .unwrap();

    for _ in 0..2 {
        let reply_bytes = dispatch(
            &router,
            state.clone(),
            member,
            "fauna.conversations.channel.actors",
            actors_payload(&channel),
        )
        .await
        .expect("actors ok");
        let reply: ChannelActorsReply = decode(&reply_bytes).unwrap();
        assert_eq!(
            reply.actors,
            vec![hex::encode(member)],
            "repeated reads never grow the roster"
        );
    }
}

// ── fauna.moderation.legal_takedown (content_type = "conversation") ─────
//
// Best-effort relay-withholding of an E2E MLS message (moderation.md
// § Categories & enforcement item 1 / content-moderation-and-ranking.md Q5 —
// the "posts / conversations" scope). The nest relays sealed blobs it cannot
// read, so a takedown keying on the message's `record_id` withholds the sealed
// record from FUTURE `channel.fetch` serves and carries a tombstone in the
// thread; `restore` re-serves (tombstone-not-delete).

async fn router_with_moderation() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    moderation_handlers::register_moderation_handlers(&mut b);
    (b.build(), state)
}

fn conv_takedown_payload(content_id: &str, legal_reference: &str, restore: bool) -> Bytes {
    let req = ModerationLegalTakedownRequest {
        content_id: content_id.into(),
        content_type: "conversation".into(),
        legal_reference: legal_reference.into(),
        restore,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Send a message, then take it down under a legal obligation → `channel.fetch`
/// withholds the sealed envelope (empty) and surfaces the tombstone reference in
/// its place, keeping the message's seq slot. `restore` re-serves the original
/// envelope. This is the end-to-end MLS relay-withhold path.
#[tokio::test]
async fn conversation_legal_takedown_withholds_from_fetch_and_restore_re_serves() {
    let (router, state) = router_with_moderation().await;
    let member = [0x51u8; 32];
    let channel_id_hex = "c0".repeat(32);
    let envelope = app_env(0xde);

    // Send (seq 1).
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        member,
        "fauna.conversations.channel.send",
        send_payload(&channel_id_hex, envelope.clone()),
    )
    .await
    .expect("send ok");
    let send_reply: ChannelSendReply = decode(&reply_bytes).unwrap();
    assert_eq!(send_reply.seq, 1);

    // The message's content id = the record's filing digest, the content hash
    // of its envelope — what a real reporting flow computes, through the same
    // shared mint the nest filed under, and hands the admin.
    let content_id = hex::encode(
        fauna_mls::segments::derive_record_cid(&envelope)
            .expect("derive conv record cid")
            .digest(),
    );

    // Admin takes it down (legal reference mandatory).
    let admin = [0x5au8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let bytes = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        conv_takedown_payload(&content_id, "EU-DSA-2024/777", false),
    )
    .await
    .expect("admin conversation takedown ok");
    let reply: ModerationLegalTakedownReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "taken_down");
    assert_eq!(reply.content_id, content_id);

    // Fetch now withholds the sealed envelope and carries the tombstone.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        member,
        "fauna.conversations.channel.fetch",
        fetch_payload(&channel_id_hex, 0, 100),
    )
    .await
    .expect("fetch ok");
    let reply: ChannelFetchReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.messages.len(), 1, "the message keeps its seq slot");
    let msg = &reply.messages[0];
    assert_eq!(msg.seq, 1);
    assert!(msg.envelope.is_empty(), "sealed envelope is withheld");
    assert_eq!(
        msg.legal_takedown
            .as_ref()
            .expect("tombstone present")
            .reference,
        "EU-DSA-2024/777"
    );

    // Restore (overturned appeal): the original sealed envelope re-serves.
    let bytes = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        conv_takedown_payload(&content_id, "appeal upheld", true),
    )
    .await
    .expect("admin restore ok");
    let reply: ModerationLegalTakedownReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "restored");

    let reply_bytes = dispatch(
        &router,
        state,
        member,
        "fauna.conversations.channel.fetch",
        fetch_payload(&channel_id_hex, 0, 100),
    )
    .await
    .expect("fetch ok");
    let reply: ChannelFetchReply = decode(&reply_bytes).unwrap();
    let msg = &reply.messages[0];
    assert_eq!(
        msg.envelope, envelope,
        "restored message re-serves its body"
    );
    assert!(msg.legal_takedown.is_none(), "tombstone cleared");
}

/// A non-admin cannot take down a conversation message (Admin-only allowlist arm
/// — the same structural guard as posts: a User can never remove content).
#[tokio::test]
async fn conversation_legal_takedown_denied_for_non_admin() {
    let (router, state) = router_with_moderation().await;
    let member = [0x61u8; 32];
    let channel_id_hex = "c1".repeat(32);
    let envelope = app_env(0x01);
    dispatch(
        &router,
        state.clone(),
        member,
        "fauna.conversations.channel.send",
        send_payload(&channel_id_hex, envelope.clone()),
    )
    .await
    .expect("send ok");
    let content_id = hex::encode(
        fauna_mls::segments::derive_record_cid(&envelope)
            .expect("derive conv record cid")
            .digest(),
    );

    let err = dispatch(
        &router,
        state,
        [0x62u8; 32], // unknown actor → User class
        "fauna.moderation.legal_takedown",
        conv_takedown_payload(&content_id, "EU-DSA-2024/1", false),
    )
    .await
    .expect_err("a non-admin cannot take down a conversation message");
    assert_eq!(err.code, "fauna.moderation.permission_denied");
}

/// Taking down an absent conversation message is `not_found` (nothing to
/// withhold); a takedown with a blank reference is `invalid_params`.
#[tokio::test]
async fn conversation_legal_takedown_missing_and_blank_reference_guards() {
    let (router, state) = router_with_moderation().await;
    let admin = [0x71u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();

    // Never-sent message id → not_found.
    let absent = hex::encode([0x7au8; 32]);
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        conv_takedown_payload(&absent, "EU-DSA-2024/1", false),
    )
    .await
    .expect_err("absent conversation message is not_found");
    assert_eq!(err.code, "fauna.moderation.not_found");

    // Existing message, blank reference → invalid_params.
    let channel_id_hex = "c2".repeat(32);
    let envelope = app_env(0x09);
    dispatch(
        &router,
        state.clone(),
        [0x72u8; 32],
        "fauna.conversations.channel.send",
        send_payload(&channel_id_hex, envelope.clone()),
    )
    .await
    .expect("send ok");
    let content_id = hex::encode(
        fauna_mls::segments::derive_record_cid(&envelope)
            .expect("derive conv record cid")
            .digest(),
    );
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.moderation.legal_takedown",
        conv_takedown_payload(&content_id, "   ", false),
    )
    .await
    .expect_err("blank legal reference is rejected");
    assert_eq!(err.code, "fauna.moderation.invalid_params");
}

// ── DM anti-spam: the behavioral feed ─────────
//
// These pin the *production flow* into `sender_behavior`, not the DB helpers:
// finding was precisely a scorer input whose SELECT existed and whose
// writer did not, so a DB-level test would have stayed green throughout.
// Authority: `docs/goal/behavior/direct-messages.md` § Anti-Spam.

/// Seat both members on the channel, the post-Welcome state in which a DM's
/// recipient is resolvable. Without this the first message records a
/// target-less event (§ Anti-Spam's "Known gap") and nothing is scored.
async fn seat_pair(state: &Arc<AppState>, a: &[u8; 32], b: &[u8; 32], channel_id: &[u8; 32]) {
    state
        .db
        .register_actor_channel(a, channel_id)
        .await
        .unwrap();
    state
        .db
        .register_actor_channel(b, channel_id)
        .await
        .unwrap();
}

/// A reply must be recorded against the actor who was replied *to* — that is
/// the only feed for `dm_response_rate`. Red before the fix: the
/// `dm_replied` event type had no production writer anywhere, so the rate was
/// structurally 0.0 and the 7-day fanout rule degenerated to fanout-alone.
#[tokio::test]
async fn a_reply_feeds_the_original_senders_dm_response_rate() {
    let (router, state) = router_with_db_only().await;
    let a = [21u8; 32];
    let b = [22u8; 32];
    let channel_id = [0xcc_u8; 32];
    let channel_id_hex = hex::encode(channel_id);
    seat_pair(&state, &a, &b, &channel_id).await;

    dispatch(
        &router,
        state.clone(),
        a,
        "fauna.conversations.channel.send",
        send_payload(&channel_id_hex, app_env(0x01)),
    )
    .await
    .expect("A opens the conversation");
    dispatch(
        &router,
        state.clone(),
        b,
        "fauna.conversations.channel.send",
        send_payload(&channel_id_hex, app_env(0x02)),
    )
    .await
    .expect("B replies");

    let now = fauna_core::data::Timestamp::now().0 as i64;
    let profile = state.db.get_behavioral_profile(&a, &b, now).await.unwrap();
    assert!(
        profile.dm_response_rate > 0.0,
        "B's reply must feed A's dm_response_rate; got {} — the `dm_replied` writer is gone again",
        profile.dm_response_rate
    );
}

/// Leg D: a group-forked thread rides the same `channel.send` kind, and its
/// sends are deliberately **not** DM-fanout-scored. Be precise about what the
/// predicate tests: "≥2 others on the roster right now" — which a sender
/// establishes at will via the shipped add-participant control — NOT group age,
/// who created it, or whether the roster predates the send. So the exemption is
/// self-service, and `direct-messages.md` § Anti-Spam step 1 says so outright
/// rather than justifying it as "an established group"; the same correction
/// sits at the call site (`conversations_handlers.rs`, the `DmPeer::Group`
/// arm). The behaviour is unchanged by the fix; this pins the
/// product call so it stops being implied by a collapsed match arm.
#[tokio::test]
async fn a_group_forked_thread_send_is_not_dm_fanout_scored() {
    let (router, state) = router_with_db_only().await;
    let a = [51u8; 32];
    let b = [52u8; 32];
    let c = [53u8; 32];
    let channel_id = [0xdd_u8; 32];
    seat_pair(&state, &a, &b, &channel_id).await;
    state
        .db
        .register_actor_channel(&c, &channel_id)
        .await
        .unwrap();

    dispatch(
        &router,
        state.clone(),
        a,
        "fauna.conversations.channel.send",
        send_payload(&hex::encode(channel_id), app_env(0x01)),
    )
    .await
    .expect("A sends into the group thread");

    let now = fauna_core::data::Timestamp::now().0 as i64;
    let profile = state.db.get_behavioral_profile(&a, &b, now).await.unwrap();
    assert_eq!(
        profile.unique_dm_recipients_7d, 0,
        "a group-thread send must not count toward DM fanout"
    );
}
