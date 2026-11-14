//! Typed-call wrapper for the Layer-3 subscription-management WS-RPC kinds —
//! the `fauna.subscriptions.*` surface an author drives from the
//! subscriptions / tiers UI (tier CRUD, the subscribe/unsubscribe flow,
//! the author-side request queue, and the KeyBlob fetch).
//!
//! Namespace split from `fauna-client-bridges` / `fauna-client-email` per the
//! one-crate-per-feature convention those crates established: a session
//! looking for `SubscriptionsClient::tiers_create` or `::subscribe` lands
//! here, not in the bridges/email crates. The server half is
//! `bins/fauna-nest/src/subscription_handlers.rs`
//! (`register_subscription_handlers` registers all 17 kinds on the WS-RPC
//! router); the encrypted-mode author-mint primitive that feeds the
//! `encrypted_upload` envelope is `fauna_core::subscription::crypto::
//! mint_key_blob` (bound to clients via `fauna-ffi`'s `mint_key_blob` /
//! `fauna-wasm`'s twin).
//!
//! Pattern matches `fauna-client-bridges`: a thin `SubscriptionsClient`
//! generic over the WS-RPC transport (`R: RpcRequester`), one async method
//! per kind, no state machine. Native call sites pass `Arc<NestClient>`; the
//! wasm SPA passes its `WsRpcClient`. The kind-composition logic is written
//! once here and shared across native + wasm (priority #2). Errors propagate
//! as the transport's `R::Error` (native `NestClientError`, wasm rpc-wasm
//! error).
//!
//! Spec tracked internally.

use fauna_core::data::Timestamp;
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::crypto::subscriber_mlkem_encaps_key;
use fauna_core::subscription::types::{FollowIntent, FollowIntentKind};
use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::capability;
use fauna_protocol::subscriptions::{
    ApproveRequestReply, ApproveRequestRequest, DelegateUploadReply, DelegateUploadRequest,
    EncapsKeyPublishReply, EncapsKeyPublishRequest, EncryptedKeyBlobUpload, KeyBlobGetReply,
    KeyBlobGetRequest, MineListReply, MineListRequest, MineSubscription, OffersListRemoteRequest,
    OffersListRequest, PendingRequest, PostUnlockGetReply, PostUnlockGetRequest, PostUnlockOffer,
    RejectRequestReply, RejectRequestRequest, RemoveSubscriberReply, RemoveSubscriberRequest,
    RequestsListReply, RequestsListRequest, RotateKeyBlobReply, RotateKeyBlobRequest,
    StatusGetReply, StatusGetRequest, SubscribeReply, SubscribeRequest, SubscriberEntry,
    SubscribersListReply, SubscribersListRequest, TierAskingPrice, TierCreateReply,
    TierCreateRequest, TierDeleteReply, TierDeleteRequest, TierItem, TierUpdateReply,
    TierUpdateRequest, TiersListReply, TiersListRequest, UnsubscribeReply, UnsubscribeRequest,
};

pub use fauna_protocol::subscriptions;

/// The `fauna.subscriptions.encaps_key.publish` request for `subscriber`: its
/// identity-seed-derived ML-KEM-768 encapsulation key (`post-quantum.md`
/// § surface B). One builder, so every caller — the session-start publish
/// ([`SubscriptionsClient::publish_encaps_key`]) and the e2e harness's C-ABI
/// seam — sends the same bytes.
#[must_use]
pub fn encaps_key_publish_request(subscriber: &ActorKeypair) -> EncapsKeyPublishRequest {
    EncapsKeyPublishRequest {
        mlkem_encaps_key: fauna_protocol::ByteBuf::from(
            subscriber_mlkem_encaps_key(subscriber).to_vec(),
        ),
        extra: Default::default(),
    }
}

/// The subscriber's own signed [`FollowIntent`] (`federated-follow.md`
/// § Decision 2, *The intent*), in the embed-as-bytes wire shape
/// `SubscribeRequest.intent` / `UnsubscribeRequest.intent` carry. Signed by
/// `subscriber`'s identity key (which holds every capability, `Follow`
/// included). `home_nest_id` is the subscriber's own nest — the only nest that
/// may relay it — and `created_at` orders the subscriber's intents at the
/// author's nest, so a caller passes now.
pub fn mint_follow_intent(
    subscriber: &ActorKeypair,
    author: ActorId,
    tier: &str,
    kind: FollowIntentKind,
    home_nest_id: [u8; 32],
    created_at: Timestamp,
) -> Result<EmbedAsBytes, fauna_core::error::Error> {
    let intent = FollowIntent {
        subscriber: subscriber.actor_id(),
        author,
        tier: tier.to_string(),
        kind,
        home_nest_id,
        created_at,
    };
    let (bytes, envelope) = fauna_core::encoding::sign_envelope(subscriber, &intent)?;
    Ok(EmbedAsBytes::from_signed(bytes, envelope))
}

use fauna_protocol::normalize_home_nest_url;

/// Why a follow that may cross nests failed: the nest's own error, or an
/// intent this client could not mint (its nest names no well-formed id) —
/// the one cross-nest error the claim redeem shares.
pub type FollowError<E> = fauna_protocol::CrossNestError<E>;

/// What [`SubscriptionsClient::subscribe_and_checkout`] hands the shell: the
/// subscribe's own reply (the optimistic pending transient reads it) and the
/// checkout URL to open.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckoutOpen {
    pub reply: SubscribeReply,
    pub url: String,
}

pub mod custody;
pub mod orchestration;
pub mod period_keys;
pub mod port;
pub mod sale_price;

/// The *Change price…* save plan and the shared asking-price parse
/// (`monetization.md` § The asking price → *Re-pricing a sold post*).
pub use sale_price::{
    AskingPriceParseError, SalePriceEdit, SalePricePrefill, parse_asking_price_sats,
    plan_sale_price_edit, plan_sale_price_edit_stored, sale_price_prefill,
};

/// The period-key store seam every custody consumer holds
/// (`fauna.state.subscriptions`, implemented on the account runtime's handle).
pub use period_keys::{PeriodKeyStore, SharedPeriodKeyStore};

/// The per-post pay-to-unlock tier vocabulary, re-exported at the crate root so
/// a consumer (the "sell this post" orchestration in `fauna-feed`, and every
/// surface that must recognise a designated tier) names one path.
pub use orchestration::{StagedTier, UNLOCK_TIER_PREFIX, mint_unlock_tier_name};

/// The author reconcile pump's shared policy — one tick body
/// ([`orchestration::SubscriptionsAuthor::reconcile_once`]) and one cadence — so
/// each app schedules the loop in its own runtime without re-deriving *what* a
/// tick does or *how long* to wait (priority #2).
pub use orchestration::{
    AUTHOR_POLL_SECS_ENV, ConnectPassLatch, DEFAULT_AUTHOR_POLL_SECS, ReconcilePass,
    author_poll_interval, author_poll_secs,
};

/// The shared loop itself, for a caller that already runs a bare tokio
/// runtime with no FFI/wasm boundary (linux, tui) — see its own doc comment
/// for why this stays a distinct opt-in from the policy re-exports above.
pub use orchestration::run_author_reconcile_loop;

/// Typed `fauna.subscriptions.*` call surface, generic over the WS-RPC
/// transport (`R: RpcRequester`). Every kind is registered with
/// `forbid_replay: false` server-side (see `register_subscription_handlers`),
/// so the auto-retry path may safely re-issue any of these — the handlers are
/// idempotent (tier mutations are upserts; subscribe/approve/remove key off
/// server-enforced roster state, not request identity).
pub struct SubscriptionsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> SubscriptionsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    // ── tiers ───────────────────────────────────────────────────────────

    /// `fauna.subscriptions.tiers.create` — define a new subscription tier
    /// for the calling author. Idempotent upsert keyed on `name` (a repeat
    /// surfaces `fauna.subscriptions.tier_already_exists`). Returns whether a
    /// row was created.
    ///
    /// `encrypted_upload` carries the tier's **birth KeyBlob** (empty roster,
    /// minted under the fresh period key) so the tier has a live blob from
    /// creation — [`SubscriptionsAuthor::create_tier`] supplies it.
    ///
    /// `unlocks_post` designates this tier a **per-post pay-to-unlock** tier
    /// naming the hex `post_id` it sells (`monetization.md` § Per-post
    /// pay-to-unlock); `None` on every ordinary tier. It is create-time
    /// immutable, which is why there is no counterpart on
    /// [`Self::tiers_update`] — see that method.
    ///
    /// `asking_price` sets the **machine-comparable purchase threshold**
    /// (`monetization.md` § The asking price); `None` — the default for every
    /// tier — means no *inferring* mechanism can buy it, which is the
    /// permanently-correct out-of-the-box state, not a gap. Unlike
    /// `unlocks_post` it IS mutable, so [`Self::tiers_update`] takes it too.
    ///
    /// `hidden` withholds the tier from every offer surface and refuses
    /// `subscribe` (`monetization.md` § The unifying model → *A tier may be
    /// hidden*); `false` on every ordinary tier.
    #[allow(clippy::too_many_arguments)]
    pub async fn tiers_create(
        &self,
        name: impl Into<String>,
        rank: u32,
        description: Option<String>,
        price_hint: Option<String>,
        payment_url: Option<String>,
        auto_approve: bool,
        encrypted_upload: EncryptedKeyBlobUpload,
        unlocks_post: Option<String>,
        asking_price: Option<TierAskingPrice>,
        hidden: bool,
    ) -> Result<bool, R::Error> {
        let reply: TierCreateReply = self
            .nest
            .request(
                "fauna.subscriptions.tiers.create",
                TierCreateRequest {
                    name: name.into(),
                    rank,
                    description,
                    price_hint,
                    payment_url,
                    auto_approve,
                    encrypted_upload,
                    unlocks_post,
                    asking_price,
                    hidden,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.created)
    }

    /// `fauna.subscriptions.tiers.update` — overwrite the mutable fields of
    /// an existing tier (each `None` field is left unchanged). Returns
    /// `fauna.subscriptions.tier_not_found` when no tier matches.
    ///
    /// There is deliberately **no `unlocks_post` parameter**: the per-post
    /// pay-to-unlock designation is create-time immutable (`monetization.md`
    /// § Per-post pay-to-unlock — re-pointing a sold unlock is a rug-pull), so
    /// this method always sends the "keep current" `None` and the designation
    /// is unmovable through the shared API by construction. The wire field
    /// still exists on `TierUpdateRequest` so a non-conforming client that
    /// tries gets a loud `fauna.subscriptions.designation_immutable` rather
    /// than a silent drop.
    ///
    /// `asking_price` **is** a parameter here, and that asymmetry with
    /// `unlocks_post` is the ratified distinction, not an oversight
    /// (`monetization.md` § The asking price — *Editability*): re-pointing a
    /// sold unlock changes what buyers already bought, while re-pricing binds
    /// future events only. `None` keeps the current price, like every other
    /// field on this method — it does not clear it (see
    /// [`TierUpdateRequest::asking_price`] for why, and for the uniform
    /// clearable-fields track that covers all four).
    #[allow(clippy::too_many_arguments)]
    pub async fn tiers_update(
        &self,
        name: impl Into<String>,
        rank: Option<u32>,
        description: Option<String>,
        price_hint: Option<String>,
        payment_url: Option<String>,
        auto_approve: Option<bool>,
        asking_price: Option<TierAskingPrice>,
    ) -> Result<bool, R::Error> {
        let reply: TierUpdateReply = self
            .nest
            .request(
                "fauna.subscriptions.tiers.update",
                TierUpdateRequest {
                    name: name.into(),
                    rank,
                    description,
                    price_hint,
                    payment_url,
                    auto_approve,
                    // Create-time immutable — always the "keep current" merge.
                    unlocks_post: None,
                    asking_price,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.updated)
    }

    /// `fauna.subscriptions.tiers.delete` — remove a tier by name. Idempotent
    /// (already-deleted returns `deleted: false`).
    pub async fn tiers_delete(&self, name: impl Into<String>) -> Result<bool, R::Error> {
        let reply: TierDeleteReply = self
            .nest
            .request(
                "fauna.subscriptions.tiers.delete",
                TierDeleteRequest {
                    name: name.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.deleted)
    }

    // ── subscribe / unsubscribe (subscriber-side) ───────────────────────

    /// `fauna.subscriptions.subscribe` — the calling actor requests `tier`
    /// from `author_id`. Resolves to `Approved` (auto-approve tier, or the
    /// author's nest applied it inline) or `Queued` with the pending
    /// request id.
    ///
    /// This **thin** form publishes no post-quantum key (`mlkem_encaps_key:
    /// None`). A client that holds the subscriber's keypair should prefer
    /// [`Self::subscribe_publishing_ek`], which additionally publishes the
    /// subscriber's ML-KEM ek so the author can wrap hybrid `KeyBlob`s (surface
    /// B, S4b). The two are wire-compatible — `None` is simply the classical
    /// (no-ek) subscribe shape.
    pub async fn subscribe(
        &self,
        author_id: ActorId,
        tier: impl Into<String>,
    ) -> Result<SubscribeReply, R::Error> {
        self.subscribe_inner(author_id, tier.into(), None).await
    }

    /// `fauna.subscriptions.subscribe`, additionally **publishing** the caller's
    /// identity-seed-derived ML-KEM-768 encapsulation key (surface B, S4b)
    /// unconditionally, so the author can later wrap the period key to this
    /// subscriber's X-Wing hybrid key. Use this from any client that holds the
    /// subscriber's keypair; no capability token gates the publication
    /// (`post-quantum.md` § Capability negotiation, the 2026-09-24 ruling).
    pub async fn subscribe_publishing_ek(
        &self,
        author_id: ActorId,
        tier: impl Into<String>,
        subscriber: &ActorKeypair,
    ) -> Result<SubscribeReply, R::Error> {
        let mlkem_encaps_key = Some(fauna_protocol::ByteBuf::from(
            subscriber_mlkem_encaps_key(subscriber).to_vec(),
        ));
        self.subscribe_inner(author_id, tier.into(), mlkem_encaps_key)
            .await
    }

    async fn subscribe_inner(
        &self,
        author_id: ActorId,
        tier: String,
        mlkem_encaps_key: Option<fauna_protocol::ByteBuf>,
    ) -> Result<SubscribeReply, R::Error> {
        self.nest
            .request(
                "fauna.subscriptions.subscribe",
                SubscribeRequest {
                    home_nest_url: None,
                    intent: None,
                    author_id,
                    tier,
                    mlkem_encaps_key,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// The follow gesture every app calls — [`Self::subscribe_publishing_ek`]
    /// for an author on this nest, and the **federated follow** for an author
    /// homed on another (`federated-follow.md` § Decision 2). `home_nest_url` is
    /// the author's home as the app learned it (a profile reached by
    /// `handle@domain`, a relayed or delivered post's
    /// `PostSummary::home_nest_url`); `None` or blank is this nest. Naming a
    /// home makes the subscriber mint their own signed [`FollowIntent`] — the
    /// subscriber's act the author's nest must see, never their nest's word for
    /// it — naming the nest this client is connected to as their home, and the
    /// nest relays it verbatim. An author hosted here is a local follow
    /// whatever home is named (the nest's first arm), so a stale home costs
    /// one `fauna.nest.info` read and nothing else.
    pub async fn subscribe_across_nests(
        &self,
        author_id: ActorId,
        tier: impl Into<String>,
        subscriber: &ActorKeypair,
        home_nest_url: Option<String>,
    ) -> Result<SubscribeReply, FollowError<R::Error>> {
        let tier = tier.into();
        let mlkem_encaps_key = Some(fauna_protocol::ByteBuf::from(
            subscriber_mlkem_encaps_key(subscriber).to_vec(),
        ));
        let Some(home) = normalize_home_nest_url(home_nest_url) else {
            return self
                .subscribe_inner(author_id, tier, mlkem_encaps_key)
                .await
                .map_err(FollowError::Nest);
        };
        let intent = self
            .follow_intent(subscriber, author_id, &tier, FollowIntentKind::Subscribe)
            .await?;
        self.nest
            .request(
                "fauna.subscriptions.subscribe",
                SubscribeRequest {
                    home_nest_url: Some(home),
                    intent: Some(intent),
                    author_id,
                    tier,
                    mlkem_encaps_key,
                    extra: Default::default(),
                },
            )
            .await
            .map_err(FollowError::Nest)
    }

    /// The payment-link gesture every app calls (`subscription-offer-payment-link`,
    /// the feed's `gated-post-payment-link`) — `monetization.md` § Pillar 3 →
    /// *Buyer↔actor binding (Q4)* → *A checkout-bound buyer homed on another
    /// nest*, rules (1) and (2): the subscribe to the tier the link sells goes
    /// FIRST, through [`Self::subscribe_across_nests`]'s dispatch (local for no
    /// home, the follow twin at `home_nest_url` otherwise; `Approved` is a
    /// renewal and proceeds like `Queued`), and only then is the checkout URL
    /// composed by [`fauna_core::subscription::checkout_url`] for the shell to
    /// open. So the buyer's intent reaches the author's nest ahead of the
    /// payment and the two halves meet on one request row.
    ///
    /// `Ok(None)` — and no subscribe — for a `payment_url` the safety guard
    /// refuses: there is nothing to open. A refused subscribe is the error and
    /// opens nothing (a payment to a tier nobody can grant is the half-row the
    /// ruling avoids).
    pub async fn subscribe_and_checkout(
        &self,
        author_id: ActorId,
        tier: impl Into<String>,
        payment_url: &str,
        buyer: &ActorKeypair,
        home_nest_url: Option<String>,
    ) -> Result<Option<CheckoutOpen>, FollowError<R::Error>> {
        let Some(url) = fauna_core::subscription::checkout_url(payment_url, &buyer.actor_id())
        else {
            return Ok(None);
        };
        let reply = self
            .subscribe_across_nests(author_id, tier, buyer, home_nest_url)
            .await?;
        Ok(Some(CheckoutOpen { reply, url }))
    }

    /// The leave twin of [`Self::subscribe_across_nests`]: [`Self::unsubscribe`]
    /// for an author on this nest, the federated unsubscribe for one homed on
    /// another. `tier` is the tier being left and the leave's floor
    /// (`subscriptions.md` § The free followers tier — follow and unfollow,
    /// rule 1): the leave ends it and every tier held above it, so
    /// `"followers"` is the unfollow. The unsubscribe intent names it, and the
    /// local door takes it as [`UnsubscribeRequest::tier`].
    pub async fn unsubscribe_across_nests(
        &self,
        author_id: ActorId,
        tier: impl Into<String>,
        subscriber: &ActorKeypair,
        home_nest_url: Option<String>,
    ) -> Result<UnsubscribeReply, FollowError<R::Error>> {
        let tier = tier.into();
        let Some(home) = normalize_home_nest_url(home_nest_url) else {
            return self
                .unsubscribe(author_id, Some(tier))
                .await
                .map_err(FollowError::Nest);
        };
        let intent = self
            .follow_intent(subscriber, author_id, &tier, FollowIntentKind::Unsubscribe)
            .await?;
        self.nest
            .request(
                "fauna.subscriptions.unsubscribe",
                UnsubscribeRequest {
                    home_nest_url: Some(home),
                    intent: Some(intent),
                    author_id,
                    tier: Some(tier),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(FollowError::Nest)
    }

    /// Mint the subscriber's intent naming the nest this client is connected
    /// to — its id as `fauna.nest.info` states it, the id the nest compares
    /// against its own key before relaying. The id is fixed for the life of a
    /// connection, so it comes off the transport's per-connection memo
    /// ([`fauna_protocol::read_nest_info`]).
    async fn follow_intent(
        &self,
        subscriber: &ActorKeypair,
        author: ActorId,
        tier: &str,
        kind: FollowIntentKind,
    ) -> Result<EmbedAsBytes, FollowError<R::Error>> {
        let home_nest_id = fauna_protocol::read_nest_id(&self.nest).await?;
        mint_follow_intent(
            subscriber,
            author,
            tier,
            kind,
            home_nest_id,
            Timestamp::now(),
        )
        .map_err(|e| FollowError::Intent(e.to_string()))
    }

    /// `fauna.subscriptions.encaps_key.publish` — publish the caller's
    /// **subscriber encapsulation-key registry entry** (`post-quantum.md`
    /// § surface B): the identity-seed-derived ML-KEM-768 key an author wraps
    /// hybrid `KeyBlob` entries to when the subscriber is not the one asking —
    /// a third-party follow under `fauna:follow:write`
    /// (`authorization-server.md` § Scope grammar → *The sixth arm*), which
    /// the nest refuses while the entry is absent. Called at every session
    /// start, the way the mail epoch schedule is refreshed: idempotent, every
    /// device of the account derives the same bytes, and an identity
    /// succession's successor publishes its own on its first session.
    pub async fn publish_encaps_key(&self, subscriber: &ActorKeypair) -> Result<(), R::Error> {
        let _: EncapsKeyPublishReply = self
            .nest
            .request(
                "fauna.subscriptions.encaps_key.publish",
                encaps_key_publish_request(subscriber),
            )
            .await?;
        Ok(())
    }

    /// Whether the home nest advertises capability `token` in its
    /// `fauna.nest.info` reply (`fauna_protocol::discovery::capability`). An old
    /// nest that omits the token — or omits the whole `capabilities` field —
    /// reads as `false` (the standard non-erroring degrade). Mirrors the
    /// mail-settings `nest_supports` seam. Every caller passes a static token,
    /// whose answer is fixed for the connection, so the read comes off the
    /// transport's per-connection memo ([`fauna_protocol::read_nest_info`]).
    pub async fn nest_supports(&self, token: &str) -> Result<bool, R::Error> {
        let info = fauna_protocol::read_nest_info(&self.nest).await?;
        Ok(capability::supports(&info.capabilities, token))
    }

    /// `fauna.subscriptions.unsubscribe` — the calling actor leaves `tier` at
    /// `author_id` and every tier it holds there at a higher rank (the leave's
    /// floor — `subscriptions.md` § The free followers tier — follow and
    /// unfollow, rule 1): `Some("followers")` is the unfollow, `None` leaves
    /// every paid tier and keeps the follow. Resolves to `Queued`: the author's
    /// client commits each tier's leave.
    pub async fn unsubscribe(
        &self,
        author_id: ActorId,
        tier: Option<String>,
    ) -> Result<UnsubscribeReply, R::Error> {
        self.nest
            .request(
                "fauna.subscriptions.unsubscribe",
                UnsubscribeRequest {
                    home_nest_url: None,
                    intent: None,
                    author_id,
                    tier,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.subscriptions.status.get` — the calling actor's current
    /// subscription status for `author_id` (active tier + expiry, plus the
    /// author's auto-approve flag). Replay-safe pure read.
    pub async fn status_get(&self, author_id: ActorId) -> Result<StatusGetReply, R::Error> {
        self.nest
            .request(
                "fauna.subscriptions.status.get",
                StatusGetRequest {
                    author_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.subscriptions.post_unlock.get` — the buyer's price read for one
    /// sold post (`monetization.md` § Per-post pay-to-unlock → *the buyer's
    /// price read is post-addressed*, ruled 2026-07-29): the sold post's
    /// public purchase fields (tier name, price hint, payment URL),
    /// post-addressed and keyed `(author_id, post_id)`. `Ok(None)` iff no
    /// tier of `author_id`'s designates `post_id` — an undesignated post, a
    /// foreign/unknown author, or (via the transport `Err` arm a caller
    /// folds to the same priceless-teaser render) any error are all the same
    /// empty reply (anti-enumeration). Authenticated USER-class, ordinary dispatch
    /// limits — an indexed point read, no bespoke throttle.
    pub async fn post_unlock_get(
        &self,
        author_id: ActorId,
        post_id: impl Into<String>,
    ) -> Result<Option<PostUnlockOffer>, R::Error> {
        let reply: PostUnlockGetReply = self
            .nest
            .request(
                "fauna.subscriptions.post_unlock.get",
                PostUnlockGetRequest {
                    author_id,
                    post_id: post_id.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.offer)
    }

    /// `fauna.subscriptions.tiers.list` — the calling author's own tier
    /// definitions (name, rank, price hint, payment URL, auto-approve),
    /// ascending by rank. Replay-safe pure read. This is the authenticated
    /// own-read powering the profile Tiers-tab SELF "My tiers" list; the public
    /// per-author HTTP read serves the unauthenticated / another-creator case.
    pub async fn tiers_list(&self) -> Result<Vec<TierItem>, R::Error> {
        let reply: TiersListReply = self
            .nest
            .request("fauna.subscriptions.tiers.list", TiersListRequest {})
            .await?;
        Ok(reply.tiers)
    }

    /// `fauna.subscriptions.offers.list` — **another** author's offered tier
    /// definitions (name, rank, price hint, payment URL), ascending by rank.
    /// The subscriber-browse read for the profile Tiers tab when viewing someone
    /// else's profile (`profile.md` § Layout & flow; `monetization.md`
    /// § Pillar 1 — `subscription-offers-section`). Replay-safe pure read.
    /// Unlike [`Self::tiers_list`] (the bearer-keyed own-read), this takes the
    /// target `author_id`; it is the authenticated WS-RPC successor to the
    /// public per-author HTTP read (the HTTP route stays for unauthenticated
    /// external consumers). Reuses the `TiersListReply` shape.
    pub async fn offers_list(&self, author_id: ActorId) -> Result<Vec<TierItem>, R::Error> {
        let reply: TiersListReply = self
            .nest
            .request(
                "fauna.subscriptions.offers.list",
                OffersListRequest {
                    author_id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.tiers)
    }

    /// The Tiers-tab browse every app calls — [`Self::offers_list`] for an
    /// author on this nest, and `fauna.subscriptions.offers.list_remote` for
    /// one homed on another (`subscriptions.md` § Pillar 1 → *The offers browse
    /// across nests*). `home` is the page's `actor_home` slot; `None` or blank
    /// is this nest. The nest answers a hosted author locally whatever home is
    /// named, so a stale home costs nothing; any other author is read from the
    /// home through the nest and forwarded unstored. The reply is the home's
    /// word — the same withholdings as the local door.
    pub async fn offers_list_across_nests(
        &self,
        author_id: ActorId,
        home: Option<String>,
    ) -> Result<Vec<TierItem>, R::Error> {
        let Some(nest_url) = normalize_home_nest_url(home) else {
            return self.offers_list(author_id).await;
        };
        let reply: TiersListReply = self
            .nest
            .request(
                "fauna.subscriptions.offers.list_remote",
                OffersListRemoteRequest {
                    author_id,
                    nest_url,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.tiers)
    }

    /// `fauna.subscriptions.mine.list` — the calling actor's own subscriptions
    /// across **every** creator (active + pending), each carrying the creator's
    /// nest-resolved handle (or `None` → the client renders the hex actor id).
    /// Caller-scoped, replay-safe pure read. This is the consumer-side
    /// enumeration powering the `subscription-settings` page `subscription-mine-list`;
    /// distinct from `status_get`, which is per-creator.
    pub async fn mine_list(&self) -> Result<Vec<MineSubscription>, R::Error> {
        let reply: MineListReply = self
            .nest
            .request("fauna.subscriptions.mine.list", MineListRequest {})
            .await?;
        Ok(reply.subscriptions)
    }

    // ── author-side request queue ────────────────────────────────────────

    /// `fauna.subscriptions.requests.list` — the calling author's pending
    /// subscribe/unsubscribe requests awaiting confirmation. Replay-safe
    /// pure read.
    pub async fn requests_list(&self) -> Result<Vec<PendingRequest>, R::Error> {
        let reply: RequestsListReply = self
            .nest
            .request("fauna.subscriptions.requests.list", RequestsListRequest {})
            .await?;
        Ok(reply.requests)
    }

    /// `fauna.subscriptions.requests.approve` — confirm a pending request.
    /// In encrypted mode the author's client mints a fresh broadcast
    /// `KeyBlob` covering the post-mutation roster and passes it as
    /// `encrypted_upload`; in plaintext mode the nest holds the period key
    /// and `encrypted_upload` is `None`. Returns the affected subscriber,
    /// tier, and new key version.
    pub async fn requests_approve(
        &self,
        request_id: i64,
        encrypted_upload: Option<EncryptedKeyBlobUpload>,
    ) -> Result<ApproveRequestReply, R::Error> {
        self.nest
            .request(
                "fauna.subscriptions.requests.approve",
                ApproveRequestRequest {
                    request_id,
                    encrypted_upload,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.subscriptions.requests.reject` — decline a pending request by
    /// id. Idempotent; returns whether a row transitioned to rejected.
    pub async fn requests_reject(&self, request_id: i64) -> Result<bool, R::Error> {
        let reply: RejectRequestReply = self
            .nest
            .request(
                "fauna.subscriptions.requests.reject",
                RejectRequestRequest {
                    request_id,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.rejected)
    }

    // ── subscribers roster ───────────────────────────────────────────────

    /// `fauna.subscriptions.subscribers.list` — the calling author's
    /// confirmed subscriber roster for `tier_name`. Replay-safe pure read.
    pub async fn subscribers_list(
        &self,
        tier_name: impl Into<String>,
    ) -> Result<Vec<SubscriberEntry>, R::Error> {
        let reply: SubscribersListReply = self
            .nest
            .request(
                "fauna.subscriptions.subscribers.list",
                SubscribersListRequest {
                    tier_name: tier_name.into(),
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.subscribers)
    }

    /// `fauna.subscriptions.subscribers.remove` — the calling author removes
    /// `subscriber_id` from `tier_name`. As with `requests_approve`, the
    /// encrypted-mode caller supplies a freshly minted `encrypted_upload`
    /// covering the post-removal roster; the plaintext caller passes `None`.
    pub async fn subscribers_remove(
        &self,
        tier_name: impl Into<String>,
        subscriber_id: ActorId,
        encrypted_upload: Option<EncryptedKeyBlobUpload>,
    ) -> Result<RemoveSubscriberReply, R::Error> {
        self.nest
            .request(
                "fauna.subscriptions.subscribers.remove",
                RemoveSubscriberRequest {
                    tier_name: tier_name.into(),
                    subscriber_id,
                    encrypted_upload,
                    extra: Default::default(),
                },
            )
            .await
    }

    // ── encrypted-mode key material fetches ──────────────────────────────

    /// `fauna.subscriptions.key_blob.get` — fetch the current broadcast
    /// `KeyBlob` (version + hash + dag-cbor-encoded blob) for `(author_id,
    /// tier_name)`. Replay-safe pure read.
    pub async fn key_blob_get(
        &self,
        author_id: ActorId,
        tier_name: impl Into<String>,
    ) -> Result<KeyBlobGetReply, R::Error> {
        self.nest
            .request(
                "fauna.subscriptions.key_blob.get",
                KeyBlobGetRequest {
                    author_id,
                    tier_name: tier_name.into(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.subscriptions.key_blob.rotate` — republish `tier_name`'s
    /// broadcast `KeyBlob` under a fresh period key with the roster unchanged.
    ///
    /// The author's own door, and the only one that re-keys a tier without a
    /// membership change: the post-succession rotation
    /// ([`crate::orchestration::SubscriptionsAuthor::rotate_period_keys_after_succession`])
    /// is its caller. Gate on
    /// [`capability::SUBSCRIPTION_PERIOD_ROTATE`](fauna_protocol::discovery::capability::SUBSCRIPTION_PERIOD_ROTATE)
    /// before calling — against a nest without it this is an unknown kind, and
    /// the caller must be able to tell that from an outage.
    pub async fn key_blob_rotate(
        &self,
        tier_name: impl Into<String>,
        encrypted_upload: EncryptedKeyBlobUpload,
    ) -> Result<RotateKeyBlobReply, R::Error> {
        self.nest
            .request(
                "fauna.subscriptions.key_blob.rotate",
                RotateKeyBlobRequest {
                    tier_name: tier_name.into(),
                    encrypted_upload,
                    extra: Default::default(),
                },
            )
            .await
    }

    // ── delegate ─────────────────────────────────────────────────────────

    /// `fauna.subscriptions.delegate.upload` — upload a signed
    /// `DeviceAuthorization` delegating the bearer's nest-key so the nest can
    /// act on the author's behalf (plaintext-mode key custody). Returns
    /// whether the delegation was stored.
    pub async fn delegate_upload(&self, authorization: EmbedAsBytes) -> Result<bool, R::Error> {
        let reply: DelegateUploadReply = self
            .nest
            .request(
                "fauna.subscriptions.delegate.upload",
                DelegateUploadRequest {
                    authorization,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.uploaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};
    use fauna_protocol::discovery::NestInfoReply;

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = SubscriptionsClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The construction-only `MockRequester` above can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: each `SubscriptionsClient` method
    // must send its exact `fauna.subscriptions.*` kind and a payload that
    // round-trips back to the typed request. No nest-side conformance test
    // routes through this adapter's *literal* kind strings, and several of
    // these methods (the encrypted-mode key-material fetches, delegate-upload)
    // have no web call-site yet — so an adapter-method kind rename would break
    // them silently the moment a UI lands. The pattern mirrors
    // `fauna-client-events` / `-snapshots` / `-sync`'s `RecordingRequester`
    // (transport-free, so it runs on every target including wasm); real
    // end-to-end round-trip conformance lives in
    // `bins/fauna-nest/tests/subscription_ws_rpc.rs`.

    use fauna_core::data::Timestamp;
    use fauna_protocol::ByteBuf;

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        use subscriptions::*;
        match kind {
            "fauna.subscriptions.tiers.create" => {
                fauna_protocol::encode_canonical(&TierCreateReply {
                    extra: Default::default(),
                    created: true,
                })
            }
            "fauna.subscriptions.tiers.update" => {
                fauna_protocol::encode_canonical(&TierUpdateReply {
                    extra: Default::default(),
                    updated: true,
                })
            }
            "fauna.subscriptions.tiers.delete" => {
                fauna_protocol::encode_canonical(&TierDeleteReply {
                    extra: Default::default(),
                    deleted: true,
                })
            }
            "fauna.subscriptions.subscribe" => {
                fauna_protocol::encode_canonical(&SubscribeReply::Queued { request_id: 7 })
            }
            "fauna.subscriptions.unsubscribe" => {
                fauna_protocol::encode_canonical(&UnsubscribeReply::Removed)
            }
            "fauna.subscriptions.status.get" => fauna_protocol::encode_canonical(&StatusGetReply {
                extra: Default::default(),
                tier: None,
                expires_at: None,
                auto_approve: false,
                follow_pending: false,
            }),
            "fauna.subscriptions.requests.list" => {
                fauna_protocol::encode_canonical(&RequestsListReply {
                    extra: Default::default(),
                    requests: vec![],
                })
            }
            "fauna.subscriptions.requests.approve" => {
                fauna_protocol::encode_canonical(&ApproveRequestReply {
                    extra: Default::default(),
                    subscriber: ActorId([0xAB; 32]),
                    tier: "gold".into(),
                    key_version: 1,
                })
            }
            "fauna.subscriptions.requests.reject" => {
                fauna_protocol::encode_canonical(&RejectRequestReply {
                    extra: Default::default(),
                    rejected: true,
                })
            }
            "fauna.subscriptions.subscribers.list" => {
                fauna_protocol::encode_canonical(&SubscribersListReply {
                    extra: Default::default(),
                    subscribers: vec![],
                })
            }
            "fauna.subscriptions.subscribers.remove" => {
                fauna_protocol::encode_canonical(&RemoveSubscriberReply {
                    extra: Default::default(),
                    subscriber: ActorId([0xAB; 32]),
                    tier: "gold".into(),
                    key_version: 2,
                })
            }
            "fauna.subscriptions.key_blob.get" => {
                fauna_protocol::encode_canonical(&KeyBlobGetReply {
                    extra: Default::default(),
                    version: 1,
                    blob_hash: ByteBuf::from(vec![0x01u8; 32]),
                    blob_data: ByteBuf::from(vec![0x02u8; 8]),
                })
            }
            "fauna.subscriptions.encaps_key.publish" => {
                fauna_protocol::encode_canonical(&EncapsKeyPublishReply::default())
            }
            "fauna.subscriptions.delegate.upload" => {
                fauna_protocol::encode_canonical(&DelegateUploadReply {
                    extra: Default::default(),
                    uploaded: true,
                })
            }
            "fauna.subscriptions.offers.list" | "fauna.subscriptions.offers.list_remote" => {
                fauna_protocol::encode_canonical(&TiersListReply {
                    extra: Default::default(),
                    tiers: vec![TierItem {
                        name: "gold".into(),
                        rank: 20,
                        description: Some("Top tier".into()),
                        price_hint: Some("$10/mo".into()),
                        payment_url: Some("https://pay.example/gold".into()),
                        auto_approve: false,
                        created_at: Timestamp(1000),
                        unlocks_post: None,
                        asking_price: None,
                        hidden: false,
                        extra: Default::default(),
                    }],
                })
            }
            "fauna.subscriptions.mine.list" => fauna_protocol::encode_canonical(&MineListReply {
                extra: Default::default(),
                subscriptions: vec![MineSubscription {
                    home_nest_url: None,
                    extra: Default::default(),
                    author_id: ActorId([0xAB; 32]),
                    tier: "gold".into(),
                    status: "active".into(),
                    handle: Some("alice".into()),
                    since: Timestamp(1000),
                }],
            }),
            "fauna.nest.info" => fauna_protocol::encode_canonical(&NestInfoReply {
                nest_id: hex::encode(OWN_NEST_ID),
                ..Default::default()
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    /// The nest id the reply table's `fauna.nest.info` answers with — the id a
    /// minted follow intent must name as the subscriber's home.
    const OWN_NEST_ID: [u8; 32] = [0x42; 32];

    /// A 32-byte actor id with a distinguishable fill.
    fn actor() -> ActorId {
        ActorId([0xCD; 32])
    }

    /// Minimal valid `EncryptedKeyBlobUpload` — the 100-byte envelope is the
    /// only length the wire shape constrains; the bytes are opaque here.
    fn upload() -> EncryptedKeyBlobUpload {
        EncryptedKeyBlobUpload {
            extra: Default::default(),
            key_blob: EmbedAsBytes {
                envelope: vec![0u8; 100],
                bytes: vec![0xEE; 4],
                signer_auth: None,
            },
            signer_auth: EmbedAsBytes {
                envelope: vec![1u8; 100],
                bytes: vec![0xDD; 4],
                signer_auth: None,
            },
        }
    }

    #[test]
    fn tiers_create_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.tiers_create(
            "gold",
            3,
            Some("Gold tier".into()),
            None,
            None,
            true,
            upload(),
            None,
            None,
            false,
        ))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.tiers.create");
        let req: TierCreateRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name, "gold");
        assert_eq!(req.rank, 3);
        assert!(req.auto_approve);
        assert_eq!(
            req.encrypted_upload,
            upload(),
            "the birth blob rides the create"
        );
    }

    #[test]
    fn tiers_update_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.tiers_update(
            "gold",
            Some(5),
            None,
            Some("$10".into()),
            None,
            Some(false),
            None,
        ))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.tiers.update");
        let req: TierUpdateRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name, "gold");
        assert_eq!(req.rank, Some(5));
        assert_eq!(req.price_hint.as_deref(), Some("$10"));
    }

    #[test]
    fn tiers_delete_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.tiers_delete("gold")).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.tiers.delete");
        let req: TierDeleteRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name, "gold");
    }

    #[test]
    fn subscribe_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.subscribe(actor(), "gold")).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.subscribe");
        let req: SubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.author_id, actor());
        assert_eq!(req.tier, "gold");
    }

    #[test]
    fn unsubscribe_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.unsubscribe(actor(), Some("followers".into()))).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.unsubscribe");
        let req: UnsubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.author_id, actor());
        assert_eq!(
            req.tier.as_deref(),
            Some("followers"),
            "the leave's floor rides"
        );

        // No tier: the field is absent on the wire (the paid-tiers leave).
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.unsubscribe(actor(), None)).expect("infallible mock");
        let req: UnsubscribeRequest =
            fauna_protocol::decode_strict(&rec.recorded().1).expect("decodes");
        assert_eq!(req.tier, None);
    }

    #[test]
    fn status_get_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.status_get(actor())).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.status.get");
        let req: StatusGetRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.author_id, actor());
    }

    #[test]
    fn requests_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.requests_list()).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.requests.list");
        let _req: RequestsListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn mine_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        let subs = block_on(client.mine_list()).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.mine.list");
        let _req: MineListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        // The reply decodes into the typed consumer rows.
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].tier, "gold");
        assert_eq!(subs[0].status, "active");
        assert_eq!(subs[0].handle.as_deref(), Some("alice"));
    }

    #[test]
    fn offers_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        let tiers = block_on(client.offers_list(actor())).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.offers.list");
        let req: OffersListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.author_id, actor());
        // The reply decodes into the typed tier offerings.
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].name, "gold");
        assert_eq!(tiers[0].price_hint.as_deref(), Some("$10/mo"));
    }

    /// `subscriptions.md` § Pillar 1 → *The offers browse across nests*, the
    /// client leg: a home selects the relay kind, carrying it normalized; no
    /// home — or a blank one — is the local `offers.list`.
    #[test]
    fn offers_list_across_nests_picks_the_kind_by_home() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        let tiers = block_on(
            client.offers_list_across_nests(actor(), Some(" https://home.example/ ".into())),
        )
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.offers.list_remote");
        let req: OffersListRemoteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.author_id, actor());
        assert_eq!(
            Some(req.nest_url),
            normalize_home_nest_url(Some(" https://home.example/ ".into()))
        );
        assert_eq!(tiers[0].name, "gold");

        for home in [None, Some("   ".to_string())] {
            let rec = std::sync::Arc::new(RecordingRequester::new(reply));
            let client = SubscriptionsClient::new(rec.clone());
            block_on(client.offers_list_across_nests(actor(), home)).expect("infallible mock");
            assert_eq!(rec.recorded().0, "fauna.subscriptions.offers.list");
        }
    }

    #[test]
    fn requests_approve_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.requests_approve(42, Some(upload()))).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.requests.approve");
        let req: ApproveRequestRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.request_id, 42);
        assert!(req.encrypted_upload.is_some());
    }

    #[test]
    fn publish_encaps_key_sends_the_accounts_mlkem_encaps_key() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        let subscriber = ActorKeypair::from_secret([0x5B; 32]);
        block_on(client.publish_encaps_key(&subscriber)).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.encaps_key.publish");
        let req: EncapsKeyPublishRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.mlkem_encaps_key.as_ref(),
            subscriber_mlkem_encaps_key(&subscriber).as_slice()
        );
    }

    #[test]
    fn requests_reject_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.requests_reject(42)).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.requests.reject");
        let req: RejectRequestRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.request_id, 42);
    }

    #[test]
    fn subscribers_list_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.subscribers_list("gold")).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.subscribers.list");
        let req: SubscribersListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.tier_name, "gold");
    }

    #[test]
    fn subscribers_remove_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.subscribers_remove("gold", actor(), None)).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.subscribers.remove");
        let req: RemoveSubscriberRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.tier_name, "gold");
        assert_eq!(req.subscriber_id, actor());
        assert!(req.encrypted_upload.is_none());
    }

    #[test]
    fn key_blob_get_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.key_blob_get(actor(), "gold")).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.key_blob.get");
        let req: KeyBlobGetRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.author_id, actor());
        assert_eq!(req.tier_name, "gold");
    }

    #[test]
    fn delegate_upload_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.delegate_upload(EmbedAsBytes {
            envelope: vec![2u8; 100],
            bytes: vec![0xAA; 4],
            signer_auth: None,
        }))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.delegate.upload");
        let req: DelegateUploadRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.authorization.envelope.len(), 100);
        assert_eq!(req.authorization.bytes, vec![0xAA; 4]);
    }

    // ── The federated follow (`federated-follow.md` § Decision 2) ───────────

    /// Decode and verify a carried intent exactly as both nests do: the
    /// authoring envelope under `Capability::Follow`.
    fn verified_intent(wire: &EmbedAsBytes) -> FollowIntent {
        let (bytes, env) = wire.clone().into_signed().expect("embed-as-bytes");
        let intent: FollowIntent =
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

    /// A follow of an author on this nest names no home and carries no
    /// intent — today's subscribe, ek published — and never reads the nest id.
    #[test]
    fn a_follow_naming_no_home_is_the_local_subscribe() {
        let me = ActorKeypair::generate();
        for home in [None, Some("   ".to_string())] {
            let rec = std::sync::Arc::new(RecordingRequester::new(reply));
            let client = SubscriptionsClient::new(rec.clone());
            block_on(client.subscribe_across_nests(actor(), "followers", &me, home))
                .expect("infallible mock");
            assert_eq!(rec.kinds(), vec!["fauna.subscriptions.subscribe"]);
            let (_, payload) = rec.recorded();
            let req: SubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
            assert_eq!(req.home_nest_url, None);
            assert!(req.intent.is_none(), "a local follow carries no intent");
            assert!(req.mlkem_encaps_key.is_some(), "the ek is still published");
        }
    }

    /// A follow of an author homed elsewhere carries the home and the
    /// subscriber's own signed intent, naming the nest the client is
    /// connected to — the one nest that may relay it.
    #[test]
    fn a_follow_naming_a_home_carries_the_subscribers_signed_intent_naming_this_nest() {
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.subscribe_across_nests(
            actor(),
            "followers",
            &me,
            Some(" https://home.test/ ".into()),
        ))
        .expect("infallible mock");

        assert_eq!(
            rec.kinds(),
            vec!["fauna.nest.info", "fauna.subscriptions.subscribe"]
        );
        let (_, payload) = rec.recorded();
        let req: SubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.home_nest_url.as_deref(), Some("https://home.test"));
        assert_eq!(req.author_id, actor());
        assert_eq!(req.tier, "followers");
        assert!(req.mlkem_encaps_key.is_some());
        let intent = verified_intent(req.intent.as_ref().expect("the intent rides"));
        assert_eq!(intent.subscriber, me.actor_id());
        assert_eq!(intent.author, actor());
        assert_eq!(intent.tier, "followers");
        assert_eq!(intent.kind, FollowIntentKind::Subscribe);
        assert_eq!(intent.home_nest_id, OWN_NEST_ID);
    }

    /// The leave twin: the intent states `unsubscribe` and the tier being left
    /// (the home stamps its replay floor under it).
    #[test]
    fn an_unfollow_naming_a_home_carries_an_unsubscribe_intent_for_the_tier_left() {
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.unsubscribe_across_nests(
            actor(),
            "followers",
            &me,
            Some("https://home.test".into()),
        ))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.subscriptions.unsubscribe");
        let req: UnsubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.home_nest_url.as_deref(), Some("https://home.test"));
        assert_eq!(req.tier.as_deref(), Some("followers"));
        let intent = verified_intent(req.intent.as_ref().expect("the intent rides"));
        assert_eq!(intent.kind, FollowIntentKind::Unsubscribe);
        assert_eq!(intent.tier, "followers");
        assert_eq!(intent.subscriber, me.actor_id());
        assert_eq!(intent.home_nest_id, OWN_NEST_ID);

        // No home: the plain leave, no intent and no nest-id read.
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.unsubscribe_across_nests(actor(), "followers", &me, None))
            .expect("infallible mock");
        assert_eq!(rec.kinds(), vec!["fauna.subscriptions.unsubscribe"]);
        let req: UnsubscribeRequest =
            fauna_protocol::decode_strict(&rec.recorded().1).expect("decodes");
        assert!(req.intent.is_none() && req.home_nest_url.is_none());
        assert_eq!(
            req.tier.as_deref(),
            Some("followers"),
            "the local arm passes the tier as the leave's floor too"
        );
    }

    /// The capability checks and the follow intent read `fauna.nest.info`
    /// through the transport's per-connection memo (`security.md` § Transport
    /// trust → *The nest's self-description is read once per connection*):
    /// every one reads a fact fixed for the connection — a static capability
    /// token, the nest's own id — so on one connection they share ONE read
    /// of the actor's discovery budget instead of one each.
    #[test]
    fn capability_checks_and_the_follow_intent_share_one_nest_info_read() {
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::on_connection(reply, 7));
        let client = SubscriptionsClient::new(rec.clone());
        block_on(client.nest_supports(capability::SUBSCRIPTION_PERIOD_ROTATE))
            .expect("infallible mock");
        block_on(client.nest_supports(capability::SUBSCRIPTION_PERIOD_ROTATE))
            .expect("infallible mock");
        block_on(client.subscribe_across_nests(
            actor(),
            "followers",
            &me,
            Some("https://home.test".into()),
        ))
        .expect("infallible mock");
        assert_eq!(
            rec.kinds(),
            vec!["fauna.nest.info", "fauna.subscriptions.subscribe"],
            "two capability checks and a follow intent on one connection must \
             draw one fauna.nest.info, not one each"
        );
    }

    /// A nest whose `nest.info` names no well-formed id gets no intent: an
    /// intent naming a guessed home would be refused there anyway, so the
    /// follow fails before anything is sent.
    #[test]
    fn a_malformed_own_nest_id_sends_no_follow() {
        fn bad_info(kind: &'static str) -> Vec<u8> {
            match kind {
                "fauna.nest.info" => fauna_protocol::encode_canonical(&NestInfoReply {
                    nest_id: "not-hex".into(),
                    ..Default::default()
                })
                .expect("encode")
                .to_vec(),
                other => reply(other),
            }
        }
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::new(bad_info));
        let client = SubscriptionsClient::new(rec.clone());
        let err = block_on(client.subscribe_across_nests(
            actor(),
            "followers",
            &me,
            Some("https://home.test".into()),
        ))
        .expect_err("no intent without this nest's id");
        assert!(matches!(err, FollowError::Intent(_)), "{err}");
        assert_eq!(rec.kinds(), vec!["fauna.nest.info"]);
    }

    // ── The payment-link gesture: subscribe first, then the checkout ───────

    /// The local door: the subscribe goes out (and only it), and the URL
    /// handed back carries the buyer's own actor id on a Stripe host.
    #[test]
    fn subscribe_and_checkout_subscribes_then_composes_the_reference() {
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        let open = block_on(client.subscribe_and_checkout(
            actor(),
            "gold",
            "https://buy.stripe.com/test_abc",
            &me,
            None,
        ))
        .expect("infallible mock")
        .expect("a safe URL opens");
        assert_eq!(rec.kinds(), vec!["fauna.subscriptions.subscribe"]);
        let (_, payload) = rec.recorded();
        let req: SubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.tier, "gold");
        assert_eq!(req.home_nest_url, None);
        assert_eq!(open.reply, SubscribeReply::Queued { request_id: 7 });
        assert_eq!(
            open.url,
            format!(
                "https://buy.stripe.com/test_abc?client_reference_id={}",
                me.actor_id().to_hex()
            )
        );
    }

    /// The remote door: the follow twin at the page's home, the intent
    /// signed by the buyer, before the checkout; a non-Stripe host opens
    /// untouched.
    #[test]
    fn subscribe_and_checkout_takes_the_follow_twin_for_a_named_home() {
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        let open = block_on(client.subscribe_and_checkout(
            actor(),
            "gold",
            "https://ko-fi.com/author",
            &me,
            Some("https://home.test".into()),
        ))
        .expect("infallible mock")
        .expect("a safe URL opens");
        assert_eq!(
            rec.kinds(),
            vec!["fauna.nest.info", "fauna.subscriptions.subscribe"]
        );
        let (_, payload) = rec.recorded();
        let req: SubscribeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.home_nest_url.as_deref(), Some("https://home.test"));
        let intent = verified_intent(req.intent.as_ref().expect("the intent rides"));
        assert_eq!(intent.subscriber, me.actor_id());
        assert_eq!(intent.tier, "gold");
        assert_eq!(open.url, "https://ko-fi.com/author");
    }

    /// An unsafe link subscribes to nothing and opens nothing.
    #[test]
    fn subscribe_and_checkout_refuses_an_unsafe_url_before_any_request() {
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SubscriptionsClient::new(rec.clone());
        let open = block_on(client.subscribe_and_checkout(
            actor(),
            "gold",
            "http://buy.stripe.com/x",
            &me,
            None,
        ))
        .expect("infallible mock");
        assert_eq!(open, None);
        assert!(rec.kinds().is_empty(), "nothing sent: {:?}", rec.kinds());
    }

    /// A refused subscribe is the error: no checkout URL comes back.
    #[test]
    fn subscribe_and_checkout_opens_nothing_when_the_subscribe_is_refused() {
        let me = ActorKeypair::generate();
        let rec = std::sync::Arc::new(fauna_client_testkit::FailingRequester::new("refused"));
        let client = SubscriptionsClient::new(rec.clone());
        let err = block_on(client.subscribe_and_checkout(
            actor(),
            "gold",
            "https://buy.stripe.com/x",
            &me,
            None,
        ))
        .expect_err("the subscribe failed");
        assert!(matches!(err, FollowError::Nest(_)), "{err}");
        assert_eq!(rec.kinds(), vec!["fauna.subscriptions.subscribe"]);
    }
}
