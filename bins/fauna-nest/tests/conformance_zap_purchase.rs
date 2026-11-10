//! The zap **purchase** leg — the tip↔purchase split at ingest
//! (`docs/goal/behavior/monetization.md` § The asking price, § Per-post
//! pay-to-unlock).
//!
//! § Per-post pay-to-unlock ratifies that a zap receipt targeting a **sold**
//! post reduces to the same *(payee, tier)* entitlement every other mechanism
//! produces — and that a receipt on a post nothing sells, or below the asking
//! price, or on a tier with no asking price set, stays a tip.
//!
//! **A tier sells a post iff the post is gated to that tier AND that tier
//! designates that post**. The
//! designation alone is a one-way client claim — unverifiable when made and
//! not unique — so resolving on it classed a zap on any ordinary public post
//! as a sale, which then erased that zap from the post's tip list. Every
//! fixture below therefore builds a **real** sold post through
//! `CacheDb::put_post`, so `content_meta.gated_tier` is production-written;
//! a sellable post's outbound `nostr_event_map` row is produced by the
//! production advertise act (`advertise_for_real`, the real
//! `fauna.nostr.advertise.set` handler, 2026-11-07), and only a post the act
//! refuses gets a hand-made one, for the reason `publish_note_as` gives.
//!
//! **Why these are flow tests, not unit tests.** The comparison itself is
//! pinned nine ways in `fauna_payments::asking_price` (including a
//! three-mutation grading of its conjuncts). What *cannot* be proven there is
//! the chain this suite walks: a signed receipt → the real ingest gate → the
//! outbound event map → the payee's linked account → the designated tier →
//! its asking price → the payment waist → a subscriber row. Every link is a
//! place the leg can be wired wrong while every unit test stays green, which
//! is exactly the class the 97th pass's own post-mortem named ("a leg can be
//! landed, green, documented, and have never once executed").
//!
//! Coverage:
//!   * a receipt meeting the asking price **grants the tier** and is recorded
//!     as a purchase, not a tip — the post's tip total must not count a sale;
//!   * one msat under the price stays a tip and grants nothing (fail-closed,
//!     and specifically NOT a refusal — the sats are still attributed);
//!   * a designated tier with **no** asking price stays a tip however large
//!     the zap — inferred sale is opt-in;
//!   * a zap on an **undesignated** post stays a tip (nothing to buy);
//!   * an **amount-less** receipt stays a tip (no number to compare);
//!   * a tipper with no local account still buys — through the waist's
//!     claim-code fallback rather than a dropped payment;
//!   * a redelivered receipt is **idempotent**: the same purchase, once;
//!   * a zap on an **ungated** post is never a purchase, however a tier was
//!     designated — and its attribution survives on the tip surface;
//!   * a tier may only sell the post it actually **gates**: a post gated to an
//!     ordinary subscription tier is subscription content, not a sale.

// The whole suite drives the zap adapter, so it only exists with it — and
// gating the file (not just the module) keeps the harness below from being
// dead code on nostr-less feature builds.
#![cfg(feature = "nostr")]

mod common;

use std::sync::Arc;

use fauna_core::feature_gate::{
    Availability, FeaturePolicy, GatedFeature, RuleTier, Window, WindowedBounds,
};
use fauna_nest::routes::AppState;

// ── harness ──────────────────────────────────────────────────────────────

/// One field of a typed Dim-4 refusal's details — the surface, tier, dimension
/// and bound a client renders "limited by …" from.
fn detail(error: &fauna_protocol::RpcError, key: &str) -> fauna_protocol::Value {
    let Some(boxed) = &error.details else {
        panic!("refusal carries no details: {}", error.code);
    };
    let fauna_protocol::Value::Map(map) = boxed.as_ref() else {
        panic!("refusal details are not a map");
    };
    map.get(key)
        .unwrap_or_else(|| panic!("refusal details carry no {key}"))
        .clone()
}

async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    fauna_nest::nostr::init_db(&db)
        .await
        .expect("init nostr tables");
    Arc::new(AppState::for_test(db))
}

mod purchase {
    use super::*;

    use fauna_bridge_nostr::signing::Keypair;
    use fauna_bridge_nostr::types::{Event, UnsignedEvent};
    use fauna_nest::nostr::{db, store, zap_ingest};
    use fauna_protocol::subscriptions::TierAskingPrice;

    use common::zap::{
        designate_zap_signer as designate, link_payee, link_payee_with_nsec, zap_receipt,
    };

    /// 21 sats = 21_000 msats. The `n` (nano) HRP multiplier is `amount x 100`
    /// msats, so `210n` is 21_000.
    const LNBC_21_SATS: &str = "lnbc210n1pjfake";
    /// 20 sats = 20_000 msats — one sat under a 21-sat asking price.
    const LNBC_20_SATS: &str = "lnbc200n1pjfake";

    /// Advertise a real sold post through **the production act** —
    /// `fauna.nostr.advertise.set`'s real handler — and return the event id
    /// the materializer signed and mapped `outbound` (`monetization.md` § The
    /// Nostr advertisement of a sold post). This is the ingress that makes a
    /// zap receipt able to name a sold post at all; nothing in the chain below
    /// it is hand-made. The payee must hold a deposited key
    /// (`link_payee_with_nsec`).
    async fn advertise_for_real(state: &Arc<AppState>, author: [u8; 32], post_id: &str) -> String {
        let _ = state.db.create_user(&author, "free", "payee").await;
        let mut b = fauna_nest::rpc_router::RpcRouter::builder();
        fauna_nest::nostr::advertise::register_nostr_advertise_handlers(&mut b);
        let router = b.build();
        let req = fauna_protocol::nostr::AdvertiseSetRequest {
            post_id: post_id.into(),
            extra: Default::default(),
        };
        common::call_raw(
            &router,
            state.clone(),
            "fauna.nostr.advertise.set",
            author,
            bytes::Bytes::from(fauna_protocol::encode_canonical(&req).unwrap().to_vec()),
        )
        .await
        .expect("a sold, priced post advertises");
        let conn = state.db.conn().await;
        let ids = db::list_event_ids_by_fauna_id(&conn, post_id).expect("read the map");
        drop(conn);
        assert_eq!(ids.len(), 1, "the act maps exactly one outbound event");
        ids[0].clone()
    }

    /// Give a post **no tier sells** a Nostr presence a receipt's `e` tag can
    /// name — for the cases where the post cannot be advertised (ungated,
    /// undesignated, priceless, gated to another tier), so the act refuses it
    /// and production gives it no outbound row as a sold post.
    ///
    /// ⚠ **Hand-made, deliberately, as defence in depth.** These tests prove
    /// that a receipt naming such a post stays a tip *even if* something mapped
    /// it — the split must not lean on the advertise act's refusals alone. A
    /// sellable post's link is never made here: it goes through
    /// [`advertise_for_real`]. Everything else in these tests — the post, its
    /// gate, the tier, the prices, the ingest path — is production-written.
    async fn publish_note_as(state: &AppState, author: &Keypair, fauna_post_id: &str) -> String {
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let note = author.sign_event(UnsignedEvent {
            pubkey: author.public_key_bytes(),
            created_at: 1_700_000_000,
            kind: 1,
            tags: vec![],
            content: format!("a sellable note {n}"),
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

    /// Write a post through the **production** `CacheDb::put_post` path, which
    /// runs `extract_post_metadata` → `write_post_index` and so projects
    /// `Post.gated.tier` into `content_meta.gated_tier` itself. `gate` names the
    /// tier the post is gated to; `None` writes an ordinary public post.
    ///
    /// Returns the post's real content-addressed id (hex) — the same
    /// `blake3(body)` derivation `prepare_sell_post` takes, so the tier's
    /// `unlocks_post` designation below names a post that genuinely exists and
    /// genuinely carries the gate it claims.
    async fn put_post_gated_to(state: &AppState, author: [u8; 32], gate: Option<&str>) -> String {
        use fauna_core::data::{ContentHash, Post, PostBody, Timestamp};
        use fauna_core::identity::ActorId;
        use fauna_core::subscription::types::{GatedInfo, KeyAccess};

        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let post = Post {
            author: ActorId(author),
            created_at: Timestamp(1_700_000_000_000_000 + n),
            body: PostBody::Text {
                // A gated post's `body` IS the public teaser — the sealed half
                // lives at `encrypted_ref` (`GatedInfo` doc comment).
                content: format!("public teaser {n}"),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: gate.map(|tier| GatedInfo {
                encrypted_ref: ContentHash::from_digest_raw([9u8; 32]),
                key_access: KeyAccess::Broadcast {
                    key_blob_ref: ContentHash::from_digest_raw([8u8; 32]),
                },
                tier: tier.to_string(),
                tier_rank: 2,
                seal_id: ContentHash::from_digest_raw([7u8; 32]),
                attachment_refs: vec![],
            }),
            content_warning: None,
            origin: None,
        };
        let cid = fauna_core::encoding::compute_post_id(&post).expect("compute post id");
        let digest: [u8; 32] = cid.as_bytes()[4..].try_into().expect("32-byte digest");
        let bytes = fauna_core::encoding::canonical_encode(&post).expect("encode post");
        state
            .db
            .put_post(&digest, &bytes, None)
            .await
            .expect("write the post through the production path");
        hex::encode(digest)
    }

    /// Author a **real** sold post: the gated post the "sell this post" flow
    /// produces, plus the degenerate single-post tier that designates it —
    /// both directions of the binding written by the code that writes them in
    /// production. Returns the post id.
    async fn sell_post_for_real(
        state: &AppState,
        author: [u8; 32],
        tier: &str,
        asking: Option<TierAskingPrice>,
    ) -> String {
        let post_id = put_post_gated_to(state, author, Some(tier)).await;
        sell_post(state, author, tier, &post_id, asking).await;
        post_id
    }

    /// Mint the degenerate single-post tier the "sell this post" flow mints,
    /// optionally priced.
    async fn sell_post(
        state: &AppState,
        author: [u8; 32],
        tier: &str,
        post_id: &str,
        asking: Option<TierAskingPrice>,
    ) {
        state
            .db
            .create_subscription_tier(
                &author,
                tier,
                2,
                None,
                Some("21 sats"),
                None,
                false,
                Some(post_id),
                asking.as_ref(),
                false,
            )
            .await
            .expect("mint the unlock tier");
    }

    /// Drive a receipt through **the real ingest path**, exactly as both
    /// ingress points do: classify → **gate** → resolve → apply → record.
    /// Open-coding a row insert here would prove nothing about what actually
    /// happens.
    ///
    /// `None` means the receipt was refused by the `zaps.receipt.ingest` gate
    /// and nothing was written — distinct from `Ok(None)`'s "believed, but a
    /// tip". Both doors return the same way, which is why the gate belongs in
    /// this mirror: a helper that skipped it would let every test below pass
    /// against an ungated path neither production door takes.
    async fn ingest_gated(
        state: &Arc<AppState>,
        receipt: &Event,
    ) -> Result<Option<String>, fauna_protocol::RpcError> {
        let conn = state.db.conn().await;
        let verdict = zap_ingest::classify_incoming_zap(&conn, receipt);
        let zap = match verdict {
            fauna_bridge_nostr::nip57::ZapVerdict::Trusted(z) => z,
            other => panic!("fixture receipt must be believed, got {other:?}"),
        };
        let subject = zap_ingest::resolve_zap_subject(&conn, &zap);
        drop(conn);

        zap_ingest::gate_receipt_ingest(state, &zap).await?;

        let purchased = match subject.as_ref() {
            Some(s) => zap_ingest::apply_zap_purchase(state, &zap, s).await,
            None => None,
        };
        let conn = state.db.conn().await;
        let newly = zap_ingest::record_zap_as(&conn, &zap, purchased.as_deref());
        drop(conn);
        zap_ingest::notify_tip(state, &zap, subject.as_ref(), purchased.as_deref(), newly).await;
        Ok(purchased)
    }

    /// How many `tip` notifications the author holds — a sale must ring none.
    async fn tip_notifications(state: &AppState, author: [u8; 32]) -> usize {
        state
            .db
            .list_notifications(&author, None, 50)
            .await
            .expect("list notifications")
            .into_iter()
            .filter(|n| n.notif_type == fauna_protocol::notifications::NotifType::Tip)
            .count()
    }

    /// The ordinary form: the receipt is expected to pass the gate.
    async fn ingest(state: &Arc<AppState>, receipt: &Event) -> Option<String> {
        ingest_gated(state, receipt)
            .await
            .expect("fixture receipt must pass the feature gate")
    }

    /// The tips the post-addressed read would report — the surface a purchase
    /// must NOT appear on.
    async fn tips_on(state: &AppState, fauna_post_id: &str) -> Vec<i64> {
        let conn = state.db.conn().await;
        let rows = db::list_tips_for_fauna_post(&conn, fauna_post_id).expect("list tips");
        drop(conn);
        rows.into_iter().filter_map(|r| r.amount_msats).collect()
    }

    /// Did the purchase reach the buyer as an entitlement?
    ///
    /// **The oracle is the enqueued `payment_entitled` request, NOT a
    /// `subscribers` row** — and getting that wrong is the whole trap here.
    /// Every tier is *client*-minted: the nest holds no period key, so it
    /// cannot write a readable subscriber row itself. `grant_paid_entitlement`
    /// therefore answers `queued: true` and enqueues a `payment_entitled`
    /// request the author's client drains into a KeyBlob (`monetization.md`
    /// § Pillar 1 — the accepted mint-latency bound). Asserting on
    /// `subscribers` would fail on a *correct* purchase — i.e. it would answer
    /// a question next to the one being asked.
    async fn entitled_rows(
        state: &AppState,
        author: [u8; 32],
        buyer: [u8; 32],
        tier: &str,
    ) -> usize {
        state
            .db
            .list_subscribe_requests(&author)
            .await
            .expect("list subscribe requests")
            .iter()
            .filter(|r| {
                r.payment_entitled && r.tier_name == tier && r.subscriber_id == buyer.to_vec()
            })
            .count()
    }

    async fn is_entitled(state: &AppState, author: [u8; 32], buyer: [u8; 32], tier: &str) -> bool {
        entitled_rows(state, author, buyer, tier).await > 0
    }

    // ── the ratified split ───────────────────────────────────────────────

    #[tokio::test]
    async fn a_zap_meeting_the_asking_price_buys_the_tier_and_is_not_a_tip() {
        let state = state().await;
        let author = [1u8; 32];
        let buyer = [2u8; 32];
        let payee = link_payee_with_nsec(&state, author, None).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-01",
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_000_100,
        );

        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-01"),
            "a receipt at exactly the asking price is a purchase (the rule is `>=`)"
        );
        assert!(
            is_entitled(&state, author, buyer, "post-unlock-01").await,
            "the purchase must reduce to an ordinary entitlement on the unlock tier — \
             the whole point of routing zaps through the unchanged waist. For a \
             client-minted tier that entitlement is an enqueued `payment_entitled` \
             request the author's client drains, not a subscriber row the nest \
             could not mint a key for."
        );
        assert!(
            tips_on(&state, &post_id).await.is_empty(),
            "a sale is not appreciation: counting the purchase as a tip would inflate \
             the post's tip total with its own sale price"
        );
        assert_eq!(
            tip_notifications(&state, author).await,
            0,
            "a purchase is a grant, not a tip: it must not ring the tip notification"
        );
    }

    #[tokio::test]
    async fn one_msat_under_the_price_stays_a_tip_and_grants_nothing() {
        let state = state().await;
        let author = [3u8; 32];
        let buyer = [4u8; 32];
        let payee = link_payee_with_nsec(&state, author, None).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-02",
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_20_SATS),
            1_700_000_200,
        );

        assert_eq!(ingest(&state, &receipt).await, None, "under-threshold");
        assert!(
            !is_entitled(&state, author, buyer, "post-unlock-02").await,
            "an under-threshold zap must not unlock the post"
        );
        // The load-bearing half: NOT a refusal. A zap is irrevocable, so
        // refusing returns no sats and only destroys the attribution the
        // sender is owed (§ The asking price — the rejected alternative).
        assert_eq!(
            tips_on(&state, &post_id).await,
            vec![20_000],
            "the under-threshold zap is still recorded and attributed as a tip"
        );
        assert_eq!(
            tip_notifications(&state, author).await,
            1,
            "a zap that stays a tip rings its author"
        );
    }

    #[tokio::test]
    async fn a_designated_tier_with_no_asking_price_can_never_be_bought_by_zap() {
        let state = state().await;
        let author = [5u8; 32];
        let buyer = [6u8; 32];
        let payee = link_payee(&state, author).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        // Priced only in the HUMAN field: `price_hint` is free text and no
        // free-text parsing ever infers a number (§ The asking price).
        let post_id = sell_post_for_real(&state, author, "post-unlock-03", None).await;
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_000_300,
        );

        assert_eq!(
            ingest(&state, &receipt).await,
            None,
            "inferred sale is OPT-IN: a tier with only a price_hint is unbuyable by zap, \
             however large the amount"
        );
        assert!(!is_entitled(&state, author, buyer, "post-unlock-03").await);
        assert_eq!(tips_on(&state, &post_id).await, vec![21_000]);
    }

    #[tokio::test]
    async fn a_zap_on_an_undesignated_post_stays_a_tip() {
        let state = state().await;
        let author = [7u8; 32];
        let buyer = [8u8; 32];
        let payee = link_payee(&state, author).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        // No unlock tier at all — the ordinary public post everybody zaps.
        let post_id = put_post_gated_to(&state, author, None).await;
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_000_400,
        );

        assert_eq!(ingest(&state, &receipt).await, None);
        assert_eq!(
            tips_on(&state, &post_id).await,
            vec![21_000],
            "the ordinary case: nothing is for sale, so the zap is pure appreciation"
        );
    }

    #[tokio::test]
    async fn an_amount_less_receipt_stays_a_tip_even_on_a_priced_post() {
        let state = state().await;
        let author = [9u8; 32];
        let buyer = [10u8; 32];
        let payee = link_payee_with_nsec(&state, author, None).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-05",
            // Zero: "any amount in this unit buys it". Even THIS must not be
            // met by a receipt carrying no amount — absent is not zero, and
            // treating it as zero would hand the post to anyone who can mint
            // an amount-less receipt.
            Some(TierAskingPrice::msats(0)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            None,
            1_700_000_500,
        );

        assert_eq!(
            ingest(&state, &receipt).await,
            None,
            "`bolt11` is optional in the wild; an absent amount is not a zero amount"
        );
        assert!(!is_entitled(&state, author, buyer, "post-unlock-05").await);
    }

    #[tokio::test]
    async fn a_buyer_with_no_local_account_still_buys_through_the_claim_fallback() {
        let state = state().await;
        let author = [11u8; 32];
        let payee = link_payee_with_nsec(&state, author, None).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;
        // A tipper from outside this box: a real Nostr key with no local
        // account behind it.
        let stranger = Keypair::generate();

        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-06",
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &stranger,
            Some(LNBC_21_SATS),
            1_700_000_600,
        );

        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-06"),
            "an unbound buyer really did pay — the waist mints a claim code rather \
             than dropping the payment (§ Pillar 3 Q4)"
        );
        assert!(
            tips_on(&state, &post_id).await.is_empty(),
            "still a purchase, not a tip, even though no actor was bound"
        );
    }

    #[tokio::test]
    async fn a_redelivered_receipt_is_the_same_purchase_once() {
        let state = state().await;
        let author = [12u8; 32];
        let buyer = [13u8; 32];
        let payee = link_payee_with_nsec(&state, author, None).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-07",
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_000_700,
        );

        // Both ingress points can see the same receipt, and the sweep re-reads
        // relays on a timer — redelivery is the normal case, not an edge one.
        // It is also the crash-recovery path: apply-then-record means a crash
        // between the two leaves no row, and the retry must converge.
        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-07")
        );
        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-07"),
            "a redelivered receipt names the same purchase"
        );

        assert_eq!(
            entitled_rows(&state, author, buyer, "post-unlock-07").await,
            1,
            "one buyer, one enqueued entitlement — a redelivered receipt must not \
             enqueue a second mint for the same purchase"
        );
        assert!(tips_on(&state, &post_id).await.is_empty());
    }

    // ── the designation is corroboration, never authority ────────────────

    #[tokio::test]
    async fn a_zap_on_an_ungated_post_is_never_a_purchase_however_the_tier_was_designated() {
        let state = state().await;
        let author = [14u8; 32];
        let buyer = [15u8; 32];
        let payee = link_payee(&state, author).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        // An ordinary PUBLIC post — nothing about it is for sale.
        let post_id = put_post_gated_to(&state, author, None).await;
        let note_id = publish_note_as(&state, &payee, &post_id).await;
        // …and a tier claiming to unlock it. `unlocks_post` is a one-way client
        // claim: `tiers.create` validates its FORMAT only (deliberately — the
        // post does not exist yet at create time), so a tier may name any id.
        sell_post(
            &state,
            author,
            "claims-to-unlock",
            &post_id,
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_000_800,
        );

        assert_eq!(
            ingest(&state, &receipt).await,
            None,
            "a post that is not gated is not for sale, whatever a tier claims about \
             it — the content-addressed gate is the authority and the designation is \
             only corroboration"
        );
        assert!(
            !is_entitled(&state, author, buyer, "claims-to-unlock").await,
            "an unsold post must not entitle anyone to anything"
        );
        // The security-shaped half: mis-classing a tip as a sale erases it from
        // the post's tip list (`AND z.purchased_tier IS NULL`), which is exactly
        // the attribution loss § The asking price forbids when it rejects
        // refusing an under-threshold receipt.
        assert_eq!(
            tips_on(&state, &post_id).await,
            vec![21_000],
            "the zap is an ordinary tip and must stay visible on the post — a \
             misclassification must never destroy attribution the sender is owed"
        );
    }

    #[tokio::test]
    async fn a_tier_may_only_sell_the_post_it_actually_gates() {
        let state = state().await;
        let author = [16u8; 32];
        let buyer = [17u8; 32];
        let payee = link_payee(&state, author).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        // The post is gated to an ordinary subscription tier. Its subscribers
        // read it because they subscribe — it is not individually for sale.
        let post_id = put_post_gated_to(&state, author, Some("gold")).await;
        state
            .db
            .create_subscription_tier(
                &author,
                "gold",
                1,
                None,
                Some("5000 sats/mo"),
                None,
                false,
                // Designates nothing: an ordinary tier, priced.
                None,
                Some(&TierAskingPrice::msats(21_000)),
                false,
            )
            .await
            .expect("mint the regular tier");
        // A second tier claims the same post. Nothing at the DB layer forbids
        // it: `PRIMARY KEY (author_id, name)` is the only constraint, so
        // `unlocks_post` is not unique.
        sell_post(
            &state,
            author,
            "claims-to-unlock",
            &post_id,
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_000_900,
        );

        assert_eq!(
            ingest(&state, &receipt).await,
            None,
            "both directions must agree: the post's own gate names `gold`, which \
             designates no post, and the tier that designates the post does not \
             gate it. Neither is the sold-post pair, so nothing is bought"
        );
        assert!(!is_entitled(&state, author, buyer, "claims-to-unlock").await);
        assert!(
            !is_entitled(&state, author, buyer, "gold").await,
            "a zap must never buy a subscription tier: § Per-post pay-to-unlock's \
             designation promises AT-LEAST-this-post, so a post gated to a regular \
             tier is subscription content, not a single-post sale"
        );
        assert_eq!(tips_on(&state, &post_id).await, vec![21_000]);
    }

    // ── the feature gate at ingest (`dynamic-features.md` § Evaluation points)

    /// **Gate surface `zaps.receipt.ingest`.** A `zaps` deny refuses the receipt
    /// at the door: no accounting row, no tip, no purchase.
    ///
    /// This is the Damus shape as a runtime property — the zap surface gone
    /// while the rest of the product stands — and the floor's whole point is
    /// that it binds without the client having read `fauna.features.status`
    /// first: nothing in this test asks the nest anything before zapping.
    #[tokio::test]
    async fn a_zaps_deny_refuses_the_receipt_at_ingest() {
        let state = state().await;
        let author = [11u8; 32];
        let buyer = [12u8; 32];
        let payee = link_payee_with_nsec(&state, author, None).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-deny",
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;

        state
            .db
            .put_feature_policy(
                RuleTier::Admin,
                &author,
                GatedFeature::Zaps,
                &FeaturePolicy {
                    availability: Availability::Deny,
                    ..FeaturePolicy::NO_OPINION
                },
            )
            .await
            .unwrap();

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_001_000,
        );

        let error = ingest_gated(&state, &receipt)
            .await
            .expect_err("a denied zaps plane must refuse the receipt");
        assert_eq!(error.code, fauna_nest::feature_gate::CODE_FEATURE_DENIED);
        assert_eq!(
            detail(&error, "surface"),
            fauna_protocol::Value::String("zaps.receipt.ingest".into())
        );
        assert_eq!(
            detail(&error, "tier"),
            fauna_protocol::Value::String("admin".into()),
            "boundary 4 — the payee must be able to see WHICH tier bound them"
        );

        assert_eq!(
            tips_on(&state, &post_id).await,
            Vec::<i64>::new(),
            "a refused receipt leaves nothing behind: not even a tip row"
        );
        assert!(!is_entitled(&state, author, buyer, "post-unlock-deny").await);
    }

    /// **Gate surface `payments.unlock.purchase`, reached from the zap leg.** A
    /// zap that meets the asking price while the *payments* plane is over quota
    /// degrades to a **tip** — attribution kept, entitlement refused.
    ///
    /// Two ratified rules meet here and this pins their intersection: quota
    /// bounds never inherit across the subset edge (so a `payments` bound binds
    /// the purchase while the `zaps` ingest still passes), and a believed zap is
    /// never dropped for failing on the payments plane's terms ("the sats really
    /// did arrive, so the attribution is owed").
    #[tokio::test]
    async fn a_payments_quota_degrades_a_zap_purchase_to_a_tip() {
        let state = state().await;
        let author = [13u8; 32];
        let buyer = [14u8; 32];
        let payee = link_payee_with_nsec(&state, author, None).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;

        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-quota",
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;

        // Nothing may buy: a payments operations bound of 0 for the payee.
        state
            .db
            .put_feature_policy(
                RuleTier::SelfImposed,
                &author,
                GatedFeature::Payments,
                &FeaturePolicy {
                    availability: Availability::Limit,
                    operations: WindowedBounds::UNSET.tightened_with(Window::Day, 0),
                    ..FeaturePolicy::NO_OPINION
                },
            )
            .await
            .unwrap();

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_001_100,
        );

        assert_eq!(
            ingest_gated(&state, &receipt).await.expect(
                "the zaps plane itself is untouched — a payments bound is not a zaps bound"
            ),
            None,
            "the purchase was refused, so the receipt stays a tip"
        );
        assert!(
            !is_entitled(&state, author, buyer, "post-unlock-quota").await,
            "no entitlement may be granted past the payments bound"
        );
        assert_eq!(
            tips_on(&state, &post_id).await,
            vec![21_000],
            "the sats arrived, so the attribution is owed regardless"
        );
    }

    // ── delivering the unlock to a stranger who paid ─────────────────────
    //
    // `monetization.md` § The Nostr advertisement of a sold post → *Delivering
    // the unlock to a stranger who paid*: an unbound buyer whose receipt names
    // a self-certified sender gets the claim code as a NIP-17 DM from the
    // payee's custodial key, through the Nostr DM leg's outbox, once per zap.
    //
    // The DM's sentence is a public text that only its public-texts round
    // lands in the catalog (`nostr::claim_delivery`). Until then the ingest
    // path sends nothing, so the mechanism's cases drive the send half
    // (`deliver_claim_message`) with a stand-in body, and the ingest cases
    // assert whichever the catalog decides — and the no-delivery cases hold
    // either way.

    use fauna_nest::db::bridged_conversations::{NOSTR_LEG_BRIDGE_ID, NOSTR_LEG_PRINCIPAL_ID};
    use fauna_nest::nostr::claim_delivery::{
        ClaimDelivery, claim_delivery_body, deliver_claim_message,
    };

    /// The recipient key's MSEK every delivery fixture seeds for the payee.
    const PAYEE_MSEK: [u8; 32] = [0x5e; 32];

    /// A payee who can send on the leg: a deposited Nostr key (default
    /// relays) and a recipient seal key for the Sent copy.
    async fn sending_payee(state: &Arc<AppState>, author: [u8; 32]) -> Keypair {
        let payee = link_payee_with_nsec(state, author, None).await;
        common::seed_recipient_seal_key(&state.db, &author, &PAYEE_MSEK).await;
        payee
    }

    /// Every item queued on the Nostr leg's outbox, opened under the leg's
    /// key: (account, far room id = the recipient pubkey, text).
    async fn leg_outbox(state: &AppState) -> Vec<([u8; 32], String, String)> {
        let items = state
            .db
            .fetch_principal_outbox(&NOSTR_LEG_PRINCIPAL_ID, 50)
            .await
            .expect("read the leg's outbox");
        let (secret, _) = state
            .db
            .first_party_bridge_key(NOSTR_LEG_BRIDGE_ID)
            .await
            .expect("the leg's key");
        items
            .into_iter()
            .map(|(actor, item)| {
                let text = fauna_nest::bridge_legs::open_outbox_text(&item.ciphertext, &secret)
                    .expect("the item opens under the leg's key, as the drain opens it");
                (actor.try_into().unwrap(), item.far_room_id, text)
            })
            .collect()
    }

    /// The payee's Sent rows on the leg, opened with their recipient key.
    async fn sent_texts(state: &AppState, author: [u8; 32]) -> Vec<String> {
        let conn = state.db.conn().await;
        let mut stmt = conn
            .prepare(
                "SELECT sealed_content FROM bridge_conversation_messages
                  WHERE actor_id = ?1 AND direction = 'out' ORDER BY id",
            )
            .unwrap();
        let blobs: Vec<Vec<u8>> = stmt
            .query_map([&author[..]], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(stmt);
        drop(conn);
        blobs
            .iter()
            .map(|b| String::from_utf8(common::open_recipient_record(b, &PAYEE_MSEK)).unwrap())
            .collect()
    }

    /// The claim codes the waist minted for the payee.
    async fn minted_codes(state: &AppState, author: [u8; 32]) -> Vec<String> {
        state
            .db
            .list_payment_claims(&author)
            .await
            .expect("list claims")
            .into_iter()
            .map(|c| c.code)
            .collect()
    }

    /// A stranger's purchase of an advertised post, through the real ingest
    /// path. Returns (payee key, the stranger's key, the receipt, the post id).
    async fn stranger_buys(
        state: &Arc<AppState>,
        author: [u8; 32],
        tier: &str,
    ) -> (Keypair, Keypair, Event, String) {
        let payee = sending_payee(state, author).await;
        let signer = Keypair::generate();
        designate(state, author, &signer).await;
        let stranger = Keypair::generate();
        let post_id =
            sell_post_for_real(state, author, tier, Some(TierAskingPrice::msats(21_000))).await;
        let note_id = advertise_for_real(state, author, &post_id).await;
        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &stranger,
            Some(LNBC_21_SATS),
            1_700_002_000,
        );
        (payee, stranger, receipt, post_id)
    }

    #[tokio::test]
    async fn the_delivery_queues_one_dm_to_the_buyer_and_a_sent_copy_for_the_author() {
        let state = state().await;
        let author = [40u8; 32];
        let payee = sending_payee(&state, author).await;
        let stranger = Keypair::generate();
        let body = "stand-in sentence: your code is ABCD-2345";

        let outcome = deliver_claim_message(
            &state,
            "zap-event-40",
            &author,
            &payee.public_key_hex(),
            &stranger.public_key_hex(),
            body,
        )
        .await
        .expect("deliver");
        assert!(matches!(outcome, ClaimDelivery::Queued(_)), "{outcome:?}");

        assert_eq!(
            leg_outbox(&state).await,
            vec![(author, stranger.public_key_hex(), body.to_string())],
            "one item on the leg's outbox, from the payee's account to the buyer's \
             key, opening under the leg's key to the body — what the drain \
             gift-wraps with the payee's custodial key"
        );
        assert_eq!(
            sent_texts(&state, author).await,
            vec![body.to_string()],
            "the author's Sent copy, sealed to their own recipient key"
        );
        let rooms = state.db.list_bridged_rooms(&author).await.unwrap();
        assert_eq!(rooms.len(), 1, "one room, with the buyer");
        assert_eq!(rooms[0].far_room_id, stranger.public_key_hex());
        assert_eq!(rooms[0].participants, vec![stranger.public_key_hex()]);
        assert_eq!(
            rooms[0].self_address.as_deref(),
            Some(payee.public_key_hex().as_str())
        );
    }

    /// A redelivered receipt, or a crash replay that finds the code already
    /// minted (`ClaimExists`), reaches the delivery again: the zap's own
    /// delivery record — written with the send — makes it once.
    #[tokio::test]
    async fn a_second_delivery_for_the_same_zap_sends_nothing() {
        let state = state().await;
        let author = [41u8; 32];
        let payee = sending_payee(&state, author).await;
        let stranger = Keypair::generate();
        for expect_first in [true, false] {
            let outcome = deliver_claim_message(
                &state,
                "zap-event-41",
                &author,
                &payee.public_key_hex(),
                &stranger.public_key_hex(),
                "stand-in sentence: ABCD-3456",
            )
            .await
            .expect("deliver");
            if expect_first {
                assert!(matches!(outcome, ClaimDelivery::Queued(_)), "{outcome:?}");
            } else {
                assert_eq!(outcome, ClaimDelivery::AlreadyDelivered);
            }
        }
        assert_eq!(leg_outbox(&state).await.len(), 1, "one DM, not two");
        assert_eq!(sent_texts(&state, author).await.len(), 1, "one Sent copy");

        // A different zap from the same buyer is a different purchase.
        let outcome = deliver_claim_message(
            &state,
            "zap-event-41b",
            &author,
            &payee.public_key_hex(),
            &stranger.public_key_hex(),
            "stand-in sentence: ABCD-4567",
        )
        .await
        .expect("deliver");
        assert!(matches!(outcome, ClaimDelivery::Queued(_)), "{outcome:?}");
        assert_eq!(leg_outbox(&state).await.len(), 2);
        assert_eq!(
            state.db.list_bridged_rooms(&author).await.unwrap().len(),
            1,
            "both in the one room with the buyer"
        );
    }

    /// A payee with no recipient seal key cannot rest a Sent copy sealed, so
    /// nothing is queued — and nothing is recorded, so a later replay may send.
    #[tokio::test]
    async fn a_payee_with_no_seal_key_sends_nothing_and_records_nothing() {
        let state = state().await;
        let author = [42u8; 32];
        let payee = link_payee_with_nsec(&state, author, None).await;
        let stranger = Keypair::generate();
        let (payee_hex, stranger_hex) = (payee.public_key_hex(), stranger.public_key_hex());
        let deliver = || {
            deliver_claim_message(
                &state,
                "zap-event-42",
                &author,
                &payee_hex,
                &stranger_hex,
                "stand-in sentence: ABCD-5678",
            )
        };
        assert_eq!(deliver().await.unwrap(), ClaimDelivery::NoSealKey);
        assert!(leg_outbox(&state).await.is_empty());

        common::seed_recipient_seal_key(&state.db, &author, &PAYEE_MSEK).await;
        assert!(
            matches!(deliver().await.unwrap(), ClaimDelivery::Queued(_)),
            "the refused attempt left no record behind"
        );
    }

    /// The ingest path end to end: a stranger's purchase with a certified
    /// sender delivers the code the waist minted — once, however often the
    /// receipt arrives — as soon as the catalog holds the sentence. The post
    /// is not web-published, so the message is the code-only sentence and
    /// names no URL.
    #[tokio::test]
    async fn a_strangers_purchase_delivers_the_minted_code_once() {
        let state = state().await;
        let author = [43u8; 32];
        let (_payee, stranger, receipt, _post) =
            stranger_buys(&state, author, "post-unlock-dm-1").await;

        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-dm-1")
        );
        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-dm-1")
        );

        let codes = minted_codes(&state, author).await;
        assert_eq!(codes.len(), 1, "one purchase, one claim code");
        let outbox = leg_outbox(&state).await;
        match claim_delivery_body(&codes[0], None) {
            Some(body) => {
                assert_eq!(
                    outbox,
                    vec![(author, stranger.public_key_hex(), body)],
                    "one DM to the buyer's key carrying the minted code"
                );
                assert!(outbox[0].2.contains(&codes[0]));
                assert!(
                    !outbox[0].2.contains("https://"),
                    "an unpublished post has no page: the code-only sentence"
                );
                assert_eq!(sent_texts(&state, author).await.len(), 1);
            }
            None => assert!(
                outbox.is_empty(),
                "the sentence awaits its public-texts round: no invented prose is sent"
            ),
        }
    }

    /// A stranger's purchase of a web-published post delivers the with-link
    /// sentence: the minted code AND the post's public page — the URL
    /// `public_post_url` derives, the advertisement's `r` tag and the web
    /// redeem door a buyer with no account uses.
    #[tokio::test]
    async fn a_web_published_posts_delivery_carries_its_public_url() {
        let state = state().await;
        let author = [48u8; 32];
        let (_payee, stranger, receipt, post) =
            stranger_buys(&state, author, "post-unlock-dm-web").await;
        // The author's site, after the account exists.
        state.db.set_handle(&author, "alice").await.unwrap();
        state.db.set_subdomain_enabled(&author).await.unwrap();
        state
            .identity_domain
            .store(Some(Arc::new("example.org".to_string())));
        let post_bytes: [u8; 32] = hex::decode(&post).unwrap().try_into().unwrap();
        state
            .db
            .publish_web_post(&author, &post_bytes, "the-teaser")
            .await
            .unwrap();
        let url = fauna_nest::web_content::public_url::public_post_url(
            &state.db,
            &author,
            &post_bytes,
            &state.web_serving_domain(),
        )
        .await
        .unwrap()
        .expect("a web-published post with a public origin has a page");
        assert_eq!(url, "https://alice.example.org/post/the-teaser.html");

        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-dm-web")
        );

        let codes = minted_codes(&state, author).await;
        assert_eq!(codes.len(), 1, "one purchase, one claim code");
        let outbox = leg_outbox(&state).await;
        match claim_delivery_body(&codes[0], Some(&url)) {
            Some(body) => {
                assert_eq!(
                    outbox,
                    vec![(author, stranger.public_key_hex(), body)],
                    "one DM to the buyer's key"
                );
                assert!(outbox[0].2.contains(&codes[0]), "it carries the code");
                assert!(outbox[0].2.contains(&url), "and the post's public page");
                assert!(!outbox[0].2.contains('{'), "no placeholder left unfilled");
            }
            None => assert!(outbox.is_empty()),
        }
    }

    /// A receipt whose request the named sender did not sign has nobody to
    /// deliver to: the purchase still stands, the audit is the recourse.
    #[tokio::test]
    async fn a_purchase_with_no_certified_sender_delivers_nothing() {
        let state = state().await;
        let author = [44u8; 32];
        let payee = sending_payee(&state, author).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;
        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-dm-2",
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;
        // Signed by one key, relabelled with another: the request names a
        // sender who never signed it, so check (iv) certifies nobody.
        let mut forged =
            common::zap::zap_request(&Keypair::generate(), &payee.public_key_hex(), 1_700_002_100);
        forged.pubkey = Keypair::generate().public_key_hex();
        let receipt = common::zap::zap_receipt_embedding(
            &signer,
            &payee.public_key_hex(),
            &note_id,
            serde_json::to_string(&forged).unwrap(),
            Some(LNBC_21_SATS),
            1_700_002_100,
        );

        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-dm-2"),
            "still a purchase: the waist mints a claim code for the unbound buyer"
        );
        assert_eq!(minted_codes(&state, author).await.len(), 1);
        assert!(leg_outbox(&state).await.is_empty(), "nobody to deliver to");
        assert!(sent_texts(&state, author).await.is_empty());
    }

    /// A buyer with an account on this box is granted by the waist directly;
    /// no claim code exists and no message is sent.
    #[tokio::test]
    async fn a_local_actor_buyer_gets_no_message() {
        let state = state().await;
        let author = [45u8; 32];
        let buyer = [46u8; 32];
        let payee = sending_payee(&state, author).await;
        let buyer_kp = link_payee(&state, buyer).await;
        let signer = Keypair::generate();
        designate(&state, author, &signer).await;
        let post_id = sell_post_for_real(
            &state,
            author,
            "post-unlock-dm-3",
            Some(TierAskingPrice::msats(21_000)),
        )
        .await;
        let note_id = advertise_for_real(&state, author, &post_id).await;
        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp,
            Some(LNBC_21_SATS),
            1_700_002_200,
        );

        assert_eq!(
            ingest(&state, &receipt).await.as_deref(),
            Some("post-unlock-dm-3")
        );
        assert!(is_entitled(&state, author, buyer, "post-unlock-dm-3").await);
        assert!(
            minted_codes(&state, author).await.is_empty(),
            "no claim code"
        );
        assert!(leg_outbox(&state).await.is_empty(), "no message");
    }
}
