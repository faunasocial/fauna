//! `fauna.tips.list` — the post-addressed tip attribution read
//! (`docs/goal/behavior/monetization.md` § Tips).
//!
//! § Tips ratifies that a tip names *(payee, post)* and that its consequence
//! is **attribution/display/notification, never a grant**. § Implementation
//! status recorded the two things that blocked every app display leg: no
//! first-class mechanism-independent tip concept, and no read addressable
//! from a post. This suite proves the second.
//!
//! **What the addressing change is actually worth.** `nostr.zaps.total` has
//! existed and worked since 2026-07-22 — keyed by *Nostr event id*. A client
//! holding a `PostSummary` has a Fauna post id and nothing else, so the
//! totals were unreachable from the only place they render. Every test below
//! asks by **Fauna post id**; none of them mentions a Nostr event id, which is
//! the property the display legs need.
//!
//! Coverage:
//!   * the kind is registered and answers **without** `--features nostr`,
//!     with an honest zero rather than `unknown_kind` (the mechanism-blind
//!     surface does not blink out when its one mechanism is off);
//!   * a malformed post id is refused;
//!   * (nostr) a believed zap on a post surfaces as a tip, resolved
//!     post → mechanism rows through the outbound event map;
//!   * (nostr) totals cover every tip while the item window is capped, and
//!     `has_more` says so;
//!   * (nostr) an amount-less tip counts but does not sum — the distinction
//!     § Tips's attribution consequence depends on;
//!   * (nostr) a tipper with a linked local account is attributed to their
//!     actor; an outside tipper still counts, unattributed;
//!   * (nostr) an **untrusted** receipt contributes nothing — not because
//!     this read filters, but because the ingest gate never recorded it
//!     (§ Zap receipts — *at ingest, never at read*).

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::tip_handlers::register_tip_handlers;
use fauna_protocol::tips::{TipsListReply, TipsListRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

// ── harness ──────────────────────────────────────────────────────────────

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    #[cfg(feature = "nostr")]
    fauna_nest::nostr::init_db(&db)
        .await
        .expect("init nostr tables");
    let st = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_tip_handlers(&mut b);
    (b.build(), st)
}

async fn tips_list(
    router: &RpcRouter,
    state: Arc<AppState>,
    post_id: &str,
    limit: Option<u32>,
) -> Result<TipsListReply, RpcError> {
    let actor = [7u8; 32];
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = router
        .kind_meta("fauna.tips.list")
        .expect("kind registered");
    let payload = Bytes::from(
        encode_canonical(&TipsListRequest {
            post_id: post_id.to_string(),
            limit,
            extra: Default::default(),
        })
        .expect("encode req")
        .to_vec(),
    );
    let bytes = (meta.handler)(state, actor, payload).await?;
    Ok(decode(&bytes).expect("decode reply"))
}

fn a_post_id(seed: u8) -> String {
    hex::encode([seed; 32])
}

// ── mechanism-blind surface ──────────────────────────────────────────────

/// The kind exists on **every** nest build. Without `--features nostr` there
/// is no tip mechanism compiled in, and the honest answer is zero — not
/// `unknown_kind`, which a client could not distinguish from "this nest is
/// too old to know the kind" and would render as an absent feature rather
/// than an empty one.
#[tokio::test]
async fn the_kind_answers_zero_on_a_nest_with_no_tip_mechanism() {
    let (router, state) = router_and_state().await;
    let reply = tips_list(&router, state, &a_post_id(0xaa), None)
        .await
        .expect("the mechanism-blind kind answers on every build");
    assert_eq!(reply.total_msats, 0);
    assert_eq!(reply.tip_count, 0);
    assert!(reply.tips.is_empty());
    assert!(!reply.has_more);
}

/// Post-addressed means the id must actually be a post id. Same rule
/// `tiers.create` applies to its `unlocks_post` designation.
#[tokio::test]
async fn a_malformed_post_id_is_refused() {
    let (router, state) = router_and_state().await;
    let err = tips_list(&router, state, "not-a-post-id", None)
        .await
        .expect_err("a malformed id is refused, not answered empty");
    assert!(
        err.code.contains("malformed"),
        "expected a malformed error, got {}",
        err.code
    );
}

// ── the zap mechanism ────────────────────────────────────────────────────

#[cfg(feature = "nostr")]
mod with_zaps {
    use super::*;

    use fauna_bridge_nostr::signing::Keypair;
    use fauna_bridge_nostr::types::{Event, UnsignedEvent};
    use fauna_nest::nostr::{db, store, zap_ingest};

    use common::zap::{designate_zap_signer as designate, link_payee, zap_receipt};

    /// Store a kind-1 note authored by `author` and map it to `fauna_post_id`
    /// the way the outbound publish path does — this is the link that makes
    /// the read post-addressed.
    async fn publish_note_as(state: &AppState, author: &Keypair, fauna_post_id: &str) -> String {
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
        assert!(
            store::store_event(&conn, &note, false)
                .expect("store note")
                .is_newly_stored(),
            "fixture note must store"
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

    /// Drive a receipt through the REAL ingest gates — the same two calls both
    /// ingress points make: the trust question (*whose signature counts?*) and
    /// the feature gate (*may this payee run the zaps plane, and how much?*). A
    /// test that inserted rows directly would prove nothing about what actually
    /// reaches the tip surface, and one that skipped the second call would
    /// silently stop mirroring production the moment either door changed.
    ///
    /// `false` covers both refusals — untrusted and gated — because from the tip
    /// surface's point of view they are the same fact: no row was written.
    async fn ingest(state: &Arc<AppState>, receipt: &Event) -> bool {
        let conn = state.db.conn().await;
        let verdict = zap_ingest::classify_incoming_zap(&conn, receipt);
        let zap = match verdict {
            fauna_bridge_nostr::nip57::ZapVerdict::Trusted(z) => z,
            _ => {
                drop(conn);
                return false;
            }
        };
        drop(conn);

        if zap_ingest::gate_receipt_ingest(state, &zap).await.is_err() {
            return false;
        }

        let conn = state.db.conn().await;
        zap_ingest::record_zap(&conn, &zap);
        drop(conn);
        true
    }

    /// 21 sats = 21_000 msats. The `n` (nano) HRP multiplier is
    /// `amount x 100` msats, so `210n` is 21_000 — worth pinning in a name,
    /// because guessing it wrong is exactly what this fixture first did.
    const LNBC_21_SATS: &str = "lnbc210n1pjfake";
    /// 1 sat = 1_000 msats.
    const LNBC_1_SAT: &str = "lnbc10n1pjfake";

    #[tokio::test]
    async fn a_believed_zap_surfaces_as_a_tip_on_its_fauna_post() {
        let (router, state) = router_and_state().await;
        let payee_actor = [1u8; 32];
        let payee = link_payee(&state, payee_actor).await;
        let signer = Keypair::generate();
        designate(&state, payee_actor, &signer).await;

        let post_id = a_post_id(0x11);
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &"d".repeat(64),
            Some(LNBC_21_SATS),
            1_700_000_000,
        );
        assert!(
            ingest(&state, &receipt).await,
            "the fixture must be believed"
        );

        // Asked by FAUNA post id — the whole point of the slice.
        let reply = tips_list(&router, state, &post_id, None).await.unwrap();
        assert_eq!(reply.tip_count, 1);
        assert_eq!(reply.total_msats, 21_000, "21 sats in msats");
        assert_eq!(reply.tips.len(), 1);
        assert_eq!(reply.tips[0].mechanism, "nostr_zap");
        assert_eq!(reply.tips[0].amount_msats, Some(21_000));
        assert!(!reply.has_more);
    }

    /// An untrusted receipt contributes nothing — and NOT because this read
    /// filters it. The ingest gate refused to record it at all, which is the
    /// ratified *at ingest, never at read* discipline: every consumer
    /// inherits the guarantee structurally.
    #[tokio::test]
    async fn an_untrusted_receipt_never_reaches_the_tip_surface() {
        let (router, state) = router_and_state().await;
        let payee_actor = [2u8; 32];
        let payee = link_payee(&state, payee_actor).await;
        // Deliberately NOT designated: the payee believes nobody.
        let stranger = Keypair::generate();

        let post_id = a_post_id(0x22);
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &stranger,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &"d".repeat(64),
            Some(LNBC_21_SATS),
            1_700_000_000,
        );
        assert!(
            !ingest(&state, &receipt).await,
            "an undesignated signer must not be believed"
        );

        let reply = tips_list(&router, state, &post_id, None).await.unwrap();
        assert_eq!(reply.tip_count, 0, "nothing was ever recorded to read");
        assert_eq!(reply.total_msats, 0);
    }

    /// The distinction § Tips's attribution consequence depends on: a receipt
    /// whose `bolt11` is absent still says "someone tipped this", so it
    /// counts — but it must not be coerced to a zero-valued tip.
    #[tokio::test]
    async fn an_amountless_tip_counts_but_does_not_sum() {
        let (router, state) = router_and_state().await;
        let payee_actor = [3u8; 32];
        let payee = link_payee(&state, payee_actor).await;
        let signer = Keypair::generate();
        designate(&state, payee_actor, &signer).await;

        let post_id = a_post_id(0x33);
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        for (invoice, at) in [(Some(LNBC_21_SATS), 1_700_000_000), (None, 1_700_000_001)] {
            let receipt = zap_receipt(
                &signer,
                &payee.public_key_hex(),
                &payee.public_key_hex(),
                &note_id,
                &"d".repeat(64),
                invoice,
                at,
            );
            assert!(ingest(&state, &receipt).await);
        }

        let reply = tips_list(&router, state, &post_id, None).await.unwrap();
        assert_eq!(reply.tip_count, 2, "both are tips that happened");
        assert_eq!(
            reply.total_msats, 21_000,
            "the amount-less one adds nothing"
        );
        assert!(
            reply.tips.iter().any(|t| t.amount_msats.is_none()),
            "and it is still returned for attribution"
        );
    }

    /// A tipper whose Nostr pubkey is a linked local account resolves to their
    /// actor; an outside tipper still counts, unattributed but with the
    /// mechanism-native reference a client can render instead of nothing.
    #[tokio::test]
    async fn a_local_tipper_is_attributed_and_an_outside_one_still_counts() {
        let (router, state) = router_and_state().await;
        let payee_actor = [4u8; 32];
        let payee = link_payee(&state, payee_actor).await;
        let signer = Keypair::generate();
        designate(&state, payee_actor, &signer).await;

        let tipper_actor = [5u8; 32];
        let tipper = link_payee(&state, tipper_actor).await;

        let post_id = a_post_id(0x44);
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        for (sender, at) in [
            (tipper.public_key_hex(), 1_700_000_000),
            ("d".repeat(64), 1_700_000_001),
        ] {
            let receipt = zap_receipt(
                &signer,
                &payee.public_key_hex(),
                &payee.public_key_hex(),
                &note_id,
                &sender,
                Some(LNBC_1_SAT),
                at,
            );
            assert!(ingest(&state, &receipt).await);
        }

        let reply = tips_list(&router, state, &post_id, None).await.unwrap();
        assert_eq!(reply.tip_count, 2);
        let attributed: Vec<_> = reply.tips.iter().filter(|t| t.sender.is_some()).collect();
        assert_eq!(attributed.len(), 1, "exactly the local tipper resolves");
        assert_eq!(
            attributed[0].sender.as_ref().map(|a| a.0),
            Some(tipper_actor),
            "and to the right actor"
        );
        assert!(
            reply.tips.iter().all(|t| t.sender_ref.is_some()),
            "both carry the mechanism-native reference"
        );
    }

    /// The window is capped; the totals are not. A post card renders the
    /// totals, so they must stay true however many tips the post has, while
    /// the item list stays bounded — the storm shape this codebase already
    /// had to fix once on the backup list kinds.
    #[tokio::test]
    async fn totals_cover_every_tip_while_the_window_is_capped() {
        let (router, state) = router_and_state().await;
        let payee_actor = [6u8; 32];
        let payee = link_payee(&state, payee_actor).await;
        let signer = Keypair::generate();
        designate(&state, payee_actor, &signer).await;

        let post_id = a_post_id(0x55);
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        for i in 0..5u64 {
            let receipt = zap_receipt(
                &signer,
                &payee.public_key_hex(),
                &payee.public_key_hex(),
                &note_id,
                &"d".repeat(64),
                Some(LNBC_1_SAT),
                1_700_000_000 + i,
            );
            assert!(
                ingest(&state, &receipt).await,
                "fixture {i} must be believed"
            );
        }

        let reply = tips_list(&router, state.clone(), &post_id, Some(2))
            .await
            .unwrap();
        assert_eq!(reply.tips.len(), 2, "the window honours the limit");
        assert!(reply.has_more, "and says there are more");
        assert_eq!(reply.tip_count, 5, "while the count covers every tip");
        assert_eq!(
            reply.total_msats,
            5 * 1_000,
            "and so does the total, not just the window"
        );

        let all = tips_list(&router, state, &post_id, Some(50)).await.unwrap();
        assert_eq!(all.tips.len(), 5);
        assert!(!all.has_more);
    }

    /// A zap on a note this box never mapped to a Fauna post is not a tip on
    /// any Fauna post — the map is the link, and an unmapped note yields
    /// nothing rather than leaking into some other post's totals.
    #[tokio::test]
    async fn an_unmapped_note_contributes_to_no_fauna_post() {
        let (router, state) = router_and_state().await;
        let payee_actor = [8u8; 32];
        let payee = link_payee(&state, payee_actor).await;
        let signer = Keypair::generate();
        designate(&state, payee_actor, &signer).await;

        // Stored and zapped, but deliberately never mapped.
        let note = payee.sign_event(UnsignedEvent {
            pubkey: payee.public_key_bytes(),
            created_at: 1_700_000_000,
            kind: 1,
            tags: vec![],
            content: "an unmapped note".into(),
        });
        {
            let conn = state.db.conn().await;
            store::store_event(&conn, &note, false).expect("store note");
        }
        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note.id,
            &"d".repeat(64),
            Some(LNBC_21_SATS),
            1_700_000_000,
        );
        assert!(ingest(&state, &receipt).await, "it IS a believed zap");

        let reply = tips_list(&router, state, &a_post_id(0x66), None)
            .await
            .unwrap();
        assert_eq!(reply.tip_count, 0);
        assert_eq!(reply.total_msats, 0);
    }
}
