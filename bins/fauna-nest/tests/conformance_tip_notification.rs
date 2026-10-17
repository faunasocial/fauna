//! A credited tip rings the post's author (`docs/goal/behavior/monetization.md`
//! § Tips — a tip's consequence is attribution, display and **notification**,
//! never a grant; the `tip` type is named in `behavior/notifications.md`
//! § The notification type).
//!
//! Every receipt below is a real, signature-valid kind-9735 driven through the
//! relay's unauthenticated receipt inbox (`handle_zap_receipt_inbox`, ingress
//! B) — the trust gate, the feature gate, the store, the subject resolution,
//! the tip↔purchase split, the record and the notification, in production
//! order. The purchase half of the split (a sale rings nothing) is pinned in
//! `conformance_zap_purchase.rs`, whose fixtures can build a sold post.
//!
//! Coverage:
//!   * a believed tip on a post rings its author once, typed `tip`, naming the
//!     post (the deep link) and carrying the localized amount sentence;
//!   * a redelivered receipt rings nobody a second time;
//!   * two tips the box cannot attribute both ring — the generic notification
//!     dedup must not fold a second tipper's money into the first's row;
//!   * an untrusted receipt rings nobody (it was never recorded);
//!   * a zap on an event this box never published rings nobody (no post).

#![cfg(all(feature = "nostr", feature = "zaps"))]

mod common;

use std::sync::Arc;

use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::{Event, UnsignedEvent};
use fauna_nest::nostr::relay_endpoint::handle_zap_receipt_inbox;
use fauna_nest::nostr::{db, store};
use fauna_nest::routes::AppState;
use fauna_protocol::notifications::NotifType;

use common::zap::{anon_sender, designate_zap_signer as designate, link_payee, zap_receipt};

/// 21 sats = 21_000 msats (`n` multiplier: amount x 100 msats).
const LNBC_21_SATS: &str = "lnbc210n1pjfake";

async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    fauna_nest::nostr::init_db(&db)
        .await
        .expect("init nostr tables");
    Arc::new(AppState::for_test(db))
}

/// Store a note by `author` and map it to `fauna_post_id` the way the outbound
/// publish path does (`nostr::store::materialize_account`).
async fn publish_note_as(state: &AppState, author: &Keypair, fauna_post_id: &str) -> String {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let note = author.sign_event(UnsignedEvent {
        pubkey: author.public_key_bytes(),
        created_at: 1_700_000_000,
        kind: 1,
        tags: vec![],
        content: format!("a tippable note {n}"),
    });
    let conn = state.db.conn().await;
    assert!(
        store::store_event(&conn, &note, false)
            .expect("store note")
            .is_newly_stored()
    );
    db::insert_event_map(
        &conn,
        fauna_post_id,
        &note.id,
        &author.public_key_hex(),
        "outbound",
    )
    .expect("map the note to its fauna post");
    drop(conn);
    note.id
}

/// Post the receipt to the relay inbox; `true` when the relay accepted it.
async fn deliver(state: &Arc<AppState>, receipt: &Event) -> bool {
    match handle_zap_receipt_inbox(state, receipt).await {
        Some(fauna_bridge_nostr::nip01::RelayMessage::Ok { accepted, .. }) => accepted,
        other => panic!("the inbox must answer a zap receipt with OK, got {other:?}"),
    }
}

async fn tips_rung(
    state: &AppState,
    author: [u8; 32],
) -> Vec<fauna_nest::db::notifications::NotificationRow> {
    state
        .db
        .list_notifications(&author, None, 50)
        .await
        .expect("list notifications")
        .into_iter()
        .filter(|n| n.notif_type == NotifType::Tip)
        .collect()
}

#[tokio::test]
async fn a_credited_tip_rings_the_posts_author_once() {
    let state = state().await;
    let author = [0x31; 32];
    let payee = link_payee(&state, author).await;
    let signer = Keypair::generate();
    designate(&state, author, &signer).await;
    let post_id = hex::encode([0x41; 32]);
    let note_id = publish_note_as(&state, &payee, &post_id).await;

    let tipper = anon_sender();
    let receipt = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note_id,
        &tipper,
        Some(LNBC_21_SATS),
        1_700_000_100,
    );
    assert!(
        deliver(&state, &receipt).await,
        "a designated receipt is accepted"
    );

    let rung = tips_rung(&state, author).await;
    assert_eq!(rung.len(), 1, "one credited tip, one notification");
    let row = &rung[0];
    assert_eq!(row.source, "fauna");
    assert_eq!(
        row.content_id.as_deref().map(hex::encode).as_deref(),
        Some(post_id.as_str()),
        "the row names the tipped post — its deep link"
    );
    assert_eq!(row.sender_id, None, "the tipper has no account on this box");
    let body = row.body.as_ref().expect("a localized body");
    assert_eq!(body.key, "notifications.row_tip_sats");
    assert_eq!(body.args.get("value").map(String::as_str), Some("21"));
    assert_eq!(
        body.args.get("sender").map(String::as_str),
        Some(&tipper.public_key_hex()[..8]),
        "an outside tipper is named by the key their zap request was signed with"
    );
    assert!(row.summary.contains("21 sats"), "{}", row.summary);

    // The same receipt again — a second relay, a replay — rings nobody.
    assert!(deliver(&state, &receipt).await);
    assert_eq!(
        tips_rung(&state, author).await.len(),
        1,
        "a redelivery is not a new tip"
    );
}

#[tokio::test]
async fn two_unattributed_tips_on_one_post_both_ring() {
    let state = state().await;
    let author = [0x32; 32];
    let payee = link_payee(&state, author).await;
    let signer = Keypair::generate();
    designate(&state, author, &signer).await;
    let post_id = hex::encode([0x42; 32]);
    let note_id = publish_note_as(&state, &payee, &post_id).await;

    for (i, tipper) in [Keypair::generate(), Keypair::generate()]
        .iter()
        .enumerate()
    {
        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            tipper,
            Some(LNBC_21_SATS),
            1_700_000_200 + i as u64,
        );
        assert!(deliver(&state, &receipt).await);
    }
    assert_eq!(
        tips_rung(&state, author).await.len(),
        2,
        "both tippers share a NULL sender and the post id; the generic dedup must not swallow the second"
    );
}

#[tokio::test]
async fn an_untrusted_receipt_rings_nobody() {
    let state = state().await;
    let author = [0x33; 32];
    let payee = link_payee(&state, author).await;
    // Deliberately NOT designated.
    let stranger = Keypair::generate();
    let post_id = hex::encode([0x43; 32]);
    let note_id = publish_note_as(&state, &payee, &post_id).await;

    let receipt = zap_receipt(
        &stranger,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note_id,
        &anon_sender(),
        Some(LNBC_21_SATS),
        1_700_000_300,
    );
    assert!(
        !deliver(&state, &receipt).await,
        "an undesignated signer is refused"
    );
    assert!(tips_rung(&state, author).await.is_empty());
}

#[tokio::test]
async fn a_zap_on_an_event_with_no_fauna_post_rings_nobody() {
    let state = state().await;
    let author = [0x34; 32];
    let payee = link_payee(&state, author).await;
    let signer = Keypair::generate();
    designate(&state, author, &signer).await;
    // A note the payee authored but this box never mapped to a Fauna post.
    let note = payee.sign_event(UnsignedEvent {
        pubkey: payee.public_key_bytes(),
        created_at: 1_700_000_000,
        kind: 1,
        tags: vec![],
        content: "a nostr-native note".into(),
    });
    let conn = state.db.conn().await;
    store::store_event(&conn, &note, false).expect("store note");
    drop(conn);

    let receipt = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note.id,
        &anon_sender(),
        Some(LNBC_21_SATS),
        1_700_000_400,
    );
    assert!(deliver(&state, &receipt).await, "still a believed tip");
    assert!(
        tips_rung(&state, author).await.is_empty(),
        "no post to open, so no notification"
    );
}
