//! The in-process Nostr DM leg through the user's bridged-conversation kinds —
//! `fauna.bridges.conversation.{rooms.list,rooms.open,inbox.fetch,send}`
//! (`docs/goal/ui/nostr.md` § Implementation status today → DMs;
//! `docs/goal/ui/conversations.md` § Where logic lives → *The `Bridged`
//! adapter*). A Nostr DM is a bridged room under bridge id `nostr`; these are
//! the only kinds a client reads or sends one through.
//!
//! tier_3-ish (real handlers + real in-memory bridged-conversation and
//! `nostr_accounts` tables, no UI, no relays), through the same `dispatch()`
//! path the production router uses; a plain non-zero actor resolves to
//! `CallerClass::User`.
//!
//! Coverage:
//!   * `rooms.list` serves the caller's leg rooms under the leg's declared
//!     identity, and `inbox.fetch` their rows — never another account's.
//!   * `rooms.open` on the leg needs a linked Nostr account, and keys the
//!     room on the peer's canonical hex whichever way the `npub` names it.
//!   * `send` on a leg room stores the Sent row and queues the item; the leg's
//!     drain opens it under the leg's key, gift-wraps it with the custodial
//!     key and enqueues it, and a real nostr-sdk client verifies and opens the
//!     wrap. The Sent row rests sealed (the at-rest probe reads the stored
//!     bytes, not just the wire).
//!   * a send the leg could never deliver — the account emptied its relay
//!     list, or its key is not on the nest — is refused by name before
//!     anything is stored or queued, never silently defaulted or dropped.
//!
//! The nest never receives a DM's plaintext on this path — the client seals
//! both copies — so there is no nest-side "no seal key on file" refusal to
//! pin, as there was while a plaintext-taking send kind existed.
//!
//! Only compiled under `--features nostr` (the leg is feature-gated).

#![cfg(feature = "nostr")]

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;
use std::time::Duration;

use fauna_bridge_nostr::nip19::encode_npub;
use fauna_bridge_nostr::signing::Keypair;
use fauna_nest::bridged_conversation_handlers::register_bridged_conversation_handlers;
use fauna_nest::nostr;
use fauna_nest::nostr::db;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::NostrState;
use fauna_protocol::bridged_conversations::{
    BridgedMessageInfo, BridgedRoomInfo, InboxFetchReply, InboxFetchRequest, KIND_INBOX_FETCH,
    KIND_ROOMS_LIST, KIND_ROOMS_OPEN, KIND_SEND, RoomsListReply, RoomsListRequest, RoomsOpenReply,
    RoomsOpenRequest, SendReply, SendRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

/// Receiver for the outbound sync channel. Held alive by the harness so the
/// leg's drain can enqueue (the default `NostrState`'s receiver is dropped,
/// which would close the channel and leave every item queued).
type SyncRx = tokio::sync::mpsc::Receiver<nostr::sync_worker::OutboundEvent>;

/// The secret that opens what [`common::sealed_dm`] seals.
fn recipient_secret() -> [u8; 32] {
    fauna_mls::wrapped_blob::derive_recipient_hpke_keypair(&common::TEST_DM_MSEK).0
}

fn open_dm(sealed: &[u8]) -> String {
    String::from_utf8(
        fauna_mail::open_sealed_inner_record(sealed, &recipient_secret()).expect("opens"),
    )
    .expect("utf8")
}

/// A router carrying the family's kinds + an in-memory `AppState` whose
/// `nostr_*` tables are created and whose outbound sync receiver is returned.
async fn router_and_state() -> (RpcRouter, Arc<AppState>, SyncRx) {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    // The family asks every compiled-in leg whether it serves the account, so
    // a build with the other bridges needs their tables too — as a booted
    // nest of that flavor holds them.
    #[cfg(feature = "bluesky")]
    fauna_nest::bluesky::init_db(&db)
        .await
        .expect("init bluesky tables");
    #[cfg(feature = "activitypub")]
    fauna_nest::activitypub::init_db(&db)
        .await
        .expect("init activitypub tables");

    let mut state = AppState::for_test(db);
    let (relay_tx, _relay_rx) = tokio::sync::broadcast::channel(256);
    let (sync_tx, sync_rx) = tokio::sync::mpsc::channel(256);
    state.nostr = NostrState {
        relay_tx,
        sync_tx,
        gift_wrap_limiter: None,
        bunker_limiter: None,
        ..NostrState::default()
    };
    let state = Arc::new(state);
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;

    let mut b = RpcRouter::builder();
    register_bridged_conversation_handlers(&mut b);
    (b.build(), state, sync_rx)
}

/// Link a custodial Nostr account for `actor` — a generated keypair whose
/// secret is encrypted at rest with the nest's signing key, the shape the
/// generate/import enable flow writes and the leg's drain decrypts.
async fn link_custodial(state: &AppState, actor: [u8; 32], relay_list: Option<&str>) -> Keypair {
    let kp = Keypair::generate();
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let encrypted = encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).expect("encrypt privkey");
    let conn = state.db.conn().await;
    db::link_account(
        &conn,
        &hex::encode(actor),
        &kp.public_key_hex(),
        "generated",
        Some(&encrypted),
        None,
        relay_list,
    )
    .expect("link account");
    kp
}

/// Seed one inbound DM through the leg's own deposit, as the ingest does past
/// its gate.
async fn seed_inbound(state: &AppState, actor: [u8; 32], peer: &str, text: &str, at: i64) {
    fauna_nest::bridge_legs::deposit_sealed(
        &state.db,
        &fauna_nest::bridge_legs::NOSTR,
        &actor,
        &fauna_nest::bridge_legs::SealedDm {
            peer,
            sender: peer,
            self_address: "",
            far_message_id: &format!("{peer}:{at}"),
            sealed: common::sealed_dm(text).as_slice(),
            created_at_ms: at.saturating_mul(1000),
        },
    )
    .await
    .expect("seed dm");
}

async fn rooms(r: &RpcRouter, st: &Arc<AppState>, actor: [u8; 32]) -> Vec<BridgedRoomInfo> {
    let reply = dispatch(
        r,
        st.clone(),
        actor,
        KIND_ROOMS_LIST,
        encode(&RoomsListRequest::default()),
    )
    .await
    .expect("rooms.list ok");
    decode::<RoomsListReply>(&reply).expect("decode").rooms
}

async fn open(
    r: &RpcRouter,
    st: &Arc<AppState>,
    actor: [u8; 32],
    address: &str,
) -> Result<BridgedRoomInfo, RpcError> {
    let reply = dispatch(
        r,
        st.clone(),
        actor,
        KIND_ROOMS_OPEN,
        encode(&RoomsOpenRequest {
            bridge_id: Some("nostr".into()),
            address: address.into(),
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode::<RoomsOpenReply>(&reply).expect("decode").room)
}

async fn inbox(
    r: &RpcRouter,
    st: &Arc<AppState>,
    actor: [u8; 32],
    room_id: &[u8],
) -> Vec<BridgedMessageInfo> {
    let reply = dispatch(
        r,
        st.clone(),
        actor,
        KIND_INBOX_FETCH,
        encode(&InboxFetchRequest {
            room_id: Some(room_id.to_vec().into()),
            ..Default::default()
        }),
    )
    .await
    .expect("inbox.fetch ok");
    decode::<InboxFetchReply>(&reply).expect("decode").messages
}

/// Send `text` on `room` the way an app does: one copy sealed to the bridge's
/// key the room carries, one to the caller's own recipient key.
async fn send(
    r: &RpcRouter,
    st: &Arc<AppState>,
    actor: [u8; 32],
    room: &BridgedRoomInfo,
    text: &str,
) -> Result<SendReply, RpcError> {
    let leg_key = <[u8; 32]>::try_from(room.bridge_x25519.as_slice()).expect("32-byte leg key");
    let sealed_for_bridge = fauna_mls::wrapped_blob::seal_to_recipient(text.as_bytes(), &leg_key)
        .expect("seal for the leg")
        .to_canonical_bytes()
        .expect("canonical");
    let reply = dispatch(
        r,
        st.clone(),
        actor,
        KIND_SEND,
        encode(&SendRequest {
            room_id: room.room_id.clone(),
            sealed_for_bridge,
            sealed_for_self: common::sealed_dm(text).as_slice().to_vec(),
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode::<SendReply>(&reply).expect("decode"))
}

async fn queued_items(state: &AppState) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row("SELECT COUNT(*) FROM bridge_conversation_outbox", [], |r| {
        r.get(0)
    })
    .expect("count outbox")
}

/// Wait for the leg's drain (nudged by `send`) to have acked every item.
async fn drained(state: &AppState) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while queued_items(state).await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the leg to drain its outbox"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ── (a) the caller's leg rooms, and only the caller's ───────────────

#[tokio::test]
async fn the_legs_rooms_list_for_the_caller_only_under_its_declared_identity() {
    let (router, state, _rx) = router_and_state().await;
    let actor1: [u8; 32] = [0x42; 32];
    let actor2: [u8; 32] = [0x77; 32];
    link_custodial(&state, actor1, None).await;
    link_custodial(&state, actor2, None).await;
    let peer_a = hex::encode([0x0a; 32]);
    let peer_z = hex::encode([0x0f; 32]);

    seed_inbound(&state, actor1, &peer_a, "to actor1", 1_000).await;
    seed_inbound(&state, actor2, &peer_a, "to actor2", 9_000).await;
    seed_inbound(&state, actor2, &peer_z, "actor2 only", 9_500).await;

    let list = rooms(&router, &state, actor1).await;
    assert_eq!(list.len(), 1, "actor1 sees only its own room");
    let room = &list[0];
    assert_eq!(
        (
            room.bridge_id.as_str(),
            room.bridge_label.as_str(),
            room.glyph.as_str(),
            room.far_room_id.as_str(),
            room.disconnected,
        ),
        ("nostr", "Nostr", "bolt", peer_a.as_str(), false),
        "the leg's declared identity, the room keyed on the peer's hex"
    );

    let rows = inbox(&router, &state, actor1, &room.room_id).await;
    assert_eq!(rows.len(), 1, "never the other account's message");
    assert_eq!(open_dm(&rows[0].sealed_content), "to actor1");
    assert_eq!(
        (rows[0].outbound, rows[0].sender.as_str()),
        (false, &*peer_a)
    );
    // Seconds on the Nostr wire, the family's milliseconds here.
    assert_eq!(rows[0].created_at, 1_000_000);

    assert_eq!(rooms(&router, &state, actor2).await.len(), 2);
}

// ── (b) rooms.open: a linked account, and the peer's canonical key ──

#[tokio::test]
async fn rooms_open_on_the_leg_needs_a_linked_account_and_keys_on_the_canonical_peer() {
    let (router, state, _rx) = router_and_state().await;
    let actor: [u8; 32] = [0x42; 32];
    let peer = [0xAB; 32];
    let npub = encode_npub(&peer);

    // No Nostr account: the leg does not serve this account, so no bridge
    // accepts the address.
    let err = open(&router, &state, actor, &npub)
        .await
        .expect_err("the leg serves only a linked account");
    assert_eq!(err.code, "fauna.bridges.address_refused");

    link_custodial(&state, actor, None).await;
    let room = open(&router, &state, actor, &npub).await.expect("opens");
    assert_eq!(
        room.far_room_id,
        hex::encode(peer),
        "an npub opens the room its ingest and the gate key on"
    );
    // The same peer again is the same room, not a second one.
    let again = open(&router, &state, actor, &npub).await.expect("opens");
    assert_eq!(again.room_id, room.room_id);
    // The leg's grammar admits an npub only: a hex spelling opens nothing.
    let err = open(&router, &state, actor, &hex::encode(peer))
        .await
        .expect_err("hex is not an address the leg declares");
    assert_eq!(err.code, "fauna.bridges.address_refused");
}

// ── (c) send: Sent row + queued item → the drain gift-wraps and relays ──

#[tokio::test]
async fn send_on_a_leg_room_is_gift_wrapped_relayed_and_rests_sealed() {
    let (router, state, mut rx) = router_and_state().await;
    let actor: [u8; 32] = [0x42; 32];
    // No relay_list → the drain falls back to DEFAULT_RELAYS.
    let sender_kp = link_custodial(&state, actor, None).await;

    // A real nostr-sdk peer: its client must verify and open what the leg's
    // drain relays.
    let peer_keys = nostr_nips::prelude::Keys::generate();
    let peer_pubkey = peer_keys.public_key().to_hex();
    let peer_bytes: [u8; 32] = fauna_core::hex32::decode(&peer_pubkey).expect("32-byte hex");
    let room = open(&router, &state, actor, &encode_npub(&peer_bytes))
        .await
        .expect("opens");

    let reply = send(&router, &state, actor, &room, "secret hello")
        .await
        .expect("send ok");

    // The drain enqueued the gift wrap on the outbound sync channel…
    let outbound = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("the leg drains after a send")
        .expect("an outbound event was enqueued");
    assert_eq!(
        outbound.relay_urls,
        nostr::relays::DEFAULT_RELAYS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "no relay_list → DEFAULT_RELAYS"
    );
    // …and acked the item: nothing is left queued for the leg.
    drained(&state).await;

    // A real client: the wrap is a valid NIP-01 event, and the peer's own
    // nostr-sdk keys open it to the plaintext from the custodial sender.
    let wire = serde_json::to_string(&outbound.event).expect("event json");
    let event = nostr_nips::prelude::Event::from_json(&wire).expect("nostr-sdk parses");
    event.verify().expect("the gift wrap's signature verifies");
    let gift = nostr_nips::nips::nip59::UnwrappedGift::from_gift_wrap(&peer_keys, &event)
        .expect("the peer's real client opens the wrap");
    assert_eq!(gift.rumor.content, "secret hello");
    assert_eq!(gift.sender.to_hex(), sender_kp.public_key_hex());

    // The Sent row reads back through `inbox.fetch`.
    let rows = inbox(&router, &state, actor, &room.room_id).await;
    assert_eq!(rows.len(), 1, "the Sent row is stored");
    assert_eq!((rows[0].id, rows[0].outbound), (reply.id, true));
    assert_eq!(open_dm(&rows[0].sealed_content), "secret hello");

    // ── The at-rest probe: read the STORED bytes, not just the wire ──
    // (the vacuous-green trap — the sealed bytes must exist, must not embed
    // the plaintext, and must open to it.)
    let stored_sealed: Vec<u8> = {
        let conn = state.db.conn().await;
        conn.query_row(
            "SELECT sealed_content FROM bridge_conversation_messages WHERE actor_id = ?1",
            [&actor[..]],
            |row| row.get(0),
        )
        .expect("stored row")
    };
    assert!(
        !stored_sealed
            .windows(b"secret hello".len())
            .any(|w| w == b"secret hello"),
        "the stored sealed bytes must not embed the plaintext"
    );
    assert_eq!(open_dm(&stored_sealed), "secret hello");
}

// ── (d) a send the leg could never deliver is refused, nothing stored ──

async fn stored_rows(state: &AppState, actor: [u8; 32]) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(
        "SELECT COUNT(*) FROM bridge_conversation_messages WHERE actor_id = ?1",
        [&actor[..]],
        |row| row.get(0),
    )
    .expect("count")
}

#[tokio::test]
async fn send_with_an_explicitly_emptied_relay_list_is_refused_not_defaulted() {
    let (router, state, mut rx) = router_and_state().await;
    let actor: [u8; 32] = [0x43; 32];
    // The user removed every relay — an explicit `[]`, not NULL.
    link_custodial(&state, actor, Some("[]")).await;

    let peer = Keypair::generate().public_key_bytes();
    let room = open(&router, &state, actor, &encode_npub(&peer))
        .await
        .expect("opens");
    let err = send(&router, &state, actor, &room, "secret hello")
        .await
        .expect_err("an explicitly empty relay list must be refused, not silently defaulted");
    assert_eq!(err.code, "fauna.nostr.no_relays_configured");

    assert!(
        rx.try_recv().is_err(),
        "nothing must be enqueued to the sync worker on refusal"
    );
    assert_eq!(queued_items(&state).await, 0, "nothing was queued");
    assert_eq!(
        stored_rows(&state, actor).await,
        0,
        "no Sent row claims a message that never left"
    );
}

#[tokio::test]
async fn send_from_an_account_whose_key_is_not_on_the_nest_is_refused() {
    let (router, state, mut rx) = router_and_state().await;
    let actor: [u8; 32] = [0x44; 32];
    // A `remote` link: the nest holds the pubkey and no key to wrap with.
    {
        let conn = state.db.conn().await;
        db::link_account(
            &conn,
            &hex::encode(actor),
            &Keypair::generate().public_key_hex(),
            "remote",
            None,
            None,
            None,
        )
        .expect("link account");
    }

    let peer = Keypair::generate().public_key_bytes();
    let room = open(&router, &state, actor, &encode_npub(&peer))
        .await
        .expect("opens");
    let err = send(&router, &state, actor, &room, "secret hello")
        .await
        .expect_err("the leg cannot gift-wrap without the account's key");
    assert_eq!(err.code, "fauna.nostr.invalid_params");

    assert!(rx.try_recv().is_err(), "nothing was relayed");
    assert_eq!(queued_items(&state).await, 0, "nothing was queued");
    assert_eq!(
        stored_rows(&state, actor).await,
        0,
        "no Sent row was stored"
    );
}
