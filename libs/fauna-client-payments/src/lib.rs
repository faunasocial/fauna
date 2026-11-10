//! Typed-call wrapper for the Layer-3 payment-provider WS-RPC kinds — the
//! `fauna.payments.*` surface (Pillar 3 of
//! `docs/goal/behavior/monetization.md`): the author-side provider
//! configuration (`providers.{set,list,remove}`, driven from the profile
//! Tiers-tab §4 provider section) and the buyer-side claim-code redemption
//! (`claims.redeem`, driven from the `subscription-settings` page).
//!
//! Pattern matches `fauna-client-subscriptions`: a thin [`PaymentsClient`]
//! generic over the WS-RPC transport (`R: RpcRequester`), one async method
//! per kind, no state machine and no client-side validation — the nest
//! rejects unknown kinds / dangling tiers / empty secrets with typed errors
//! (`fauna.payments.*`, see `bins/fauna-nest/src/payment_handlers.rs`).
//! Native call sites pass `Arc<NestClient>`; the wasm SPA passes its
//! `WsRpcClient`. The kind-composition logic is written once here and shared
//! across native + wasm (priority #2).
//!
//! The provider form's kind select enumerates [`known_kinds`] — re-exported
//! from `fauna-payments`, the adapter registry the nest validates against,
//! so client and nest can never disagree on the list at equal versions (a
//! newer client offering a kind an older nest lacks gets the nest's typed
//! `fauna.payments.unknown_provider`).

use fauna_core::data::Timestamp;
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::types::ClaimRedeemIntent;
use fauna_protocol::payments::{
    ClaimItem, ClaimMintReply, ClaimMintRequest, ClaimRedeemReply, ClaimRedeemRequest,
    ClaimVoidReply, ClaimVoidRequest, ClaimsListReply, ClaimsListRequest, ProviderItem,
    ProviderRemoveReply, ProviderRemoveRequest, ProviderSetReply, ProviderSetRequest,
    ProvidersListReply, ProvidersListRequest,
};
use fauna_protocol::{CrossNestError, RpcRequester, normalize_home_nest_url};

pub use fauna_payments::known_kinds;
/// The visitor return door's creator-facing URL (the post-checkout redirect
/// pasted into the provider's settings), re-exported for the same reason.
pub use fauna_payments::{RETURN_PATH_PREFIX, return_url};
/// Re-exported so a client builds the creator-facing webhook URL from the
/// same constant the nest registers its ingress route from, rather than
/// hand-assembling the path (priority #2 — same reasoning as
/// [`known_kinds`]).
pub use fauna_payments::{WEBHOOK_PATH_PREFIX, webhook_url};
pub use fauna_protocol::payments;
/// The tip wire types, re-exported so a display leg needs one dependency for
/// the call and its reply shape.
pub use fauna_protocol::tips;

use fauna_protocol::tips::{TipsListReply, TipsListRequest};

/// The redeemer's own signed [`ClaimRedeemIntent`] (`federated-follow.md`
/// § Decision 2 → *The redeem twin*), in the embed-as-bytes wire shape
/// `ClaimRedeemRequest.intent` carries. Signed by `redeemer`'s identity key
/// (which holds every capability, `Follow` included). `home_nest_id` is the
/// redeemer's own nest — the only nest that may relay it — and `created_at`
/// orders the redeemer's intents at the author's nest, so a caller passes now.
pub fn mint_claim_redeem_intent(
    redeemer: &ActorKeypair,
    author: ActorId,
    code: &str,
    home_nest_id: [u8; 32],
    created_at: Timestamp,
) -> Result<EmbedAsBytes, fauna_core::error::Error> {
    let intent = ClaimRedeemIntent {
        redeemer: redeemer.actor_id(),
        author,
        code: code.to_string(),
        home_nest_id,
        created_at,
    };
    let (bytes, envelope) = fauna_core::encoding::sign_envelope(redeemer, &intent)?;
    Ok(EmbedAsBytes::from_signed(bytes, envelope))
}

/// Typed `fauna.payments.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`). Every kind but `claims.mint` is registered with
/// `forbid_replay: false` server-side (see `register_payment_handlers`), so
/// the auto-retry path may safely re-issue them — set is an upsert, remove
/// and void are idempotent, and redeem keys off server-enforced claim state
/// (a retried redeem by the same actor returns the already-bound
/// entitlement). A mint allocates a fresh code per call, so it is never
/// replayed.
pub struct PaymentsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> PaymentsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    // ── providers ───────────────────────────────────────────────────────

    /// `fauna.payments.providers.set` — upsert the calling author's config
    /// for one provider kind (one config per kind per author, first cut).
    /// The nest holds ONLY the webhook-verification secret; it is never
    /// echoed back (see [`Self::providers_list`]). Returns whether the
    /// config was saved.
    ///
    /// `product_ref` is the product the tier is sold as, in the provider's
    /// own reference (Stripe: the Payment Link id, `plink_…`) — the wire
    /// contract of [`ProviderSetRequest::product_ref`]: `None` keeps the
    /// saved value, `Some("")` clears it so every verified payment maps
    /// again (monetization.md § Pillar 3 → *Configuration*). A form that
    /// shows the field sends what it shows — `Some(value)`, empty included —
    /// so the saved mapping always reads back as the creator last saw it.
    pub async fn providers_set(
        &self,
        kind: impl Into<String>,
        webhook_secret: impl Into<String>,
        tier: impl Into<String>,
        product_ref: Option<String>,
    ) -> Result<bool, R::Error> {
        let reply: ProviderSetReply = self
            .nest
            .request(
                "fauna.payments.providers.set",
                ProviderSetRequest {
                    kind: kind.into(),
                    webhook_secret: webhook_secret.into(),
                    tier: tier.into(),
                    product_ref,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.saved)
    }

    /// `fauna.payments.providers.list` — the calling author's own configured
    /// providers, ascending by kind. The rows deliberately omit the webhook
    /// secret (least exposure — changing it means re-entering it in the
    /// form). Replay-safe pure read.
    pub async fn providers_list(&self) -> Result<Vec<ProviderItem>, R::Error> {
        let reply: ProvidersListReply = self
            .nest
            .request(
                "fauna.payments.providers.list",
                ProvidersListRequest::default(),
            )
            .await?;
        Ok(reply.providers)
    }

    /// `fauna.payments.providers.remove` — delete the calling author's config
    /// for one provider kind. Idempotent; returns whether a row was removed.
    pub async fn providers_remove(&self, kind: impl Into<String>) -> Result<bool, R::Error> {
        let reply: ProviderRemoveReply = self
            .nest
            .request(
                "fauna.payments.providers.remove",
                ProviderRemoveRequest {
                    kind: kind.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.removed)
    }

    // ── claims ──────────────────────────────────────────────────────────

    /// `fauna.payments.claims.redeem` — bind a post-payment claim code to
    /// the calling (bearer) actor; the entitlement lands through Pillar 1's
    /// grant queue (`ClaimRedeemReply::queued` mirrors a queued subscribe).
    /// Typed errors: `fauna.payments.claim_{not_found,already_redeemed,voided}`.
    pub async fn claims_redeem(
        &self,
        code: impl Into<String>,
    ) -> Result<ClaimRedeemReply, R::Error> {
        self.nest
            .request(
                "fauna.payments.claims.redeem",
                ClaimRedeemRequest {
                    code: code.into(),
                    ..Default::default()
                },
            )
            .await
    }

    /// The redeem gesture every app calls — [`Self::claims_redeem`] for a code
    /// of this nest, and the **cross-nest redeem** for a code minted by an
    /// author homed on another (`monetization.md` § Pillar 3 → *Which nest an
    /// in-app redeem binds on*; the follow's
    /// `SubscriptionsClient::subscribe_across_nests` shape). `author_home_url`
    /// is the author's home as the app learned it (the profile's `actor_home`
    /// slot); `None` or blank is this nest. Naming a home makes the redeemer
    /// mint their own signed [`ClaimRedeemIntent`] — the buyer's act the
    /// code's home must see, never their nest's word for it — naming the nest
    /// this client is connected to as their home, and the nest relays it
    /// verbatim. An author hosted here is a local redeem whatever home is
    /// named (the nest's first arm). Typed errors: those of
    /// [`Self::claims_redeem`], plus `fauna.payments.claim_admits_elsewhere`
    /// (a membership code redeemed from another nest).
    pub async fn claims_redeem_across_nests(
        &self,
        code: impl Into<String>,
        redeemer: &ActorKeypair,
        author_id: ActorId,
        author_home_url: Option<String>,
    ) -> Result<ClaimRedeemReply, CrossNestError<R::Error>> {
        let code = code.into();
        let Some(home) = normalize_home_nest_url(author_home_url) else {
            return self.claims_redeem(code).await.map_err(CrossNestError::Nest);
        };
        let home_nest_id = fauna_protocol::read_nest_id(&self.nest).await?;
        let intent = mint_claim_redeem_intent(
            redeemer,
            author_id,
            code.trim(),
            home_nest_id,
            Timestamp::now(),
        )
        .map_err(|e| CrossNestError::Intent(e.to_string()))?;
        self.nest
            .request(
                "fauna.payments.claims.redeem",
                ClaimRedeemRequest {
                    code,
                    author_home_url: Some(home),
                    intent: Some(intent),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(CrossNestError::Nest)
    }

    /// `fauna.payments.claims.mint` — the author mints a claim code manually,
    /// for a no-API provider (bank transfer, cash, …) already paid
    /// out-of-band. Always `provider = "manual"` server-side; the nest
    /// rejects a `tier` that isn't one of the author's own tiers.
    pub async fn claims_mint(
        &self,
        tier: impl Into<String>,
        valid_until: Option<Timestamp>,
    ) -> Result<ClaimMintReply, R::Error> {
        self.nest
            .request(
                "fauna.payments.claims.mint",
                ClaimMintRequest {
                    tier: tier.into(),
                    valid_until,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.payments.claims.list` — the calling author's own claim codes,
    /// newest first — the audit surface for BOTH manually-minted
    /// (`claims_mint`) and webhook-minted codes, whose only other delivery
    /// channel is the webhook HTTP response body. Replay-safe pure read.
    pub async fn claims_list(&self) -> Result<Vec<ClaimItem>, R::Error> {
        let reply: ClaimsListReply = self
            .nest
            .request("fauna.payments.claims.list", ClaimsListRequest::default())
            .await?;
        Ok(reply.claims)
    }

    /// `fauna.payments.claims.void` — the author withdraws one of their own
    /// claim codes (monetization.md § Pillar 3 → *Buyer↔actor binding*): an
    /// unredeemed code stops redeeming (in the app and on the web); a
    /// redeemed one also ends the redeemer's entitlement to its tier, as a
    /// refund does. Idempotent — a repeat answers the first void's stamp.
    /// Typed error: `fauna.payments.claim_not_found` for an unknown code or
    /// one the caller did not mint. Returns when the code was voided.
    pub async fn claims_void(&self, code: impl Into<String>) -> Result<Timestamp, R::Error> {
        let reply: ClaimVoidReply = self
            .nest
            .request(
                "fauna.payments.claims.void",
                ClaimVoidRequest {
                    code: code.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.voided_at)
    }
}

// ── tips ────────────────────────────────────────────────────────────────

/// Typed `fauna.tips.*` call surface — the post-addressed tip attribution
/// read (`monetization.md` § Tips).
///
/// **A separate client from [`PaymentsClient`], deliberately.** A tip names
/// *(payee, post)* and grants nothing, so it never touches the entitlement
/// waist the `fauna.payments.*` kinds feed; giving it its own type keeps the
/// two consequence classes from blurring at the one seam every app calls.
/// Same crate, because both are the monetization mechanisms' client face.
///
/// **Mechanism-blind by construction.** Nothing in this type mentions NIP-57.
/// A tip arriving over a future mechanism surfaces through the same call with
/// no client change, which is what lets the 7 app display legs be written
/// once against this rather than against a zap-shaped read.
pub struct TipsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> TipsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.tips.list` — the tips on one post, newest-first, plus the
    /// **unbounded** totals.
    ///
    /// `post_id` is the lowercase-hex 32-byte Fauna post id. `limit` is the
    /// attribution-window size; `None` takes the nest's default and an
    /// oversized value is clamped rather than refused, so this call cannot
    /// fail on a limit.
    ///
    /// The reply's `total_msats` / `tip_count` cover **every** tip on the
    /// post, not just the returned window — a post card renders those and
    /// never needs the items at all. Replay-safe pure read; callers fold any error
    /// (a transport error, say) to "no tip surface".
    pub async fn tips_list(
        &self,
        post_id: impl Into<String>,
        limit: Option<u32>,
    ) -> Result<TipsListReply, R::Error> {
        self.nest
            .request(
                "fauna.tips.list",
                TipsListRequest {
                    post_id: post_id.into(),
                    limit,
                    extra: Default::default(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use fauna_core::data::Timestamp;
    use fauna_core::identity::ActorId;

    // ── Wire-contract tests ─────────────────────────────────────────────
    //
    // Each `PaymentsClient` method must send its exact `fauna.payments.*`
    // kind and a payload that round-trips back to the typed request — a kind
    // rename or field drift would otherwise break silently the moment a UI
    // lands. Mirrors `fauna-client-subscriptions`' `RecordingRequester`
    // (transport-free, so it runs on every target including wasm); real
    // end-to-end round-trip conformance lives in
    // `bins/fauna-nest/tests/conformance_payments.rs` and the tier_3
    // `tests/e2e-unified/tests/api/test_payments_webhook.py`.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes — one
        // arm per kind, each the minimal valid shape.
        match kind {
            "fauna.payments.providers.set" => fauna_protocol::encode_canonical(&ProviderSetReply {
                saved: true,
                extra: Default::default(),
            }),
            "fauna.payments.providers.list" => {
                fauna_protocol::encode_canonical(&ProvidersListReply {
                    providers: vec![ProviderItem {
                        kind: "fake".into(),
                        tier: "gold".into(),
                        created_at: Timestamp(1_700_000_000_000_000),
                        last_verified_at: None,
                        last_rejected_at: None,
                        product_ref: None,
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                })
            }
            "fauna.payments.providers.remove" => {
                fauna_protocol::encode_canonical(&ProviderRemoveReply {
                    removed: true,
                    extra: Default::default(),
                })
            }
            "fauna.nest.info" => {
                fauna_protocol::encode_canonical(&fauna_protocol::discovery::NestInfoReply {
                    nest_id: fauna_core::hex32::encode(&OWN_NEST_ID),
                    ..Default::default()
                })
            }
            "fauna.payments.claims.redeem" => fauna_protocol::encode_canonical(&ClaimRedeemReply {
                author: ActorId([0xAB; 32]),
                tier: "gold".into(),
                valid_until: Some(Timestamp(1_700_000_000_000_000)),
                queued: true,
                extra: Default::default(),
            }),
            "fauna.payments.claims.mint" => fauna_protocol::encode_canonical(&ClaimMintReply {
                code: "ABCD-1234".into(),
                tier: "gold".into(),
                valid_until: None,
                extra: Default::default(),
            }),
            "fauna.payments.claims.list" => fauna_protocol::encode_canonical(&ClaimsListReply {
                claims: vec![ClaimItem {
                    code: "ABCD-1234".into(),
                    tier: "gold".into(),
                    provider: "manual".into(),
                    valid_until: None,
                    created_at: Timestamp(1_700_000_000_000_000),
                    redeemed_by: None,
                    redeemed_at: None,
                    voided_at: None,
                    extra: Default::default(),
                }],
                extra: Default::default(),
            }),
            "fauna.payments.claims.void" => fauna_protocol::encode_canonical(&ClaimVoidReply {
                voided_at: Timestamp(1_700_000_001_000_000),
                extra: Default::default(),
            }),
            "fauna.tips.list" => fauna_protocol::encode_canonical(&tips::TipsListReply {
                total_msats: 2_100_000,
                tip_count: 1,
                tips: vec![tips::TipItem {
                    sender: Some(ActorId([0xCD; 32])),
                    sender_ref: Some("e".repeat(64)),
                    amount_msats: Some(2_100_000),
                    mechanism: "nostr_zap".into(),
                    received_at: 1_700_000_000,
                    extra: Default::default(),
                }],
                has_more: false,
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn providers_set_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PaymentsClient::new(rec.clone());
        let saved =
            block_on(client.providers_set("fake", "whsec_test", "gold", Some("prod_1".into())))
                .expect("infallible mock");
        assert!(saved);
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.payments.providers.set");
        let req: ProviderSetRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.kind, "fake");
        assert_eq!(req.webhook_secret, "whsec_test");
        assert_eq!(req.tier, "gold");
        assert_eq!(req.product_ref.as_deref(), Some("prod_1"));
    }

    #[test]
    fn providers_list_composes_kind_and_returns_rows() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PaymentsClient::new(rec.clone());
        let rows = block_on(client.providers_list()).expect("infallible mock");
        assert_eq!(rec.recorded().0, "fauna.payments.providers.list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "fake");
        assert_eq!(rows[0].tier, "gold");
    }

    #[test]
    fn providers_remove_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PaymentsClient::new(rec.clone());
        let removed = block_on(client.providers_remove("fake")).expect("infallible mock");
        assert!(removed);
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.payments.providers.remove");
        let req: ProviderRemoveRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.kind, "fake");
    }

    #[test]
    fn claims_redeem_composes_kind_and_returns_reply() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PaymentsClient::new(rec.clone());
        let reply = block_on(client.claims_redeem("ABCD-1234")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.payments.claims.redeem");
        let req: ClaimRedeemRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.code, "ABCD-1234");
        assert_eq!(reply.tier, "gold");
        assert!(reply.queued);
        assert_eq!(reply.author, ActorId([0xAB; 32]));
    }

    #[test]
    fn claims_mint_composes_kind_and_returns_reply() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PaymentsClient::new(rec.clone());
        let reply = block_on(client.claims_mint("gold", None)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.payments.claims.mint");
        let req: ClaimMintRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.tier, "gold");
        assert_eq!(req.valid_until, None);
        assert_eq!(reply.code, "ABCD-1234");
        assert_eq!(reply.tier, "gold");
    }

    #[test]
    fn claims_list_composes_kind_and_returns_rows() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PaymentsClient::new(rec.clone());
        let rows = block_on(client.claims_list()).expect("infallible mock");
        assert_eq!(rec.recorded().0, "fauna.payments.claims.list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].code, "ABCD-1234");
        assert_eq!(rows[0].tier, "gold");
        assert_eq!(rows[0].provider, "manual");
        assert!(rows[0].redeemed_by.is_none());
    }

    #[test]
    fn claims_void_composes_kind_and_returns_the_stamp() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PaymentsClient::new(rec.clone());
        let voided_at = block_on(client.claims_void("ABCD-1234")).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.payments.claims.void");
        let req: ClaimVoidRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.code, "ABCD-1234");
        assert_eq!(voided_at, Timestamp(1_700_000_001_000_000));
    }

    #[test]
    fn known_kinds_reexport_is_the_registry() {
        // The form's kind select and the nest's providers.set gate must
        // enumerate the same registry.
        assert!(known_kinds().contains(&"fake"));
        assert!(known_kinds().contains(&"stripe"));
    }

    // ── tips ────────────────────────────────────────────────────────────

    #[test]
    fn tips_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = TipsClient::new(rec.clone());
        let post_id = "ab".repeat(32);
        let reply = block_on(client.tips_list(post_id.clone(), Some(5))).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.tips.list");
        let req: TipsListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.post_id, post_id, "asked by FAUNA post id");
        assert_eq!(req.limit, Some(5));
        assert_eq!(reply.total_msats, 2_100_000);
        assert_eq!(reply.tip_count, 1);
    }

    /// The reply's totals are the surface a post card renders; the items are
    /// the attribution window. Pinned together so a future change cannot
    /// quietly make the totals describe only the window.
    #[test]
    fn a_tip_reply_carries_attribution_beside_its_totals() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = TipsClient::new(rec.clone());
        let reply = block_on(client.tips_list("cd".repeat(32), None)).expect("infallible mock");
        assert_eq!(reply.tips.len(), 1);
        assert_eq!(reply.tips[0].mechanism, "nostr_zap");
        assert_eq!(reply.tips[0].sender, Some(ActorId([0xCD; 32])));
        assert!(!reply.has_more);
    }

    /// Nothing on this client's surface names a mechanism: the display legs
    /// are written against tips, not against zaps. A `nostr`/`zap` token
    /// appearing in a kind or field name here would mean the seam had leaked.
    #[test]
    fn the_tip_call_surface_names_no_mechanism() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = TipsClient::new(rec.clone());
        let _ = block_on(client.tips_list("ef".repeat(32), None)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert!(
            !kind.contains("nostr") && !kind.contains("zap"),
            "kind {kind}"
        );
        let req: TipsListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.extra.is_empty(), "no mechanism-shaped extra field");
    }

    // ── The cross-nest redeem (`monetization.md` § Pillar 3 → *Which nest an
    //    in-app redeem binds on*) ───────────────────────────────────────────

    /// The nest id the reply table's `fauna.nest.info` answers with — the id a
    /// minted redeem intent must name as the redeemer's home.
    const OWN_NEST_ID: [u8; 32] = [0x42; 32];

    /// Decode and verify a carried intent exactly as both nests do: the
    /// authoring envelope under `Capability::Follow`.
    fn verified_intent(wire: &EmbedAsBytes) -> ClaimRedeemIntent {
        let (bytes, env) = wire.clone().into_signed().expect("embed-as-bytes");
        let intent: ClaimRedeemIntent =
            fauna_core::encoding::decode_signed_bytes(&bytes).expect("decodes");
        fauna_core::encoding::verify_authoring_envelope(
            &intent,
            &bytes,
            &env,
            wire.signer_auth.as_deref(),
            &fauna_core::data::Capability::Follow,
            intent.created_at,
        )
        .expect("the intent verifies under Capability::Follow");
        intent
    }

    /// A redeem naming no home is today's local redeem: no intent, no home,
    /// and no `fauna.nest.info` read.
    #[test]
    fn a_redeem_naming_no_home_is_the_local_redeem() {
        let me = ActorKeypair::generate();
        for home in [None, Some("  ".to_string())] {
            let rec = std::sync::Arc::new(RecordingRequester::new(reply));
            let client = PaymentsClient::new(rec.clone());
            let reply = block_on(client.claims_redeem_across_nests(
                "ABCD-1234",
                &me,
                ActorId([0xAB; 32]),
                home,
            ))
            .expect("infallible mock");
            assert_eq!(reply.tier, "gold");
            assert_eq!(rec.kinds(), vec!["fauna.payments.claims.redeem"]);
            let req: ClaimRedeemRequest =
                fauna_protocol::decode_strict(&rec.recorded().1).expect("decodes");
            assert_eq!(req.code, "ABCD-1234");
            assert_eq!(req.author_home_url, None);
            assert!(req.intent.is_none(), "a local redeem carries no intent");
        }
    }

    /// A redeem naming the author's home carries it and the redeemer's own
    /// signed intent, naming the nest the client is connected to.
    #[test]
    fn a_redeem_naming_a_home_carries_the_redeemers_signed_intent_naming_this_nest() {
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PaymentsClient::new(rec.clone());
        block_on(client.claims_redeem_across_nests(
            " ABCD-1234 ",
            &me,
            ActorId([0xAB; 32]),
            Some(" https://author.test/ ".into()),
        ))
        .expect("infallible mock");
        assert_eq!(
            rec.kinds(),
            vec!["fauna.nest.info", "fauna.payments.claims.redeem"]
        );
        let req: ClaimRedeemRequest =
            fauna_protocol::decode_strict(&rec.recorded().1).expect("decodes");
        assert_eq!(req.author_home_url.as_deref(), Some("https://author.test"));
        let intent = verified_intent(req.intent.as_ref().expect("the intent rides"));
        assert_eq!(intent.redeemer, me.actor_id());
        assert_eq!(intent.author, ActorId([0xAB; 32]));
        assert_eq!(
            intent.code, "ABCD-1234",
            "the intent names the trimmed code"
        );
        assert_eq!(intent.home_nest_id, OWN_NEST_ID);
    }

    /// A nest whose `nest.info` names no well-formed id gets no intent, and no
    /// redeem is sent.
    #[test]
    fn a_malformed_own_nest_id_sends_no_redeem() {
        fn bad_info(kind: &'static str) -> Vec<u8> {
            match kind {
                "fauna.nest.info" => {
                    fauna_protocol::encode_canonical(&fauna_protocol::discovery::NestInfoReply {
                        nest_id: "not-hex".into(),
                        ..Default::default()
                    })
                    .expect("encode")
                    .to_vec()
                }
                other => reply(other),
            }
        }
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::new(bad_info));
        let client = PaymentsClient::new(rec.clone());
        let err = block_on(client.claims_redeem_across_nests(
            "ABCD-1234",
            &me,
            ActorId([0xAB; 32]),
            Some("https://author.test".into()),
        ))
        .expect_err("no intent without this nest's id");
        assert!(matches!(err, CrossNestError::Intent(_)), "{err}");
        assert_eq!(rec.kinds(), vec!["fauna.nest.info"]);
    }
}
