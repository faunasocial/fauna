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
//! the sole hand-made link is the outbound `nostr_event_map` row, for the
//! reason `publish_note_as` gives.
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

    use common::zap::{designate_zap_signer as designate, link_payee, zap_receipt};

    /// 21 sats = 21_000 msats. The `n` (nano) HRP multiplier is `amount x 100`
    /// msats, so `210n` is 21_000.
    const LNBC_21_SATS: &str = "lnbc210n1pjfake";
    /// 20 sats = 20_000 msats — one sat under a 21-sat asking price.
    const LNBC_20_SATS: &str = "lnbc200n1pjfake";

    /// Give the post a Nostr presence a receipt's `e` tag can name.
    ///
    /// ⚠ **This is the one deliberately hand-made link in the chain, and it
    /// stands in for an ingress that does not exist yet.** In production the
    /// only writer of an outbound `nostr_event_map` row is
    /// `nostr::store::materialize_account`, and it skips gated posts
    /// (`public_post_from_payload` → `None`) — so a *sold* post, which is gated
    /// by construction, never reaches Nostr at all today and no zap can name
    /// it. That reachability fact is pinned on the materializer itself
    /// (`nostr::store`'s `a_sold_post_is_never_materialized_so_no_zap_can_name_it`)
    /// and declared in `monetization.md` § Implementation status today; here we
    /// simulate the future advertise ingress so the *split* below is testable.
    /// Everything else in these tests — the post, its gate, the tier, the
    /// prices, the ingest path — is production-written.
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
        zap_ingest::record_zap_as(&conn, &zap, purchased.as_deref());
        drop(conn);
        Ok(purchased)
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
        let payee = link_payee(&state, author).await;
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
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp.public_key_hex(),
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
    }

    #[tokio::test]
    async fn one_msat_under_the_price_stays_a_tip_and_grants_nothing() {
        let state = state().await;
        let author = [3u8; 32];
        let buyer = [4u8; 32];
        let payee = link_payee(&state, author).await;
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
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp.public_key_hex(),
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
            &buyer_kp.public_key_hex(),
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
            &buyer_kp.public_key_hex(),
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
        let payee = link_payee(&state, author).await;
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
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp.public_key_hex(),
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
        let payee = link_payee(&state, author).await;
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
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &stranger.public_key_hex(),
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
        let payee = link_payee(&state, author).await;
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
        let note_id = publish_note_as(&state, &payee, &post_id).await;

        let receipt = zap_receipt(
            &signer,
            &payee.public_key_hex(),
            &payee.public_key_hex(),
            &note_id,
            &buyer_kp.public_key_hex(),
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
            &buyer_kp.public_key_hex(),
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
            &buyer_kp.public_key_hex(),
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
        let payee = link_payee(&state, author).await;
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
        let note_id = publish_note_as(&state, &payee, &post_id).await;

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
            &buyer_kp.public_key_hex(),
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
        let payee = link_payee(&state, author).await;
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
        let note_id = publish_note_as(&state, &payee, &post_id).await;

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
            &buyer_kp.public_key_hex(),
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
}
