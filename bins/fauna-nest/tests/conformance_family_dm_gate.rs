//! The family **bridge-DM gate** end-to-end (`family-safety.md` § The bridge-DM
//! gate, design firmed 2026-07-16) — the sibling of
//! `conformance_family_mail_gate.rs`, for the reach pillar's other non-actor
//! ingress class.
//!
//! tier_3: real handlers, real in-memory bridged-conversation + `guardian_*` tables, real
//! `fauna.bridges.conversation.{rooms.list,rooms.open,inbox.fetch,send}` and
//! `fauna.family.approvals.{list,decide}` dispatch, on the in-process Nostr
//! leg's rooms. No UI, no relays.
//!
//! **What these pin that the unit tests cannot.** `fauna_core::data`'s ten
//! `supervised_dm_verdict` arms prove the *decision*; the `db::family` arms prove
//! the *store*. These prove the decision is actually wired to the surfaces a user
//! touches — the queue the guardian reads, the marker the ward's client renders,
//! the send the ward makes — which is where a correct primitive installed on the
//! wrong path (the 2026-07-09 reach review's bypass class) would still show
//! green everywhere else.
//!
//! The defining property under test is **computed placement**: no hold state is
//! stored anywhere, so hold-ness is a function of `(knob, verdict row)` evaluated
//! at read. Every assertion here that a knob flip changes a marker with no
//! migration step is a load-bearing pin on that (§ Don't do these — *"don't store
//! bridge-DM hold state"*).
//!
//! Only compiled under `--features nostr` (the Nostr leg is feature-gated) —
//! the bridge these journeys ride; the gate itself is bridge-generic.

#![cfg(feature = "nostr")]

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;
use std::time::Duration;

use fauna_bridge_nostr::nip19::encode_npub;
use fauna_bridge_nostr::signing::Keypair;
use fauna_nest::bridged_conversation_handlers::register_bridged_conversation_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::family_handlers::register_family_handlers;
use fauna_nest::nostr;
use fauna_nest::nostr::db;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::NostrState;
use fauna_protocol::bridged_conversations::{
    BridgedRoomInfo, InboxFetchReply, InboxFetchRequest, KIND_INBOX_FETCH, KIND_ROOMS_LIST,
    KIND_ROOMS_OPEN, KIND_SEND, RoomsListReply, RoomsListRequest, RoomsOpenReply, RoomsOpenRequest,
    SendReply, SendRequest,
};
use fauna_protocol::family::{
    FamilyApprovalDecideRequest, FamilyApprovalsListReply, FamilyApprovalsListRequest,
    FamilyOkReply,
};
use fauna_protocol::{RpcError, decode_strict as decode};
use serde_bytes::ByteBuf;

type SyncRx = tokio::sync::mpsc::Receiver<nostr::sync_worker::OutboundEvent>;

const GUARDIAN: [u8; 32] = [0xA1; 32];
const WARD: [u8; 32] = [0xA2; 32];

/// A router carrying both surfaces the gate spans — the bridged-conversation
/// kinds the ward uses and the family kinds the guardian uses — over one
/// supervised ward.
async fn harness() -> (RpcRouter, Arc<AppState>, SyncRx) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
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
    db.create_user_with_handle(&GUARDIAN, "personal", "parent", None)
        .await
        .unwrap();
    db.create_user_with_handle(&WARD, "personal", "kid", Some(&GUARDIAN[..]))
        .await
        .unwrap();

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
    register_family_handlers(&mut b);
    (b.build(), state, sync_rx)
}

/// Set the ward's `unknown_peer_dm` knob through the shared policy writer.
async fn set_knob(state: &AppState, value: &str) {
    state
        .db
        .update_guardian_policy(
            &WARD[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            Some(value),
            None,
        )
        .await
        .unwrap();
}

/// Seed an inbound DM from `peer` — the row the ingest would write, through
/// the same leg deposit it calls past its gate. Depositing it directly is the
/// honest setup: the ingest's own gate is pinned in `sync_worker`'s in-crate
/// tests, and this suite is about what the *read* and *queue* surfaces then
/// compute from a stored conversation.
async fn seed_inbound(state: &AppState, peer: &str, text: &str, at: i64) {
    fauna_nest::bridge_legs::deposit_sealed(
        &state.db,
        &fauna_nest::bridge_legs::NOSTR,
        &WARD,
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

/// The ward's room with `peer` on the Nostr leg, as `rooms.list` serves it.
async fn ward_room(r: &RpcRouter, st: &Arc<AppState>, peer: &str) -> BridgedRoomInfo {
    let reply: RoomsListReply = decode(
        &dispatch(
            r,
            st.clone(),
            WARD,
            KIND_ROOMS_LIST,
            encode(&RoomsListRequest::default()),
        )
        .await
        .expect("rooms.list ok"),
    )
    .expect("decode rooms.list");
    reply
        .rooms
        .into_iter()
        .find(|room| room.bridge_id == "nostr" && room.far_room_id == peer)
        .expect("the conversation is listed")
}

/// The ward's view of one conversation's guardian marker.
async fn ward_marker(r: &RpcRouter, st: &Arc<AppState>, peer: &str) -> Option<String> {
    ward_room(r, st, peer).await.guardian_state
}

/// The ward reading one conversation: its rows and the reply-level marker.
async fn ward_inbox(r: &RpcRouter, st: &Arc<AppState>, peer: &str) -> InboxFetchReply {
    let room = ward_room(r, st, peer).await;
    decode(
        &dispatch(
            r,
            st.clone(),
            WARD,
            KIND_INBOX_FETCH,
            encode(&InboxFetchRequest {
                room_id: Some(room.room_id.into()),
                ..Default::default()
            }),
        )
        .await
        .expect("inbox.fetch ok"),
    )
    .expect("decode inbox.fetch")
}

/// Give the ward a custodial Nostr account, so the leg serves it: a room can
/// be opened and a send relayed.
async fn link_ward(st: &AppState) {
    let kp = Keypair::generate();
    let nest_key = st.nest_identity.signing_key.to_bytes();
    let encrypted = encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).unwrap();
    let conn = st.db.conn().await;
    db::link_account(
        &conn,
        &hex::encode(WARD),
        &kp.public_key_hex(),
        "generated",
        Some(&encrypted),
        None,
        None,
    )
    .unwrap();
}

/// The ward opening its room with `address` on the Nostr leg.
async fn ward_open(
    r: &RpcRouter,
    st: &Arc<AppState>,
    address: &str,
) -> Result<BridgedRoomInfo, RpcError> {
    let reply = dispatch(
        r,
        st.clone(),
        WARD,
        KIND_ROOMS_OPEN,
        encode(&RoomsOpenRequest {
            bridge_id: Some("nostr".into()),
            address: address.into(),
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode::<RoomsOpenReply>(&reply)
        .expect("decode rooms.open")
        .room)
}

/// The ward sending `text` on `room` the way an app does: one copy sealed to
/// the bridge's key the room carries, one to the ward's own recipient key.
async fn ward_send(
    r: &RpcRouter,
    st: &Arc<AppState>,
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
        WARD,
        KIND_SEND,
        encode(&SendRequest {
            room_id: room.room_id.clone(),
            sealed_for_bridge,
            sealed_for_self: common::sealed_dm(text).as_slice().to_vec(),
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode::<SendReply>(&reply).expect("decode send"))
}

/// The guardian's queue, `dm_hold` entries only.
async fn dm_holds(r: &RpcRouter, st: &Arc<AppState>) -> Vec<(String, String)> {
    let reply: FamilyApprovalsListReply = decode(
        &dispatch(
            r,
            st.clone(),
            GUARDIAN,
            "fauna.family.approvals.list",
            encode(&FamilyApprovalsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("approvals ok"),
    )
    .expect("decode approvals");
    reply
        .approvals
        .into_iter()
        .filter(|a| a.kind == "dm_hold")
        .map(|a| (a.bridge_id, a.peer_address))
        .collect()
}

async fn decide(r: &RpcRouter, st: &Arc<AppState>, peer: &str, approve: bool) {
    let reply: FamilyOkReply = decode(
        &dispatch(
            r,
            st.clone(),
            GUARDIAN,
            "fauna.family.approvals.decide",
            encode(&FamilyApprovalDecideRequest {
                supervised_actor_id: ByteBuf::from(WARD.to_vec()),
                kind: "dm_hold".into(),
                bridge_id: "nostr".into(),
                peer_address: peer.into(),
                approve,
                ..Default::default()
            }),
        )
        .await
        .expect("decide ok"),
    )
    .expect("decode decide");
    assert!(reply.ok);
}

/// The journey (§ The bridge-DM gate): a cold peer's conversation is marked held
/// and queued for the guardian; approve releases it; the release is permanent.
#[tokio::test]
async fn a_cold_peer_is_held_and_queued_then_approve_releases_it() {
    let (r, st, _rx) = harness().await;
    let peer = hex::encode([0x11u8; 32]);
    set_knob(&st, "hold").await;
    seed_inbound(&st, &peer, "hi kid", 100).await;

    // The ward's client renders it held…
    assert_eq!(ward_marker(&r, &st, &peer).await.as_deref(), Some("held"));
    // …and the guardian sees exactly one queue entry, keyed on the peer.
    assert_eq!(
        dm_holds(&r, &st).await,
        vec![("nostr".into(), peer.clone())]
    );

    // Reading is never gated — the ward can open the held conversation
    // (§ The trust shape invariant 4).
    let msgs = ward_inbox(&r, &st, &peer).await;
    assert_eq!(msgs.messages.len(), 1, "a held DM is readable by the ward");
    assert_eq!(msgs.guardian_state.as_deref(), Some("held"));

    decide(&r, &st, &peer, true).await;

    // Released: no marker, and the entry has left the queue.
    assert_eq!(ward_marker(&r, &st, &peer).await, None);
    assert!(dm_holds(&r, &st).await.is_empty());

    // A later DM from the same peer now delivers unmarked — the approve is a
    // standing verdict, not a one-message pass.
    seed_inbound(&st, &peer, "hi again", 200).await;
    assert_eq!(ward_marker(&r, &st, &peer).await, None);
}

/// **Deny** blocks new arrivals but never destroys stored rows — refusing new
/// arrivals is the RCPT-reject analogue (§ The bridge-DM gate).
#[tokio::test]
async fn deny_marks_blocked_and_leaves_already_stored_messages_readable() {
    let (r, st, _rx) = harness().await;
    let peer = hex::encode([0x22u8; 32]);
    set_knob(&st, "hold").await;
    seed_inbound(&st, &peer, "already here", 100).await;

    decide(&r, &st, &peer, false).await;

    assert_eq!(
        ward_marker(&r, &st, &peer).await.as_deref(),
        Some("blocked")
    );
    // The stored message is still the ward's to read — the deny refuses the
    // FUTURE, it does not destroy the past (no user-data loss).
    let msgs = ward_inbox(&r, &st, &peer).await;
    assert_eq!(
        msgs.messages.len(),
        1,
        "a denied peer's already-stored messages stay readable"
    );
    assert_eq!(msgs.guardian_state.as_deref(), Some("blocked"));

    // A blocked peer is no longer *held*, so it leaves the guardian's queue —
    // it is decided, not pending.
    assert!(dm_holds(&r, &st).await.is_empty());
}

/// **Relaxing the knob releases everything by construction** — the property that
/// lets `dm_hold` have no drainage rule (§ Reach approvals). No migration, no
/// drain step: the same stored rows simply compute differently.
#[tokio::test]
async fn relaxing_the_knob_releases_every_held_conversation_with_no_drain_step() {
    let (r, st, _rx) = harness().await;
    let a = hex::encode([0x31u8; 32]);
    let b = hex::encode([0x32u8; 32]);
    set_knob(&st, "hold").await;
    seed_inbound(&st, &a, "one", 100).await;
    seed_inbound(&st, &b, "two", 101).await;
    assert_eq!(dm_holds(&r, &st).await.len(), 2);

    set_knob(&st, "allow").await;

    assert_eq!(ward_marker(&r, &st, &a).await, None);
    assert_eq!(ward_marker(&r, &st, &b).await, None);
    assert!(
        dm_holds(&r, &st).await.is_empty(),
        "with the knob relaxed nothing is held — no drainage rule needed"
    );

    // …and a guardian's explicit BLOCK is not a hold: it survives the relax,
    // because relaxing means "stop reviewing cold peers", never "unblock the
    // peers I decided against".
    let c = hex::encode([0x33u8; 32]);
    seed_inbound(&st, &c, "three", 102).await;
    decide(&r, &st, &c, false).await;
    assert_eq!(ward_marker(&r, &st, &c).await.as_deref(), Some("blocked"));
}

/// A **`dm_hold` entry only surfaces while the knob holds** (§ Reach approvals's
/// per-kind gating), and an unsupervised account is never marked at all.
#[tokio::test]
async fn the_queue_gates_on_the_knob_and_an_unsupervised_account_is_never_marked() {
    let (r, st, _rx) = harness().await;
    let peer = hex::encode([0x44u8; 32]);
    // Default knob = allow (the unsupervised-equivalent): nothing held.
    seed_inbound(&st, &peer, "hello", 100).await;
    assert_eq!(ward_marker(&r, &st, &peer).await, None);
    assert!(dm_holds(&r, &st).await.is_empty());

    // Graduation drops the link — and with it every marker, by construction.
    set_knob(&st, "hold").await;
    assert_eq!(ward_marker(&r, &st, &peer).await.as_deref(), Some("held"));
    st.db.graduate(&WARD[..]).await.unwrap();
    assert_eq!(
        ward_marker(&r, &st, &peer).await,
        None,
        "a graduated account has no gate — the policy read short-circuits"
    );
}

/// **Outbound seeds `allow`** (`family-safety.md` § The bridge-DM gate — the
/// mail allowlist's auto-seed in DM form): the child chose the correspondent, so
/// their replies always flow instead of piling into the guardian's queue.
#[tokio::test]
async fn the_wards_own_send_seeds_the_peer_so_replies_flow() {
    let (r, st, mut rx) = harness().await;
    set_knob(&st, "hold").await;
    link_ward(&st).await;

    // A real curve point: `wrap_dm` encrypts the rumor TO this key, so an
    // arbitrary 32 bytes is not a usable peer (only ~half are valid x-only
    // pubkeys).
    let peer_kp = Keypair::generate();
    let peer = peer_kp.public_key_hex();
    let room = ward_open(&r, &st, &encode_npub(&peer_kp.public_key_bytes()))
        .await
        .expect("open ok");
    ward_send(&r, &st, &room, "hey, it's me")
        .await
        .expect("send ok");
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("the leg drains after a send")
        .expect("the DM was enqueued for the relays");

    // The peer is now known…
    assert_eq!(
        st.db
            .dm_peer_verdict(&WARD[..], "nostr", &peer)
            .await
            .unwrap(),
        Some("allow".to_string())
    );
    // …so their inbound reply delivers unheld, even though the knob still holds
    // cold peers.
    seed_inbound(&st, &peer, "hey you", 200).await;
    assert_eq!(ward_marker(&r, &st, &peer).await, None);
    assert!(dm_holds(&r, &st).await.is_empty());
}

/// **Outbound to a guardian-`block`ed peer is refused, typed** (§ The bridge-DM
/// gate). Outbound is otherwise ungated: only an explicit block refuses, never
/// the knob.
#[tokio::test]
async fn a_send_to_a_blocked_peer_is_refused_with_the_typed_error() {
    let (r, st, _rx) = harness().await;
    link_ward(&st).await;

    let peer = hex::encode([0x66u8; 32]);
    st.db
        .set_dm_peer_verdict(
            &WARD[..],
            "nostr",
            &peer,
            fauna_core::data::DmPeerVerdict::Block,
        )
        .await
        .unwrap();

    // Opening the room is not gated — the stored thread stays readable — but
    // the send on it is.
    let room = ward_open(&r, &st, &encode_npub(&[0x66u8; 32]))
        .await
        .expect("open ok");
    let err = ward_send(&r, &st, &room, "let me through")
        .await
        .expect_err("a blocked peer's send is refused");
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");

    // Nothing was queued or stored — the refusal precedes the queue.
    let conn = st.db.conn().await;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bridge_conversation_messages",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "a refused send stores no outbound copy");
}

/// **A guardian's `block` binds every spelling of the peer, not just the one it
/// was written under** (§ The bridge-DM gate — *"a send to a guardian-`block`ed
/// peer is refused with the typed guardian-approval error"*, and the seed
/// *"never overwrites a `block`"*).
///
/// A nostr pubkey is 32 bytes with many spellings — an `npub1…`, hex in either
/// case, hex with padding. A gate that keyed its verdict lookup, its `allow`
/// seed or its room on the string the *client* sent would let a ward re-spell
/// a blocked peer past it, and mint a second `allow` row that shadows the
/// guardian's `block` instead of being suppressed by its `INSERT OR IGNORE`.
/// On the bridged family the spelling is settled once, where the room is
/// opened: the leg declares one address grammar (a lowercase `npub1…`), keys
/// the room on the canonical hex it decodes to — the key the guardian's block
/// is stored under — and every other spelling opens no room at all. Both
/// halves are asserted below: each spelling's refusal, and the row counts.
///
/// ⚠ **The fixture byte must contain hex letters.** The sibling test above uses
/// `[0x66; 32]` — all digits — for which `to_ascii_uppercase` is the identity
/// function, so a case probe built on it passes vacuously. `assert_ne!` guards
/// that directly, so a later fixture change cannot silently re-vacuum this pin.
#[tokio::test]
async fn a_block_refuses_every_spelling_of_the_same_peer() {
    let (r, st, _rx) = harness().await;
    link_ward(&st).await;

    // 0xAB spells "abab…" — letters, so the uppercase variant is a DIFFERENT
    // string. The all-digit fixture the sibling test uses cannot express this.
    let bytes = [0xABu8; 32];
    let canonical = hex::encode(bytes);
    let upper = canonical.to_ascii_uppercase();
    let padded = format!("  {canonical}\n");
    let npub = encode_npub(&bytes);
    let npub_upper = npub.to_ascii_uppercase();
    assert_ne!(
        upper, canonical,
        "vacuity guard: the fixture byte must have a hex LETTER, or this pin \
         asserts nothing"
    );
    for variant in [&upper, &padded] {
        assert_eq!(
            fauna_core::hex32::decode(variant).unwrap(),
            fauna_core::hex32::decode(&canonical).unwrap(),
            "fixture: every spelling must name the SAME wire recipient, or the \
             refusal below proves nothing about the gate"
        );
    }

    // The guardian blocks the peer, under the canonical spelling.
    st.db
        .set_dm_peer_verdict(
            &WARD[..],
            "nostr",
            &canonical,
            fauna_core::data::DmPeerVerdict::Block,
        )
        .await
        .unwrap();

    // The one spelling the leg admits lands on the canonical key, where the
    // block binds.
    let room = ward_open(&r, &st, &npub).await.expect("open ok");
    assert_eq!(
        room.far_room_id, canonical,
        "the npub opens the room keyed on the canonical hex the block names"
    );
    assert_eq!(room.guardian_state.as_deref(), Some("blocked"));
    let err = ward_send(&r, &st, &room, "let me through")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.bridges.guardian_approval_required");

    // Every other spelling of the same 32 bytes opens nothing: there is no
    // second room for a send to slip through, and none is minted.
    for spelling in [&canonical, &upper, &padded, &npub_upper] {
        let err = ward_open(&r, &st, spelling).await.unwrap_err();
        assert_eq!(
            err.code, "fauna.bridges.address_refused",
            "spelling {spelling:?} must not open a room of its own"
        );
    }

    let conn = st.db.conn().await;
    let rooms: i64 = conn
        .query_row("SELECT COUNT(*) FROM bridge_conversation_rooms", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rooms, 1, "one peer, one room, whatever the spelling");
    let stored: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bridge_conversation_messages",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, 0,
        "a refused send stores no outbound copy, any spelling"
    );
    // 1 = the block was merely missed; 2+ = the ward also minted the shadow
    // `allow` row the seed's `INSERT OR IGNORE` was supposed to suppress.
    let peers: i64 = conn
        .query_row("SELECT COUNT(*) FROM guardian_dm_peers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        peers, 1,
        "the guardian's block is the only row — no variant spelling shadowed it"
    );
}

/// **The fail-closed read-path parse.** A value only a *newer* nest could store
/// must never resolve permissively on rollback. The DB writer is deliberately
/// unvalidating (the handler validates), which is the only way to stage the
/// unnameable value the RPC path refuses.
#[tokio::test]
async fn a_knob_value_only_a_newer_nest_could_write_fails_closed_to_hold() {
    let (r, st, _rx) = harness().await;
    let peer = hex::encode([0x77u8; 32]);
    seed_inbound(&st, &peer, "hi", 100).await;

    for junk in ["hologram", "ALLOW", " allow", ""] {
        set_knob(&st, junk).await;
        assert_eq!(
            ward_marker(&r, &st, &peer).await.as_deref(),
            Some("held"),
            "unknown_peer_dm {junk:?} must fail closed to hold, never open to allow"
        );
        assert_eq!(
            dm_holds(&r, &st).await.len(),
            1,
            "…and the guardian still gets the queue entry for {junk:?}"
        );
    }

    // …and the one permissive value still means allow — the rule is
    // fail-CLOSED, not hold-everything (which would pass the loop vacuously).
    set_knob(&st, "allow").await;
    assert_eq!(ward_marker(&r, &st, &peer).await, None);
}

/// The RPC path **refuses** an unnameable knob outright — the nest never stores
/// what it cannot name (§ Policy-update compatibility).
#[tokio::test]
async fn policy_update_refuses_an_unnameable_knob() {
    let (r, st, _rx) = harness().await;
    let err = dispatch(
        &r,
        st.clone(),
        GUARDIAN,
        "fauna.family.policy.update",
        encode(&fauna_protocol::family::FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(WARD.to_vec()),
            policy: fauna_protocol::family::ReachPolicy {
                unknown_peer_dm: Some("sometimes".into()),
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("an unnameable knob is refused at write");
    assert_eq!(err.code, "fauna.family.invalid_params");
}
