//! The NIP-57 zap trust gate, proven at **both ingress points** plus the
//! designation surface that feeds them (`docs/goal/behavior/monetization.md`
//! § Zap receipts — the trust model; `docs/goal/ui/nostr.md` § The relay
//! event store, owner-only scope category (2)).
//!
//! What makes this slice necessary: a kind-9735 zap receipt is signed by the
//! *recipient's* LNURL/wallet server — not the sender, and not any key Fauna
//! knows a priori — and it is plain signed JSON anyone may mint naming any
//! recipient, whose `bolt11` no part of this system checks against a real
//! Lightning payment. **A valid signature therefore proves nothing about
//! payment.** Before this slice both ingresses believed any signature-valid
//! receipt naming a local pubkey.
//!
//! Coverage:
//!   * **ingress A** (the external-relay sweep's shared gate): a designated
//!     signer's receipt is recorded; an undesignated one is not;
//!   * **ingress B** (the nest's own relay endpoint): the same, end-to-end
//!     through the real `handle_zap_receipt_inbox` — and this is also the
//!     first implementation of the ratified category-2 acceptance, which had
//!     stood unbuilt since 2026-07-13 (a receipt is authored by the payee's
//!     LNURL server, never a local account, so the owner-only gate refused
//!     every one of them);
//!   * **the ratified default**: a payee who has designated nobody believes
//!     nobody, at both ingresses;
//!   * **fails closed for a non-local payee** (no account → empty designation
//!     set → inert), with no separate arm;
//!   * **the stapling forgery**: even a designated signer cannot staple a
//!     genuine zap request onto a different recipient;
//!   * **the F4 privacy property**: every untrusted verdict is one wire
//!     message, so a probe cannot decide local-account membership or read a
//!     payee's trust root from reject reasons;
//!   * **the designation round-trips over WS-RPC** through the real router
//!     dispatch, caller-scoped.
//!
//! Only compiled under `--features nostr`.

#![cfg(feature = "nostr")]

mod common;
use common::dispatch;
use common::encode;
use common::zap::{designate_zap_signer as designate, link_payee, zap_receipt};

use std::sync::Arc;

use fauna_bridge_nostr::nip01::RelayMessage;
use fauna_bridge_nostr::nip57::{ZapUntrustedReason, ZapVerdict};
use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::UnsignedEvent;
use fauna_nest::nostr::relay_endpoint::handle_zap_receipt_inbox;
use fauna_nest::nostr::zap_signer_handlers::register_nostr_zap_signer_handlers;
use fauna_nest::nostr::{self, db, store, zap_ingest};
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::decode_strict as decode;
use fauna_protocol::nostr::{
    AddZapSignerReply, AddZapSignerRequest, ListZapSignersReply, ListZapSignersRequest,
    RemoveZapSignerReply, RemoveZapSignerRequest,
};

// ── harness ──────────────────────────────────────────────────────────────

async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    Arc::new(AppState::for_test(db))
}

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let st = state().await;
    let mut b = RpcRouter::builder();
    register_nostr_zap_signer_handlers(&mut b);
    (b.build(), st)
}

/// Store a real, signature-valid kind-1 note authored by `author` in the
/// relay event store, returning its event id — the zap subject. The subject
/// binding compares the receipt's `e`-tagged event's stored author against
/// the `p`-tagged payee, so a zappable fixture must actually rest on the box.
async fn store_note_by(state: &AppState, author: &Keypair) -> String {
    // A per-call nonce in the content keeps successive notes distinct: the
    // event id is content-derived, so two identical notes would collide and
    // the second store would be a no-op duplicate.
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let note = author.sign_event(UnsignedEvent {
        pubkey: author.public_key_bytes(),
        created_at: 1_700_000_000,
        kind: 1,
        tags: vec![],
        content: format!("a zappable note {n}"),
    });
    let conn = state.db.conn().await;
    let outcome = store::store_event(&conn, &note, false).expect("store note");
    assert!(outcome.is_newly_stored(), "fixture note must store");
    drop(conn);
    note.id
}

async fn zap_count(state: &AppState, target_event_id: &str) -> (i64, i64) {
    let conn = state.db.conn().await;
    let out = db::get_zap_total(&conn, target_event_id).unwrap();
    drop(conn);
    out
}

async fn event_is_stored(state: &AppState, event_id: &str) -> bool {
    let conn = state.db.conn().await;
    let filter = fauna_bridge_nostr::types::Filter {
        ids: Some(vec![event_id.to_string()]),
        ..Default::default()
    };
    let found = store::query_events(&conn, std::slice::from_ref(&filter), 10)
        .map(|events| !events.is_empty())
        .unwrap_or(false);
    drop(conn);
    found
}

fn reject_message(msg: &RelayMessage) -> String {
    match msg {
        RelayMessage::Ok {
            accepted, message, ..
        } => {
            assert!(!accepted, "expected a rejection, got accept: {message}");
            message.clone()
        }
        other => panic!("expected OK frame, got {other:?}"),
    }
}

// ── ingress A — the external-relay sweep's shared gate ───────────────────

#[tokio::test]
async fn ingress_a_believes_a_designated_signer_and_records_the_zap() {
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();
    designate(&state, actor, &signer).await;

    // The zapped event must be the payee's OWN, held on this box (the subject
    // binding). This is the positive control the verify contract requires: a
    // legitimate zap of the payee's own event is still recorded.
    let note = store_note_by(&state, &payee).await;
    let ev = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let conn = state.db.conn().await;
    let verdict = zap_ingest::classify_incoming_zap(&conn, &ev);
    let ZapVerdict::Trusted(zap) = verdict else {
        panic!("a designated signer's receipt must be trusted, got {verdict:?}");
    };
    zap_ingest::record_zap(&conn, &zap);
    drop(conn);

    // Counted, with the real parsed amount — not merely "a row exists".
    assert_eq!(zap_count(&state, &note).await, (21_000, 1));
}

#[tokio::test]
async fn ingress_a_is_inert_for_an_undesignated_signer() {
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    // A *different* signer designated; the receipt comes from a stranger.
    designate(&state, actor, &Keypair::generate()).await;
    let stranger = Keypair::generate();

    let ev = zap_receipt(
        &stranger,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        "post-1",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let conn = state.db.conn().await;
    let verdict = zap_ingest::classify_incoming_zap(&conn, &ev);
    drop(conn);
    assert!(
        matches!(
            verdict,
            ZapVerdict::Untrusted(ZapUntrustedReason::UndesignatedSigner)
        ),
        "a stranger's receipt must be untrusted, got {verdict:?}"
    );
    // Nothing recorded — the signature was perfectly valid and bought nothing.
    assert_eq!(zap_count(&state, "post-1").await, (0, 0));
}

#[tokio::test]
async fn a_payee_who_designated_nobody_believes_nobody() {
    // The ratified out-of-the-box default: an empty designation set makes
    // every receipt inert. This is the state a fresh nest is in, so it is the
    // behavior that must hold before a user ever opens the Nostr page.
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();

    let ev = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        "post-1",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let conn = state.db.conn().await;
    let verdict = zap_ingest::classify_incoming_zap(&conn, &ev);
    drop(conn);
    assert!(
        matches!(verdict, ZapVerdict::Untrusted(_)),
        "no designation means no belief, got {verdict:?}"
    );
    assert_eq!(zap_count(&state, "post-1").await, (0, 0));
}

#[tokio::test]
async fn a_receipt_for_a_non_local_payee_fails_closed() {
    // No `nostr_accounts` row → the join yields the empty designation set →
    // the ordinary untrusted path. There is deliberately no separate arm for
    // "unknown payee", so there is none to get wrong.
    let state = state().await;
    let signer = Keypair::generate();
    let stranger_payee = Keypair::generate();

    let ev = zap_receipt(
        &signer,
        &stranger_payee.public_key_hex(),
        &stranger_payee.public_key_hex(),
        "post-1",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let conn = state.db.conn().await;
    let verdict = zap_ingest::classify_incoming_zap(&conn, &ev);
    drop(conn);
    assert!(
        matches!(verdict, ZapVerdict::Untrusted(_)),
        "a receipt for nobody local must be inert, got {verdict:?}"
    );
    assert_eq!(zap_count(&state, "post-1").await, (0, 0));
}

#[tokio::test]
async fn a_designation_is_scoped_to_the_payee_who_made_it() {
    // A trust root is per payee, not per box. One user designating their
    // wallet provider must not make that provider's receipts believable for
    // a *different* user on the same nest — otherwise the first user to add
    // a signer would silently extend belief to every other local account.
    //
    // Added after the mutation matrix found nothing pinning this: dropping
    // the `nostr_accounts` join in `trusted_zap_signers_for_pubkey` (making
    // the lookup global) left the whole suite green.
    let state = state().await;
    let alice: [u8; 32] = [0x11; 32];
    let bob: [u8; 32] = [0x22; 32];
    let _alice_key = link_payee(&state, alice).await;
    let bob_key = link_payee(&state, bob).await;

    // Alice designates a signer. Bob designates nobody.
    let signer = Keypair::generate();
    designate(&state, alice, &signer).await;

    // A receipt for BOB, signed by ALICE's designated signer.
    let ev = zap_receipt(
        &signer,
        &bob_key.public_key_hex(),
        &bob_key.public_key_hex(),
        "post-1",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let conn = state.db.conn().await;
    let verdict = zap_ingest::classify_incoming_zap(&conn, &ev);
    drop(conn);
    assert!(
        matches!(
            verdict,
            ZapVerdict::Untrusted(ZapUntrustedReason::UndesignatedSigner)
        ),
        "Alice's designation must not speak for Bob's money, got {verdict:?}"
    );
    assert_eq!(zap_count(&state, "post-1").await, (0, 0));
}

#[tokio::test]
async fn a_designated_signer_still_cannot_staple_a_request_onto_another_recipient() {
    // The one forgery a designated signer could still attempt: a genuine
    // kind-9734 zap request for someone else, re-pointed by the receipt's own
    // `p` tag. Designation is not a licence to rewrite who was paid.
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();
    designate(&state, actor, &signer).await;

    let someone_else = Keypair::generate();
    let ev = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &someone_else.public_key_hex(), // request names a different recipient
        "post-1",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let conn = state.db.conn().await;
    let verdict = zap_ingest::classify_incoming_zap(&conn, &ev);
    drop(conn);
    assert!(
        matches!(
            verdict,
            ZapVerdict::Untrusted(ZapUntrustedReason::ZapRequestRecipientMismatch)
        ),
        "a stapled request must be refused even from a designated signer, got {verdict:?}"
    );
    assert_eq!(zap_count(&state, "post-1").await, (0, 0));
}

#[tokio::test]
async fn ingress_a_refuses_a_cross_attributed_receipt() {
    // The subject binding at the shared gate: a signer designated by
    // payee A speaks only about A's own events. A receipt naming A in `p` but
    // tagging B's event in `e` writes A's signer's words onto B's money
    // surface — refused whichever door it arrives through.
    let state = state().await;
    let attacker: [u8; 32] = [0x11; 32];
    let victim: [u8; 32] = [0x22; 32];
    let attacker_payee = link_payee(&state, attacker).await;
    let victim_payee = link_payee(&state, victim).await;
    let signer = Keypair::generate();
    designate(&state, attacker, &signer).await;

    let victim_note = store_note_by(&state, &victim_payee).await;

    // All three conjuncts satisfied: designated signer, `p` names
    // the attacker's own payee, request recipient agrees. Only the subject is
    // someone else's.
    let ev = zap_receipt(
        &signer,
        &attacker_payee.public_key_hex(),
        &attacker_payee.public_key_hex(),
        &victim_note,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let conn = state.db.conn().await;
    let verdict = zap_ingest::classify_incoming_zap(&conn, &ev);
    drop(conn);
    assert!(
        matches!(verdict, ZapVerdict::Untrusted(_)),
        "a designated signer must not attribute a zap to another user's event, got {verdict:?}"
    );
    assert_eq!(zap_count(&state, &victim_note).await, (0, 0));
}

// ── ingress B — the nest's own relay endpoint (category 2, first built) ──

#[tokio::test]
async fn ingress_b_accepts_stores_and_counts_a_designated_signers_receipt() {
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();
    designate(&state, actor, &signer).await;

    let note = store_note_by(&state, &payee).await;
    let ev = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    let event_id = ev.id.clone();

    let reply = handle_zap_receipt_inbox(&state, &ev)
        .await
        .expect("a parseable zap receipt is handled by the carve-out, not fallen through");
    match reply {
        RelayMessage::Ok {
            accepted, message, ..
        } => assert!(accepted, "must be accepted, got reject: {message}"),
        other => panic!("expected OK frame, got {other:?}"),
    }

    // Category (2) means the receipt rests in the event store *and* is
    // counted in the accounting table — both halves, not just one.
    assert!(
        event_is_stored(&state, &event_id).await,
        "an accepted receipt is stored verbatim so Nostr clients can fetch it"
    );
    assert_eq!(zap_count(&state, &note).await, (21_000, 1));
}

#[tokio::test]
async fn a_designated_signer_cannot_attribute_a_zap_to_another_users_post() {
    // This gap is reachable at the unauthenticated door. The probe reproduced verbatim: an attacker links their own account,
    // designates a throwaway key they hold, and mints a signature-valid
    // kind-9735 with `p` = their own pubkey (so every `p`-tag conjunct passes)
    // but `e` = a victim's real stored post. Pre-fix this was `accepted: true`
    // and counted 21_000 onto the victim's post; it must now be refused.
    let state = state().await;
    let attacker: [u8; 32] = [0x11; 32];
    let victim: [u8; 32] = [0x22; 32];
    let attacker_payee = link_payee(&state, attacker).await;
    let victim_payee = link_payee(&state, victim).await;
    let signer = Keypair::generate();
    designate(&state, attacker, &signer).await;

    let victim_note = store_note_by(&state, &victim_payee).await;

    let ev = zap_receipt(
        &signer,
        &attacker_payee.public_key_hex(),
        &attacker_payee.public_key_hex(),
        &victim_note,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    let event_id = ev.id.clone();

    let reply = handle_zap_receipt_inbox(&state, &ev)
        .await
        .expect("handled by the carve-out");
    assert!(
        reject_message(&reply).starts_with("restricted:"),
        "a receipt tagging another user's event is refused"
    );
    assert!(
        !event_is_stored(&state, &event_id).await,
        "the cross-attributed receipt must not rest on the box"
    );
    assert_eq!(
        zap_count(&state, &victim_note).await,
        (0, 0),
        "nothing is attributed to the victim's post"
    );
}

#[tokio::test]
async fn a_receipt_for_an_event_this_box_has_never_seen_is_refused() {
    // The not-held sub-question of fix (a), ruled REFUSE: a total for an event
    // the box cannot see is unrenderable anyway, and accepting-but-marking
    // would defer the check to every reader — which the "never at read" clause
    // exists to prevent. The receipt is otherwise perfect (designated signer,
    // addressed to the payee, request agrees); only its subject is unknown.
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();
    designate(&state, actor, &signer).await;

    let unseen = "f".repeat(64);
    let ev = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &unseen,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    let event_id = ev.id.clone();

    let reply = handle_zap_receipt_inbox(&state, &ev)
        .await
        .expect("handled by the carve-out");
    assert!(
        reject_message(&reply).starts_with("restricted:"),
        "a receipt naming an unheld event is refused"
    );
    assert!(!event_is_stored(&state, &event_id).await);
    assert_eq!(zap_count(&state, &unseen).await, (0, 0));
}

#[tokio::test]
async fn ingress_b_refuses_an_undesignated_signer_and_stores_nothing() {
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    designate(&state, actor, &Keypair::generate()).await;
    let stranger = Keypair::generate();

    let ev = zap_receipt(
        &stranger,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        "post-1",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    let event_id = ev.id.clone();

    let reply = handle_zap_receipt_inbox(&state, &ev)
        .await
        .expect("handled by the carve-out");
    assert!(
        reject_message(&reply).starts_with("restricted:"),
        "an undesignated signer is refused as restricted"
    );
    assert!(
        !event_is_stored(&state, &event_id).await,
        "a refused receipt must not rest on the box — the gate is at ingest"
    );
    assert_eq!(zap_count(&state, "post-1").await, (0, 0));
}

#[tokio::test]
async fn ingress_b_refuses_a_bad_signature_before_asking_the_trust_question() {
    // Ordering is load-bearing for privacy (`network-exposure.md` § Rulings
    // F4): signature-verify runs before the trust lookup, so an unsigned
    // probe cannot decide local-account membership from the reject reason.
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();
    designate(&state, actor, &signer).await;

    let mut ev = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        "post-1",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    ev.sig = "0".repeat(128); // otherwise a receipt that WOULD be believed

    let reply = handle_zap_receipt_inbox(&state, &ev)
        .await
        .expect("handled by the carve-out");
    assert_eq!(reject_message(&reply), "invalid: bad signature");
    assert_eq!(zap_count(&state, "post-1").await, (0, 0));
}

#[tokio::test]
async fn every_untrusted_verdict_is_one_indistinguishable_wire_message() {
    // The F4 property, asserted directly: were "no such local payee"
    // distinguishable from "signer not designated" — or from "the subject is
    // someone else's event" — a signed probe could enumerate local accounts,
    // read every payee's trust root, and probe which events a payee authored
    // off the reject reasons. All must be byte-identical on the wire.
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let designated = Keypair::generate();
    designate(&state, actor, &designated).await;
    let signer = Keypair::generate();

    let undesignated = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        "post-1",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    let non_local_payee = Keypair::generate();
    let unknown = zap_receipt(
        &signer,
        &non_local_payee.public_key_hex(),
        &non_local_payee.public_key_hex(),
        "post-2",
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    // A designated signer's receipt tagging another user's stored event — the
    // refusal. It must be indistinguishable from the other two, or a
    // designated wallet provider could probe which events a payee did NOT
    // author (a distinct reject would leak exactly that).
    let victim = link_payee(&state, [0x22; 32]).await;
    let victim_note = store_note_by(&state, &victim).await;
    let cross_attributed = zap_receipt(
        &designated,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &victim_note,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let a = reject_message(
        &handle_zap_receipt_inbox(&state, &undesignated)
            .await
            .unwrap(),
    );
    let b = reject_message(&handle_zap_receipt_inbox(&state, &unknown).await.unwrap());
    let c = reject_message(
        &handle_zap_receipt_inbox(&state, &cross_attributed)
            .await
            .unwrap(),
    );
    assert_eq!(
        a, b,
        "local-account membership must not be decidable from the reject reason"
    );
    assert_eq!(
        a, c,
        "a subject-mismatch refusal must be indistinguishable from a signer refusal"
    );
}

#[tokio::test]
async fn ingress_b_is_idempotent_on_a_resent_receipt() {
    // A relay client may resend. The second delivery must not double-count
    // the zap — the accounting table is what the tip and purchase surfaces
    // read, so a duplicate would inflate a payment total.
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();
    designate(&state, actor, &signer).await;

    let note = store_note_by(&state, &payee).await;
    let ev = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );

    let _ = handle_zap_receipt_inbox(&state, &ev).await.unwrap();
    let second = handle_zap_receipt_inbox(&state, &ev).await.unwrap();
    match second {
        RelayMessage::Ok {
            accepted, message, ..
        } => {
            assert!(accepted, "a resend is acknowledged, not refused");
            assert!(message.starts_with("duplicate:"), "got {message}");
        }
        other => panic!("expected OK frame, got {other:?}"),
    }
    assert_eq!(
        zap_count(&state, &note).await,
        (21_000, 1),
        "a resent receipt must not double-count"
    );
}

// ── the designation surface, over WS-RPC ─────────────────────────────────

#[tokio::test]
async fn the_designation_round_trips_over_ws_rpc_and_is_caller_scoped() {
    let (router, state) = router_and_state().await;
    let actor: [u8; 32] = [0x11; 32];
    let other: [u8; 32] = [0x22; 32];
    let signer = Keypair::generate();

    // add — the reply carries the stored row, normalized.
    let added: AddZapSignerReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.nostr.zap_signers.add",
            encode(&AddZapSignerRequest {
                signer_pubkey: signer.public_key_hex().to_uppercase(),
                label: "Alby".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("add succeeds"),
    )
    .unwrap();
    assert_eq!(added.signer.signer_pubkey, signer.public_key_hex());
    assert_eq!(added.signer.label, "Alby");

    // list — the caller sees their own designation.
    let listed: ListZapSignersReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.nostr.zap_signers.list",
            encode(&ListZapSignersRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list succeeds"),
    )
    .unwrap();
    assert_eq!(listed.signers.len(), 1);
    assert_eq!(listed.signers[0].signer_pubkey, signer.public_key_hex());

    // caller-scoping — another actor sees nothing and cannot remove it.
    let other_list: ListZapSignersReply = decode(
        &dispatch(
            &router,
            state.clone(),
            other,
            "fauna.nostr.zap_signers.list",
            encode(&ListZapSignersRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list succeeds"),
    )
    .unwrap();
    assert!(
        other_list.signers.is_empty(),
        "a trust root is the payee's own — nobody else may read it"
    );
    let other_remove: RemoveZapSignerReply = decode(
        &dispatch(
            &router,
            state.clone(),
            other,
            "fauna.nostr.zap_signers.remove",
            encode(&RemoveZapSignerRequest {
                signer_pubkey: signer.public_key_hex(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("remove dispatches"),
    )
    .unwrap();
    assert!(
        !other_remove.removed,
        "another actor must not be able to undesignate this payee's signer"
    );

    // remove — the owner can, and it is reflected.
    let removed: RemoveZapSignerReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.nostr.zap_signers.remove",
            encode(&RemoveZapSignerRequest {
                signer_pubkey: signer.public_key_hex(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("remove succeeds"),
    )
    .unwrap();
    assert!(removed.removed);
    let after: ListZapSignersReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.nostr.zap_signers.list",
            encode(&ListZapSignersRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list succeeds"),
    )
    .unwrap();
    assert!(after.signers.is_empty());
}

#[tokio::test]
async fn a_malformed_signer_pubkey_is_refused_rather_than_stored() {
    // A designation that could never match a real `event.pubkey` is silently
    // dead weight the payee believes they made — it would present as "my zaps
    // are ignored" against a correct-looking roster.
    let (router, state) = router_and_state().await;
    let actor: [u8; 32] = [0x11; 32];

    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.zap_signers.add",
        encode(&AddZapSignerRequest {
            signer_pubkey: "not-a-pubkey".into(),
            label: String::new(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("a malformed pubkey is refused");
    assert_eq!(err.code, "fauna.nostr.invalid_params");

    let listed: ListZapSignersReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.nostr.zap_signers.list",
            encode(&ListZapSignersRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list succeeds"),
    )
    .unwrap();
    assert!(listed.signers.is_empty(), "nothing was stored");
}

#[tokio::test]
async fn designating_the_same_signer_twice_refreshes_rather_than_duplicates() {
    let (router, state) = router_and_state().await;
    let actor: [u8; 32] = [0x11; 32];
    let signer = Keypair::generate();

    for label in ["first", "second"] {
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.nostr.zap_signers.add",
            encode(&AddZapSignerRequest {
                signer_pubkey: signer.public_key_hex(),
                label: label.into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("add succeeds");
    }

    let listed: ListZapSignersReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.nostr.zap_signers.list",
            encode(&ListZapSignersRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list succeeds"),
    )
    .unwrap();
    assert_eq!(listed.signers.len(), 1, "one signer, one row");
    assert_eq!(listed.signers[0].label, "second", "the label refreshed");
}

#[tokio::test]
async fn a_designation_added_in_any_case_matches_a_real_receipt() {
    // The case-normalization is not cosmetic: the roster and the verdict must
    // agree, or a payee who pasted an uppercase pubkey from their provider's
    // dashboard would have a correct-looking roster that believes nothing.
    let (router, state) = router_and_state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();

    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.zap_signers.add",
        encode(&AddZapSignerRequest {
            signer_pubkey: signer.public_key_hex().to_uppercase(),
            label: String::new(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("add succeeds");

    let note = store_note_by(&state, &payee).await;
    let ev = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    let conn = state.db.conn().await;
    let verdict = zap_ingest::classify_incoming_zap(&conn, &ev);
    drop(conn);
    assert!(
        matches!(verdict, ZapVerdict::Trusted(_)),
        "an uppercase designation must still match, got {verdict:?}"
    );
}

#[tokio::test]
async fn undesignating_stops_belief_at_the_next_receipt() {
    let state = state().await;
    let actor: [u8; 32] = [0x11; 32];
    let payee = link_payee(&state, actor).await;
    let signer = Keypair::generate();
    designate(&state, actor, &signer).await;

    let note_1 = store_note_by(&state, &payee).await;
    let first = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note_1,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    let reply = handle_zap_receipt_inbox(&state, &first).await.unwrap();
    assert!(matches!(reply, RelayMessage::Ok { accepted: true, .. }));

    let conn = state.db.conn().await;
    assert!(db::remove_zap_signer(&conn, &hex::encode(actor), &signer.public_key_hex()).unwrap());
    drop(conn);

    let note_2 = store_note_by(&state, &payee).await;
    let second = zap_receipt(
        &signer,
        &payee.public_key_hex(),
        &payee.public_key_hex(),
        &note_2,
        &"d".repeat(64),
        Some("lnbc210n1pjfake"),
        1_700_000_000,
    );
    let reply = handle_zap_receipt_inbox(&state, &second).await.unwrap();
    assert!(
        reject_message(&reply).starts_with("restricted:"),
        "revoking a designation stops belief at the next receipt"
    );
    // The already-accepted one stands: the gate is at ingest, so
    // undesignating stops future belief rather than retracting the past.
    assert_eq!(zap_count(&state, &note_1).await, (21_000, 1));
    assert_eq!(zap_count(&state, &note_2).await, (0, 0));
}
