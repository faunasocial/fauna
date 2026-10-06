#![cfg(feature = "nostr")]
//! Slice-B integration test for the NIP-17 gift-wrap **inbox** on the `/nostr`
//! relay (`docs/goal/ui/nostr.md` § The relay event store — inbox role +
//! read/write policy). Drives the accept path (`handle_gift_wrap_inbox`) with a
//! real `AppState` + real `nostr_*` tables (there is no in-crate WebSocket
//! harness), proving the properties the store-level unit tests cannot:
//!
//!  * an **unauthenticated** kind-1059 gift wrap addressed to a local depositor
//!    is accepted, stored verbatim, and fed to the S8.9 seal pipeline;
//!  * the DM plaintext **never rests** — the bridged DM row is sealed (probe
//!    the stored bytes: plaintext absent AND opens under the recipient's
//!    MSEK-derived secret; the vacuous-green trap);
//!  * the stored wrap is **recipient-gated** on serving — visible only to the
//!    NIP-42-authed `p`-tag recipient, never to anon or another pubkey;
//!  * a wrap to a **non-depositor** is rejected `restricted:`;
//!  * the unauthenticated inbox is **rate-limited** (a hard-coded const).
//!
//! Only compiled under `--features nostr`.

mod common;

use std::sync::Arc;

use fauna_bridge_nostr::nip01::RelayMessage;
use fauna_bridge_nostr::nip17::wrap_dm;
use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::Filter;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::relay_endpoint::{
    GIFT_WRAP_INBOX_PER_RECIPIENT_PER_MINUTE, handle_gift_wrap_inbox,
};
use fauna_nest::nostr::{self, db, store};
use fauna_nest::routes::AppState;

/// A fresh in-memory `AppState` with the `nostr_*` tables created and the
/// default `NostrState` (its gift-wrap limiter enabled).
async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let state = Arc::new(AppState::for_test(db));
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
    state
}

/// Link a custodial depositor account for `actor` (nsec encrypted with the
/// state's own nest key, so the accept path can decrypt it) and provision the
/// recipient's MSEK-derived seal key (the D2 row). Returns the account's Nostr
/// keypair and the recipient's seal secret (for the at-rest open).
async fn link_depositor(state: &AppState, actor: [u8; 32], msek: &[u8; 32]) -> Keypair {
    let actor_hex = hex::encode(actor);
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let kp = Keypair::generate();
    let encrypted = encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).unwrap();

    let conn = state.db.conn().await;
    db::link_account(
        &conn,
        &actor_hex,
        &kp.public_key_hex(),
        "generated",
        Some(&encrypted),
        None,
        None,
    )
    .unwrap();
    drop(conn);
    common::seed_recipient_seal_key(&state.db, &actor, msek).await;
    kp
}

/// Link a **proxied** account for `actor` (signing_mode='proxied', no nsec —
/// the paired head holds the key) and, unless `with_pairing` is false, grant a
/// `nostr_push` pairing so the proxied row serves as a local inbox (R8 (account-data-plane.md § The ratified decisions)/R9).
/// Returns the account's Nostr keypair — its pubkey is the `p`-tag target; the
/// secret is never used on this keyless box.
async fn link_proxied(state: &AppState, actor: [u8; 32], with_pairing: bool) -> Keypair {
    let actor_hex = hex::encode(actor);
    let kp = Keypair::generate();
    let conn = state.db.conn().await;
    db::link_account(
        &conn,
        &actor_hex,
        &kp.public_key_hex(),
        "proxied",
        None,
        None,
        None,
    )
    .unwrap();
    drop(conn);
    if with_pairing {
        state
            .db
            .store_pairing(
                &actor,
                &[0xEEu8; 32], // the paired head's nest id
                &[fauna_protocol::pair::capability::NOSTR_PUSH.to_string()],
                None,
                None,
                None,
            )
            .await
            .unwrap();
    }
    kp
}

fn accepted(msg: &RelayMessage) -> bool {
    matches!(msg, RelayMessage::Ok { accepted: true, .. })
}

fn reject_message(msg: &RelayMessage) -> &str {
    match msg {
        RelayMessage::Ok {
            accepted: false,
            message,
            ..
        } => message,
        _ => panic!("expected OK false, got {msg:?}"),
    }
}

#[tokio::test]
async fn gift_wrap_accepted_sealed_and_recipient_gated() {
    let state = state().await;
    let actor = [0x71u8; 32];
    let msek = [0x42u8; 32];
    let recipient_kp = link_depositor(&state, actor, &msek).await;
    let recipient_hex = recipient_kp.public_key_hex();

    // An UNAUTHENTICATED foreign sender deposits a gift-wrapped DM to the owner.
    let sender = Keypair::generate();
    let plaintext = "meet at the old mill at dawn";
    let wrap = wrap_dm(&sender, &recipient_kp.public_key_bytes(), plaintext).unwrap();

    let reply = handle_gift_wrap_inbox(&state, wrap.clone()).await;
    assert!(
        accepted(&reply),
        "gift wrap to a local depositor is accepted"
    );

    // (1) Stored verbatim in the relay event store (Nostr clients can fetch it).
    let conn = state.db.conn().await;
    let stored = store::query_events(
        &conn,
        &[Filter {
            kinds: Some(vec![1059]),
            ..Default::default()
        }],
        100,
    )
    .unwrap();
    drop(conn);
    assert_eq!(stored.len(), 1, "the wrap is persisted");
    assert_eq!(stored[0].id, wrap.id);

    // (2) Recipient-gated serving: visible only to the authed `p`-tag recipient.
    let ev = &stored[0];
    assert!(
        store::gift_wrap_visible_to(ev, Some(&recipient_hex)),
        "served to the authed recipient"
    );
    assert!(
        !store::gift_wrap_visible_to(ev, None),
        "hidden from an anonymous reader — no ciphertext/metadata leak"
    );
    assert!(
        !store::gift_wrap_visible_to(ev, Some(&Keypair::generate().public_key_hex())),
        "hidden from a different authed pubkey"
    );

    // (3) At-rest: the DM plaintext never rests — probe the STORED sealed bytes.
    let conn = state.db.conn().await;
    let (sealed, direction): (Vec<u8>, String) = conn
        .query_row(
            "SELECT sealed_content, direction FROM bridge_conversation_messages
              WHERE actor_id = ?1",
            [&actor[..]],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("a sealed DM row was written from the relay accept path");
    drop(conn);
    assert_eq!(direction, "in");
    assert!(
        !sealed
            .windows(plaintext.len())
            .any(|w| w == plaintext.as_bytes()),
        "the stored sealed bytes must not embed the plaintext"
    );
    let opened = common::open_recipient_record(&sealed, &msek);
    assert_eq!(opened, plaintext.as_bytes());
}

#[tokio::test]
async fn gift_wrap_to_non_depositor_is_rejected() {
    let state = state().await;
    // No account linked — the `p`-tag recipient is not a local inbox.
    let stranger = Keypair::generate();
    let sender = Keypair::generate();
    let wrap = wrap_dm(&sender, &stranger.public_key_bytes(), "for nobody here").unwrap();

    let reply = handle_gift_wrap_inbox(&state, wrap).await;
    assert!(
        !accepted(&reply),
        "a wrap to a non-depositor is not accepted"
    );
    assert!(
        reject_message(&reply).contains("restricted"),
        "rejected as restricted, got: {}",
        reject_message(&reply)
    );

    // Nothing was stored, and no DM row was written.
    let conn = state.db.conn().await;
    let n_events = store::query_events(&conn, &[Filter::default()], 100)
        .unwrap()
        .len();
    let n_dms: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bridge_conversation_messages",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    assert_eq!(n_events, 0, "a rejected wrap is not stored");
    assert_eq!(n_dms, 0, "a rejected wrap produces no DM row");
}

#[tokio::test]
async fn gift_wrap_accepted_for_a_proxied_recipient_with_a_pairing_but_not_sealed() {
    // P2.5 (spec R8/R9): on a keyless PUBLIC box, a wrap addressed to a proxied
    // account (the head holds the nsec) is accepted and stored verbatim while
    // the actor holds a `nostr_push` pairing — the box serves as the account's
    // inbox. The seal seam self-gates keyless, so NO DM row is written
    // here (the head produces it on pull; constraint (i): no plaintext / key
    // material rests on the public box).
    let state = state().await;
    let actor = [0x91u8; 32];
    let recipient_kp = link_proxied(&state, actor, true).await;

    let sender = Keypair::generate();
    let wrap = wrap_dm(&sender, &recipient_kp.public_key_bytes(), "proxied hello").unwrap();
    let reply = handle_gift_wrap_inbox(&state, wrap.clone()).await;
    assert!(
        accepted(&reply),
        "a wrap to a proxied account with a nostr_push pairing is accepted"
    );

    let conn = state.db.conn().await;
    let stored = store::query_events(
        &conn,
        &[Filter {
            kinds: Some(vec![1059]),
            ..Default::default()
        }],
        100,
    )
    .unwrap();
    let n_dms: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bridge_conversation_messages",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    assert_eq!(
        stored.len(),
        1,
        "the wrap is persisted verbatim for the head to pull"
    );
    assert_eq!(stored[0].id, wrap.id);
    assert_eq!(
        n_dms, 0,
        "the keyless box seals nothing — no DM row rests here (constraint (i))"
    );
}

#[tokio::test]
async fn gift_wrap_to_a_proxied_recipient_without_a_pairing_is_rejected() {
    // The `nostr_push` pairing is load-bearing: a proxied row ALONE does not
    // make the box an inbox (a revoked/absent pairing flips it closed). Without
    // a live pairing the wrap is rejected `restricted:`, exactly like a stranger
    // — and nothing is stored.
    let state = state().await;
    let actor = [0x92u8; 32];
    let recipient_kp = link_proxied(&state, actor, false).await;

    let sender = Keypair::generate();
    let wrap = wrap_dm(&sender, &recipient_kp.public_key_bytes(), "no pairing here").unwrap();
    let reply = handle_gift_wrap_inbox(&state, wrap).await;
    assert!(
        !accepted(&reply),
        "a proxied row without a nostr_push pairing is not a local inbox"
    );
    assert!(
        reject_message(&reply).contains("restricted"),
        "rejected as restricted, got: {}",
        reject_message(&reply)
    );

    let conn = state.db.conn().await;
    let n_events = store::query_events(&conn, &[Filter::default()], 100)
        .unwrap()
        .len();
    drop(conn);
    assert_eq!(n_events, 0, "a rejected wrap is not stored");
}

#[tokio::test]
async fn unauthenticated_inbox_is_rate_limited() {
    let state = state().await;
    let actor = [0x73u8; 32];
    let msek = [0x24u8; 32];
    let recipient_kp = link_depositor(&state, actor, &msek).await;

    // Burst well past the per-recipient quota; each `wrap_dm` is a distinct
    // event (fresh ephemeral key), so none are deduped as duplicates.
    let burst = GIFT_WRAP_INBOX_PER_RECIPIENT_PER_MINUTE + 10;
    let mut accepted_count = 0u32;
    let mut rate_limited = 0u32;
    for i in 0..burst {
        let sender = Keypair::generate();
        let wrap = wrap_dm(
            &sender,
            &recipient_kp.public_key_bytes(),
            &format!("flood {i}"),
        )
        .unwrap();
        let reply = handle_gift_wrap_inbox(&state, wrap).await;
        if accepted(&reply) {
            accepted_count += 1;
        } else if reject_message(&reply).contains("rate-limited") {
            rate_limited += 1;
        }
    }

    assert!(
        accepted_count >= 1,
        "legitimate wraps get through the limiter"
    );
    assert!(
        rate_limited >= 1,
        "the unauthenticated inbox rate limit engages under a burst \
         (accepted={accepted_count}, rate_limited={rate_limited})"
    );
    assert!(
        accepted_count <= GIFT_WRAP_INBOX_PER_RECIPIENT_PER_MINUTE + 1,
        "accepted count stays near the per-minute quota, not the full burst \
         (accepted={accepted_count})"
    );
}

#[tokio::test]
async fn unsigned_probe_cannot_distinguish_a_local_depositor_from_a_stranger() {
    // Security-review (network-exposure.md § Rulings F4) — the pre-signature
    // depositor-membership oracle. A kind-1059 gift wrap with a BAD signature
    // must reject with the SAME reason whether its `p`-tag names a local
    // nsec-depositor or a stranger; otherwise an unsigned junk probe decides
    // "is pubkey X a local inbox here?" from the reject reason alone.
    let state = state().await;

    // (a) A linked local depositor.
    let actor = [0x77u8; 32];
    let msek = [0x55u8; 32];
    let depositor_kp = link_depositor(&state, actor, &msek).await;

    // (b) A stranger — never linked, not a local inbox.
    let stranger = Keypair::generate();

    // Build a validly-structured wrap to each, then CORRUPT the signature so
    // each is unsigned-equivalent — the attacker's cheap probe. The two events
    // differ ONLY in the `p`-tag recipient (the membership axis under test).
    let mut probe_depositor = wrap_dm(
        &Keypair::generate(),
        &depositor_kp.public_key_bytes(),
        "probe",
    )
    .unwrap();
    probe_depositor.sig = "0".repeat(128); // 64 zero bytes → verify_event fails

    let mut probe_stranger =
        wrap_dm(&Keypair::generate(), &stranger.public_key_bytes(), "probe").unwrap();
    probe_stranger.sig = "0".repeat(128);

    let reply_depositor = handle_gift_wrap_inbox(&state, probe_depositor).await;
    let reply_stranger = handle_gift_wrap_inbox(&state, probe_stranger).await;

    assert!(
        !accepted(&reply_depositor),
        "a bad-sig wrap is not accepted"
    );
    assert!(!accepted(&reply_stranger), "a bad-sig wrap is not accepted");

    // The IDENTICAL reason — membership is NOT decidable from an unsigned probe.
    // RED on the old order: the depositor probe reached "invalid: bad signature"
    // while the stranger probe short-circuited to
    // "restricted: recipient is not a local inbox".
    assert_eq!(
        reject_message(&reply_depositor),
        reject_message(&reply_stranger),
        "an unsigned probe must not distinguish a depositor from a stranger \
         (depositor={:?}, stranger={:?})",
        reject_message(&reply_depositor),
        reject_message(&reply_stranger),
    );
    assert!(
        reject_message(&reply_depositor).contains("bad signature"),
        "the uniform reject is the signature failure, got: {}",
        reject_message(&reply_depositor)
    );

    // Nothing was stored on either probe.
    let conn = state.db.conn().await;
    let n_events = store::query_events(&conn, &[Filter::default()], 100)
        .unwrap()
        .len();
    drop(conn);
    assert_eq!(n_events, 0, "an unsigned probe stores nothing");
}

#[tokio::test]
async fn duplicate_wrap_resend_does_not_grow_the_dm_plane() {
    // resending the SAME valid wrap dedupes in `nostr_events`
    // (`StoreOutcome::Duplicate`) but each resend used to run the seal seam
    // again, growing the DM plane without bound while the gift-wrap total cap
    // (which counts `nostr_events`) reads 1 forever — and duplicating the DM
    // in the owner's inbox. The accept path must stop at the duplicate.
    let state = state().await;
    let actor = [0x74u8; 32];
    let msek = [0x45u8; 32];
    let recipient_kp = link_depositor(&state, actor, &msek).await;

    let sender = Keypair::generate();
    let wrap = wrap_dm(&sender, &recipient_kp.public_key_bytes(), "hello once").unwrap();

    for _ in 0..3 {
        let reply = handle_gift_wrap_inbox(&state, wrap.clone()).await;
        assert!(accepted(&reply), "a duplicate resend is still OK true");
    }

    let conn = state.db.conn().await;
    let dm_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bridge_conversation_messages WHERE actor_id = ?1",
            [&actor[..]],
            |r| r.get(0),
        )
        .unwrap();
    let event_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM nostr_events WHERE kind = 1059",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);

    assert_eq!(event_rows, 1, "the wrap dedupes in the event store");
    assert_eq!(
        dm_rows, 1,
        "a resent wrap must not mint another sealed DM row (bypass 1)"
    );
}
