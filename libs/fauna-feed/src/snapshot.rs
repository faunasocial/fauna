//! The shared feed snapshot — the single read model the Feed page renders
//! from on every app (`docs/goal/ui/feed.md` § State & data shape, ratified
//! 2026-06-14). No client open-codes post-list state (priorities #1/#2/#3); the
//! manager owns it and hands down this cheap-to-clone projection.
//!
//! UniFFI/WASM-exposed, so no serde-`flatten` maps: `feeds` is a clean
//! [`FeedSummaryView`] projection of the wire `FeedSummary` (which carries an
//! `extra: BTreeMap`), exactly as `ConversationsSnapshot` projects its threads.

use fauna_core::localized::LocalizedText;
use fauna_core::render::{AuthoringOriginStatus, RenderDocument, VerificationStatus};
use serde::{Deserialize, Serialize};

use crate::compose::{BridgeFormState, FeedComposeState};

/// Whether the post list is mid-load, loaded, or errored — drives the
/// empty/loading/error rendering (`feed.md` § Errors & edge cases: empty is
/// `status == Loaded && posts.is_empty()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FeedStatus {
    /// The initial fetch (or a re-query) is in flight; the list is not yet
    /// authoritative. The default for a freshly-constructed manager.
    #[default]
    Loading,
    /// The post list reflects the latest successful query. Empty-state is
    /// `Loaded` with `posts.is_empty()`.
    Loaded,
    /// The last query failed; `FeedSnapshot.error` carries the reason.
    Error,
}

/// Which empty state the Feed page shows — the answer of
/// [`FeedSnapshot::empty_state`] (`feed.md` § Errors & edge cases: two
/// elements, at most one present at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FeedEmptyState {
    /// `feed-empty-state` — the loaded feed holds no posts and no search is
    /// active (copy `feed.list.no_posts`).
    NoPosts,
    /// `feed-no-results` — an active search matched no posts (copy
    /// `feed.list.no_matching_posts`).
    NoMatches,
}

/// Where this reader's words go when they reply to, or quote, a **restricted**
/// post (`ui/feed.md` § Encryption at rest → *Ruling 5's build — the shape*,
/// (d)). The manager's answer, derived at every snapshot read like
/// [`PostSummary::room_label`] — the reply dialog states it and never decides
/// it. A public target has no answer to state ([`PostSummary::reply_audience`]
/// is `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ReplyAudience {
    /// A room post whose room this device still holds a seat on
    /// ([`FeedSnapshot::own_rooms`]): the reply seals to the same room.
    SealedToRoom,
    /// The reader's own tier post, the tier still theirs
    /// ([`FeedSnapshot::own_tiers`]): the reply seals to the same tier.
    SealedToTier,
    /// Restricted, and this reader cannot author under its arm — a tier's
    /// subscriber, a reader off the room's floor. Their words go out public
    /// only by an explicit per-reply answer; unconfirmed, they are refused.
    PublicByConfirmation,
}

/// What [`crate::FeedManager::resolve_post`] found for a post addressed by id
/// (the deep-link door — `ui/search.md` § Where logic lives → *Result navigation
/// (deep link)*). The manager answers the question; each app decides how to
/// render each answer, from the same four cases on every platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum PostResolution {
    /// The timeline already holds it — [`FeedSnapshot::posts`] is the render
    /// source, no fetch was made and the deep-link slot is clear.
    Loaded,
    /// Fetched by id and parked in [`FeedSnapshot::deep_linked_post`].
    Fetched,
    /// Taken down under a legal obligation (`moderation.md` § Categories &
    /// enforcement item 1): the nest withholds the body from every viewer, so
    /// there is no post to render. The app surfaces
    /// `fauna_core::obligation::legal_takedown_tombstone(reference)` — the one
    /// shared tombstone string — rather than a blank surface.
    TakenDown { reference: String },
    /// Not found, quarantine-gated, undecodable, or the request failed. The
    /// surface degrades to its empty state, as it always has for a stale id.
    Unavailable,
}

/// What [`crate::FeedManager::playback_source`] decided a media block plays from
/// (`render-model.md` § D6c → *Inline playback*): the shared half of inline
/// playback. The player itself, and its position / paused / volume, are each app's
/// native glue — this is only the decision of *what plays*, made once for all 7 apps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum PlaybackSource {
    /// A nest-relative URL the native player streams directly (`/api/v1/blob/<hash>`
    /// for a public `Video`); the app prefixes its nest origin, as it does for images.
    Url { url: String },
    /// A `Video` item of a post this reader unlocked: no URL can play it. The app
    /// opens the blob through its existing sealed-media path (`open_media_bytes`)
    /// to an object URL or a hardened temp file and plays that — whole blob.
    Sealed { hash: String },
    /// Nothing to play — the block is not a playable video. `reason` is a short
    /// diagnostic for logs, never shown to the user.
    Unplayable { reason: String },
}

/// A feed in the selector list — the `FeedManager` projection of the wire
/// `fauna_protocol::feed::FeedSummary` that drops the forward-compat `extra`
/// map (UniFFI records can't carry serde-`flatten`), matching how
/// `ConversationsSnapshot` projects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FeedSummaryView {
    /// Hex feed id.
    pub feed_id: String,
    pub name: String,
    /// `all` / `any` rule-combination mode.
    pub combination: String,
    /// `local` / `discovery`.
    pub scope: String,
    pub contributor_seeds: Vec<String>,
}

/// A single post in the visible, ordered, deduplicated list — the
/// `FeedManager` projection of the wire `fauna_protocol::feed::FeedPostItem`.
/// The first seven fields are exactly ui.yaml `feed.state_fields`; `source`
/// (badges) and `quoted_post_id` (embed) are model fields beyond them.
///
/// `Default` is derived deliberately: this record has grown a field on nearly
/// every feed slice (verification, authoring origin, the four counters, gating,
/// labels, the unlock offer, the takedown reference), and each growth used to
/// break every hand-listed fixture at once. Fixtures build from
/// `..Default::default()` instead, so two branches growing it merge cleanly
/// rather than colliding on the grown axis — the standing convention for any
/// wire type still gaining fields. The
/// defaults are catalog-aligned, not arbitrary: `verification` is `Unchecked`
/// and `authoring_origin` `Unknown` — exactly what a nest-index projection with
/// no envelope to verify reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PostSummary {
    /// Hex `[u8; 32]` post id.
    pub post_id: String,
    /// Hex `[u8; 32]` author.
    pub author: String,
    /// A **bridged** author's face — the handle, display name and (proxied)
    /// avatar the origin bridge served, carried 1:1 from
    /// `FeedPostItem.author_display` (`docs/goal/behavior/bridges.md`
    /// § Unified feed ingestion → *Bridged authors*). `None` for a native
    /// author. Apps hand `display_name` and `handle` to
    /// `fauna_core::format::peer_display_label` in the self-published-name
    /// slot, so the viewer's own nickname still wins and the chain otherwise
    /// reads display name → handle → short id for every author alike.
    pub author_display: Option<AuthorDisplayView>,
    /// List-card body (the nest's 500-char FTS preview; full body on
    /// `post_detail`). Retained as the canonical text **source**: the
    /// [`document`](Self::document) below is its render projection, and the
    /// shared quoted-post projection ([`crate::quote::project_from_loaded`])
    /// reads it.
    pub body: String,
    /// The post body as the shared semantic [`RenderDocument`]
    /// (`docs/goal/architecture/render-model.md` § D6) — produced once by the
    /// manager from [`body`](Self::body) via
    /// `fauna_core::render::markdown_to_document`, the **same** model the
    /// Conversations page paints (D1). Every app walks this with its existing
    /// conversations document-walker instead of re-rendering the raw `body`
    /// string, retiring the per-app flat-text feed body renderers (priority
    /// #1/#4). Embeds (quoted post, media) **are** folded into the document in
    /// body order (after the text) by [`crate::manager::build_post_document`]:
    /// the lazy [`resolve_quoted_post`](crate::FeedManager::resolve_quoted_post)
    /// and [`resolve_media`](crate::FeedManager::resolve_media) rebuild the
    /// document with a [`fauna_core::render::RenderBlock::QuotedPost`] /
    /// [`fauna_core::render::RenderBlock::Image`] when they resolve, so each
    /// app paints `quoted-post` / `post-image` from the document. The sibling
    /// [`quoted_post_id`](Self::quoted_post_id) / [`media_hash`](Self::media_hash)
    /// fields persist (the former still drives the resolution trigger) until the
    /// cross-app field-deletion slice (render-model.md § D6).
    pub document: RenderDocument,
    /// Epoch-millis at the client boundary (`FeedPostItem.created_at` is
    /// micros; the manager divides by 1000).
    pub timestamp: i64,
    pub tags: Vec<String>,
    pub has_media: bool,
    pub is_reply: bool,
    /// Comma-separated protocol list → `classify_sources()` for badges.
    pub source: String,
    /// `Reference::Quote` target (hex post id) — drives the embedded
    /// quoted-post card. `None` when the post quotes nothing, or talking to a
    /// nest that doesn't yet project it.
    pub quoted_post_id: Option<String>,
    /// `Reference::Repost` target (hex post id) — `Some` marks this a **repost
    /// row** (`feed.md` § Interaction bar → Repost, ratified 2026-08-10): the
    /// app renders the `feed.post.reposted_marker` attribution in the author
    /// header, the original folds in through the same quoted-post embed
    /// ([`crate::FeedManager::resolve_quoted_post`] resolves either field),
    /// **no own interaction bar renders** (a repost's own counters are
    /// structurally dark), and card activation opens the ORIGINAL's detail.
    /// `None` for a non-repost row.
    pub reposted_post_id: Option<String>,
    /// The **viewer's own live repost post** naming this row's post (hex id).
    /// Presence = "reposted by me" → `feed-repost-button` renders active; the
    /// value is exactly what `unrepost` takes. Maintained locally by the
    /// [`crate::FeedManager::repost`] toggle between reloads. `None` on
    /// bridged rows.
    pub viewer_repost_id: Option<String>,
    /// The viewer's like-toggle state (the nest's `engagement_events` row that
    /// `like`/`unlike` maintain). Carrier for the like button's toggle state —
    /// per-app consumption is a follow-on (`feed.md` § Interaction bar →
    /// Repost). `false` when the viewer has not liked the post.
    pub viewer_liked: bool,
    /// The first media attachment's blob hash (64-hex), resolved lazily by
    /// [`crate::FeedManager::resolve_media`] for a `has_media` post — drives the
    /// `post-image` render. `None` until resolved (the feed-index projection
    /// never reads `content.payload`, where the body's `MediaItem` blob hash
    /// lives — `feed.md` § The read model — so the hash is a per-post lazy
    /// resolve, the direct analogue of the quoted-post fallback). A model field
    /// beyond ui.yaml `feed.state_fields`, like `source` / `quoted_post_id`.
    ///
    /// Also the fire-once resolve guard, so a resolve never leaves it `None`: a
    /// bridged post whose attachments carry no blob at all (every item a
    /// `remote_url`) resolves to `Some("")` (render-model.md § D6c). An app still
    /// painting from this field treats the empty string as none; the document's
    /// `RenderBlock::ProxiedImage` blocks are what paint such a post's pictures.
    pub media_hash: Option<String>,
    /// Whether **this client** cryptographically verified the post's signed
    /// envelope (`security.md` § App display of unverified content; F-CL2/F-CL3).
    /// A model field beyond ui.yaml `feed.state_fields`, like `source`, that drives
    /// the muted "unverified source" badge (rendered **iff**
    /// [`VerificationStatus::Failed`]). Defaults to
    /// [`Unchecked`](VerificationStatus::Unchecked) at [`crate::manager::map_post`]
    /// (a `FeedPostItem` is the home nest's trusted index projection with no
    /// envelope to verify); the manager flips it to `Verified`/`Failed` only where
    /// it actually decodes the raw signed body — today
    /// [`resolve_media`](crate::FeedManager::resolve_media), the one list-path that
    /// fetches `fauna.posts.get`. Post-detail and the quoted embed carry their own
    /// verification (the latter on [`QuotedPostView::verification`]).
    pub verification: VerificationStatus,
    /// Whether an **external app authored this post as the account**, via a
    /// delegated authoring sub-key — the D10 audit surface
    /// (`atproto-pds-full.md` § Problem 1 → D10 → *Audit*, ratified 2026-07-29;
    /// `principles.md` § capability grants are audited from the user's own app).
    /// Drives the `delegated-origin-badge`, rendered **iff**
    /// [`Delegated`](AuthoringOriginStatus::Delegated).
    ///
    /// Set on exactly the same paths and by exactly the same rule as
    /// [`Self::verification`] — both are answers only a client that decoded the
    /// **raw signed envelope** can give, so a nest-index projection stays
    /// [`Unknown`](AuthoringOriginStatus::Unknown). The two are separate fields
    /// rather than one enum because they answer independent questions (*did it
    /// verify* vs. *who signed it*), and a failed verification must report
    /// `Unknown` here rather than a cert claim nothing authenticated.
    pub authoring_origin: AuthoringOriginStatus,
    /// Interaction-bar counters (ratified 2026-06-27, `feed.md` § Interaction
    /// bar) — carried 1:1 from the wire [`FeedPostItem`] by
    /// [`crate::manager::map_post`]. Each app renders `icon + count` (count
    /// hidden at 0) for like / reply / repost / quote. Model fields beyond
    /// ui.yaml `feed.state_fields`, like `source` / `quoted_post_id`. `0` when
    /// talking to a nest that does not yet project them (additive + wire-default).
    pub like_count: i64,
    pub reply_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
    /// Tier name of a gated-to-tier post (`FeedPostItem.gated_tier` ←
    /// `content_meta.gated_tier`; `ui/feed.md` § Encryption at rest). Drives
    /// the `gated-post-badge` on the card. `None` = public post. The list `body` of a gated post is its
    /// plaintext teaser; the sealed full body renders only after
    /// [`crate::FeedManager::unlock_gated_post`].
    pub gated_tier: Option<String>,
    /// Hex channel id of the room a **room-restricted** post addresses
    /// (`FeedPostItem.gated_room` ← `content_meta.gated_room`, or the
    /// deep-link door's own decode of the `KeyAccess::Room` arm; `ui/feed.md`
    /// § Encryption at rest → *Room-restricted — the app half*, the card
    /// bullet). The raw fact the card's room reading rests on; `None` = not a
    /// room post. Additive: an app
    /// built before the field constructs without it.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub gated_room: Option<String>,
    /// The room's label **as this reader's own conversation list reads it**,
    /// `Some` exactly when [`gated_room`](Self::gated_room) names a room in
    /// [`FeedSnapshot::own_rooms`] — the reader sits on its floor. Derived at
    /// every snapshot read (never carried on the wire, never taken from
    /// anything the author sent), so it follows the conversations plane's own
    /// changes. What the card renders: `Some(label)` → the badge names the
    /// room (the `gate_room` string, the composer's own) and the card's
    /// detail-open is the member's "open"; `None` on a room post → the
    /// reserved tier in [`gated_tier`](Self::gated_tier), the outsider's
    /// card. Additive.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub room_label: Option<String>,
    /// Where this reader's reply to — or quote of — this post would go, `Some`
    /// exactly when the post is restricted ([`ReplyAudience`]). Derived at
    /// every snapshot read beside [`room_label`](Self::room_label), so it
    /// follows a lost seat or a retired tier with no second bookkeeping path.
    /// Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub reply_audience: Option<ReplyAudience>,
    /// The slug this post is published to the web under (`FeedPostItem.web_slug`
    /// ← the `content_links` web-publish row). `None` = not published.
    ///
    /// The **publish-state half** of the own-post web verbs on
    /// `feed-post-actions-menu` (`ui/feed.md` § User actions): `None` offers
    /// *Publish to web*, `Some` offers *Unpublish* + *Copy web link*, and
    /// `Some` **together with** [`gated_tier`](Self::gated_tier) additionally
    /// offers *Copy paywall link*. Deriving presence from this field is what
    /// keeps the menu identical on all 7 apps — no app queries publish state
    /// per row.
    pub web_slug: Option<String>,
    /// Set once [`crate::FeedManager::unlock_gated_post`] decrypted this
    /// gated post's full body into `body`/`document` (session-local render
    /// state; the at-rest and wire shapes stay sealed).
    pub gated_unlocked: bool,
    /// Per-category content-label verdicts (`FeedPostItem.labels` ←
    /// `content_labels`; `moderation.md` § Per-row badge data path, ratified
    /// 2026-07-16) — drives the `content-label-badge`. Empty when unlabelled,
    /// or talking to a nest that doesn't yet project labels.
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// The buyer's price read for a sold post (`monetization.md` § Per-post
    /// pay-to-unlock → *the buyer's price read is post-addressed*, ruled
    /// 2026-07-29) — resolved lazily by
    /// [`crate::FeedManager::resolve_post_unlock_offer`] when
    /// [`gated_tier`](Self::gated_tier) names a `post-unlock-*` tier
    /// (`fauna_client_subscriptions::UNLOCK_TIER_PREFIX`), the `resolve_media`
    /// pattern. `None` until resolved, or when the nest answers no offer for
    /// this post (any error degrades the same way) — the teaser then renders no price, and
    /// claim-code redemption (§5) stays the fallback purchase path. Drives
    /// `gated-post-price` / `gated-post-payment-link` / `gated-post-buy-button`.
    pub unlock_offer: Option<UnlockOfferView>,
    /// `Some(reference)` when this post has been **taken down under a legal
    /// obligation** (`moderation.md` § Categories & enforcement item 1): the
    /// nest withholds its body from every viewer, so [`body`](Self::body) /
    /// [`author`](Self::author) / [`document`](Self::document) are empty and each
    /// app paints the shared tombstone
    /// ([`fauna_core::obligation::legal_takedown_tombstone`] — "Removed under
    /// legal obligation ({reference})") **in the body area, in place of the
    /// post**, never a blank detail dialog.
    ///
    /// The exact twin of
    /// [`RenderBlock::QuotedPost::legal_takedown_ref`](fauna_core::render::RenderBlock::QuotedPost)
    /// (the quoted embed) and `MessageSnapshot::legal_takedown_ref` (the DM
    /// bubble) — one concept, one shape, one shared string, on all three
    /// surfaces (priorities #1/#3). Additive `Option`, `None` for every live
    /// post.
    ///
    /// Set only by [`crate::FeedManager::resolve_post`], the single-post
    /// deep-link door: it is the one read that decodes a `fauna.posts.get`
    /// reply, which is where the takedown marker lives. The list path
    /// (`query_feed`'s feed-index projection) never carries one, because the
    /// nest omits a taken-down post from feed queries outright.
    pub legal_takedown_ref: Option<String>,
    /// The post's tip surface (`monetization.md` § Tips) — total, count, and a
    /// bounded newest-first attribution window. Resolved lazily by
    /// [`crate::FeedManager::resolve_post_tips`], the `resolve_media` pattern.
    ///
    /// **`Some` is what closes the fire-once guard, so the resolver writes it
    /// on EVERY outcome — including a post with no tips at all.** That is the
    /// one place this differs from [`unlock_offer`](Self::unlock_offer), and it
    /// is forced: an unlock offer re-resolves only for the rare post whose
    /// `gated_tier` names an unlock tier, whereas **nothing in the feed-index
    /// projection says whether a post has tips**, so a `None`-on-empty resolver
    /// would re-ask for *every* untipped post on every snapshot notify, forever.
    /// [`TipView::default()`] (zero total, zero count) is a truthful "no tips",
    /// which makes the empty answer a real value rather than an absence.
    ///
    /// `None` therefore means only **not resolved yet**. A nest with no tip
    /// mechanism compiled in answers zero and a failed read
    /// errors; both fold to the same empty view, so "no tips yet" and
    /// "feature absent" render identically — the ratified degradation
    /// (`monetization.md` § Implementation status today, the Tips bullet).
    ///
    /// Stays `None` forever in a build with the `payments` feature excised,
    /// because the resolver is compiled out (`dynamic-features.md` § Charter
    /// members — tips are a buy-side gate surface); the field itself is ungated
    /// and inert, the same posture `fauna-protocol`'s `p2p` feature takes.
    ///
    // ⚠ The element ids ride a `payments`-GATED doc line, and every gated id in
    // this file does. This record is a `uniffi::Record`, so UniFFI embeds its
    // docstrings in the cdylib metadata and propagates them into the generated
    // Swift/Kotlin face: an ungated line naming `post-tip-*` ships the excised
    // plane's element ids in a store-safe artifact, and *documents* the UI the
    // build removed. Criterion 1 is "prose included" exactly as criterion 2
    // is. **Gate the line; never reword
    // it** — the kebab ids stay spelled verbatim here, so `rg post-tip-total`
    // still finds every driver. `just ffi-store-safe-check` pins both halves.
    #[cfg_attr(
        feature = "payments",
        doc = " Drives `post-tip-total` / `post-tip-count` / `post-tip-list-button`"
    )]
    #[cfg_attr(feature = "payments", doc = " (IDs user-approved 2026-08-11).")]
    pub tips: Option<TipView>,
}

impl PostSummary {
    /// The projection of a **legally taken-down** post — everything the nest
    /// will tell any viewer about it (`moderation.md` § Categories & enforcement
    /// item 1).
    ///
    /// Carries the id (so [`FeedSnapshot::find_post`] matches it) and the
    /// reference, and nothing else: there is no body, author, timestamp or
    /// envelope to project, and `verification` stays
    /// [`Unchecked`](VerificationStatus::Unchecked) because nothing was
    /// withheld-then-verified — the quoted-post embed omits its badge for the
    /// same reason.
    pub fn taken_down(post_id: impl Into<String>, reference: impl Into<String>) -> Self {
        PostSummary {
            post_id: post_id.into(),
            legal_takedown_ref: Some(reference.into()),
            ..Self::default()
        }
    }
}

/// One post's tip surface (`monetization.md` § Tips) — the display projection
/// of a `fauna_protocol::tips::TipsListReply`, minus its serde-`flatten` `extra` map
/// (which UniFFI records can't hold): the `UnlockOfferView` / `FeedSummaryView`
/// pattern.
///
/// ⚠ **This doc comment deliberately does not spell the wire kind, and no doc
/// comment on an ungated exported item may.** UniFFI embeds docstrings in the
/// library's metadata, so prose on a type like this one — ungated by design,
/// below — ships inside the *excised* artifact and in its generated
/// Swift/Kotlin face. `ffi-store-safe-check` witnesses criterion 2 by
/// `strings`-grepping that artifact and cannot tell prose from a sender, which
/// is why the rule is absence, not intent (`dynamic-features.md` § What
/// "completely compiled away" means, criterion 2). Naming the kind here turned
/// the gate red on 2026-08-11; name it in the *gated* resolver instead, where
/// `FeedManager::resolve_post_tips` already does.
///
/// **Deliberately mechanism-blind, exactly like the wire it projects.** Nothing
/// here mentions NIP-57, so a tip arriving over a future mechanism renders
/// through this same view with no app change — which is what lets the 7 app
/// display legs be written once (§ *One model, many mechanisms, two targets*).
///
/// **Deliberately absent, and load-bearing by their absence: no tier and no
/// validity window.** A tip grants nothing (§ Tips), so a field of either kind
/// appearing here would mean a purchase had been misrouted onto the tip
/// surface.
///
/// The type is **ungated** while its resolver is not: a `payments`-excised
/// build keeps the (permanently `None`) field and compiles out every way to
/// populate it, so there is no re-enable path (`dynamic-features.md` § What
/// "completely compiled away" means, criterion 5).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TipView {
    /// Summed millisatoshis over **every** tip on the post that reported an
    /// amount — not just the [`senders`](Self::senders) window, and not just
    /// the tips that carried a parseable amount.
    // `payments`-gated doc line — see the note on `PostSummary::tips`.
    #[cfg_attr(
        feature = "payments",
        doc = " Drives `post-tip-total`, which renders **iff this is non-zero**."
    )]
    pub total_msats: i64,
    /// How many tips the post has in total, including any that carried no
    /// amount — so this can exceed the number of tips contributing to
    /// [`total_msats`](Self::total_msats).
    ///
    /// **The two counters are separate elements precisely because this can be
    /// non-zero while the total is zero**: "3 people tipped" stays true when
    /// every receipt had an unparseable invoice, and the ratified rule is to
    /// render that honestly rather than coerce a missing amount to 0
    /// (`monetization.md` § Tips).
    // `payments`-gated doc line — see the note on `PostSummary::tips`.
    #[cfg_attr(
        feature = "payments",
        doc = " Drives `post-tip-count`, which renders iff this is non-zero."
    )]
    pub tip_count: i64,
    /// Newest-first attribution window — at most the nest's effective limit,
    /// and bounded by design: the totals above are what a card renders, and no
    /// display needs every tipper at once.
    // `payments`-gated doc line — see the note on `PostSummary::tips`.
    #[cfg_attr(
        feature = "payments",
        doc = " Drives `post-tip-item` rows inside `post-tip-list`."
    )]
    pub senders: Vec<TipSenderView>,
    /// Whether the post has more tips than [`senders`](Self::senders) carries,
    /// so an app can say "and N others" honestly instead of inferring it from a
    /// length comparison against a cap it would have to hard-code.
    pub has_more: bool,
}

/// One tip in the attribution window.
// `payments`-gated doc line — see the note on `PostSummary::tips`.
#[cfg_attr(feature = "payments", doc = " Rendered as a `post-tip-item` row.")]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TipSenderView {
    /// The tipper's hex actor id, when their mechanism identity resolved to an
    /// actor on this box. `None` covers both "the mechanism named no sender"
    /// and "the sender is not a local actor" — an outside tip still counts and
    /// still displays, unattributed.
    pub sender: Option<String>,
    /// The tipper's mechanism-native identifier when there is one (a Nostr
    /// pubkey today) — what an app shows for an unresolvable
    /// [`sender`](Self::sender) rather than showing nothing. Public data by
    /// construction: it is what the mechanism itself published.
    pub sender_ref: Option<String>,
    /// Millisatoshis, when the mechanism reported an amount. **`None` is a real
    /// state, not a parse failure** — a consumer that sums MUST skip it rather
    /// than coerce it to 0.
    pub amount_msats: Option<i64>,
    /// Which mechanism carried it (`"nostr_zap"` today). Present so a display
    /// can attribute the carrier; **nothing may branch on it for behavior** —
    /// the consequence class is fixed by being a tip.
    pub mechanism: String,
    /// Arrival on this box, seconds since epoch. The box's own observation,
    /// never a sender-controlled timestamp, so display ordering cannot be
    /// steered by whoever minted the payment.
    pub received_at: i64,
}

/// A sold post's public purchase fields, as read by a prospective buyer
/// (`monetization.md` § Per-post pay-to-unlock) — a clean UniFFI/WASM-safe
/// projection of `fauna_protocol::subscriptions::PostUnlockOffer` (which
/// carries a serde-`flatten` `extra` map UniFFI records can't hold), the
/// `FeedSummaryView` pattern.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct UnlockOfferView {
    /// The unlock tier's name (`post-unlock-<16 hex>`) — what
    /// `gated-post-buy-button` subscribes against.
    pub tier_name: String,
    pub price_hint: Option<String>,
    pub payment_url: Option<String>,
}

/// A subscribed bridge feed in the selector (`bridge-feed-unsubscribe-button`
/// rows) — the `FeedManager` projection of the wire
/// `fauna_protocol::bridges_ui::FeedSubscription` (dropping `created_at`, which
/// the UI doesn't render). A Bluesky / ActivityPub URI subscribed as a
/// synthesised feed; refreshed by [`crate::FeedManager::refresh_bridge_feeds`]
/// and mutated via subscribe/unsubscribe. Held separately from `feeds` because
/// the nest keeps bridge subscriptions in their own `bridge_feed_subscriptions`
/// table (`fauna.bridges.feeds.list`), distinct from `fauna.feed.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BridgeFeedView {
    /// Row id (the `bridge-feed-unsubscribe-button` target).
    pub id: i64,
    /// Bridge kind (`bluesky` / `activitypub` / …).
    pub bridge: String,
    /// The subscribed feed URI (e.g. an `at://…` Bluesky custom feed).
    pub feed_uri: String,
    /// Display name.
    pub name: String,
}

/// One option in the `bridge-form-bridge-select` selector: a bridge the nest's
/// *build* supports **and** is *runtime-available*, projected from
/// `fauna.bridges.list` by [`crate::FeedManager::refresh_available_bridges`].
/// Driving the selector from this set — never a hard-coded per-app protocol
/// list — is the capability *consumption* of
/// `docs/goal/architecture/version-compatibility.md` § Dimension 3: a client
/// never offers a protocol the nest can't serve (the nest's bridge list is
/// already cfg-gated by build feature *and* the per-bridge runtime `available`
/// flag, so this is the same gate the bridges page consumes). An empty set ⇒ the
/// nest supports no bridges ⇒ the client hides the subscribe form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AvailableBridge {
    /// Provider id — the value sent as `CreateFeedRequest.bridge`
    /// (`bluesky` / `activitypub` / `nostr` / …).
    pub id: String,
    /// Display label (`Bluesky`, `ActivityPub`, …).
    pub name: String,
}

/// The embedded quoted-post card the focal post renders (`feed.md` § Layout —
/// `post_detail`; § Post content types — "the quoted post inline with the
/// quoted author and a truncated body (280 character cap)"). Projected by the
/// shared [`crate::quote`] helper from the already-loaded post set (with a
/// `fauna.posts.get` fallback for a quote outside the loaded page) — the single
/// The snapshot projection of the wire `fauna_protocol::feed::AuthorDisplay`
/// (drops the wire `extra` map — UniFFI records can't carry serde-`flatten`),
/// the bridged author's face [`PostSummary::author_display`] carries.
/// Bridge-asserted, nest-relayed: the trust class of the post's `source`
/// badge, never a signed `Profile`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AuthorDisplayView {
    /// The bridge's own user-facing handle (`@user@host`, a NIP-05 address,
    /// `alice.bsky.social`).
    pub handle: Option<String>,
    pub display_name: Option<String>,
    /// Nest-relative and already proxied — fetched from the reader's own nest
    /// with the session bearer, like a `RenderBlock::ProxiedImage` path
    /// (render-model.md § D6c).
    pub avatar_url: Option<String>,
}

impl From<fauna_protocol::feed::AuthorDisplay> for AuthorDisplayView {
    fn from(a: fauna_protocol::feed::AuthorDisplay) -> Self {
        Self {
            handle: a.handle,
            display_name: a.display_name,
            avatar_url: a.avatar_url,
        }
    }
}

/// shared projection that retires the per-app quoted-post divergence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct QuotedPostView {
    /// Hex `[u8; 32]` post id of the quoted post.
    pub post_id: String,
    /// Hex `[u8; 32]` author of the quoted post.
    pub author: String,
    /// The quoted body, truncated to the 280-char cap.
    pub body: String,
    /// Whether **this client** cryptographically verified the *quoted* post's
    /// signed envelope (`security.md` § App display of unverified content).
    /// [`Verified`](VerificationStatus::Verified)/[`Failed`](VerificationStatus::Failed)
    /// only on the [`project_decoded`](crate::quote::project_decoded) fallback
    /// (which decodes a `fauna.posts.get` body); for a quote already in the loaded
    /// page ([`project_from_loaded`](crate::quote::project_from_loaded)) it inherits
    /// the source [`PostSummary::verification`] (usually
    /// [`Unchecked`](VerificationStatus::Unchecked) — the list card wasn't decoded).
    /// Drives the "unverified source" badge on the quoted-post embed.
    pub verification: VerificationStatus,
    /// Whether an external app authored the *quoted* post as its account — the
    /// [`PostSummary::authoring_origin`] twin, on exactly the same paths, driving
    /// the `delegated-origin-badge` scoped under the quoted-post embed (the
    /// `unverified-source-badge` precedent: a focal-card badge and a
    /// quoted-embed badge in one card are addressed separately).
    pub authoring_origin: AuthoringOriginStatus,
    /// `Some(reference)` when the quoted post has been **taken down under a legal
    /// obligation** (`moderation.md` § Categories & enforcement item 1). The nest
    /// withholds its body ([`PostGetReply::legal_takedown`](fauna_protocol::posts::PostGetReply)
    /// `= Some`, `body` empty), so `body`/`author` are empty here and the client
    /// renders the shared tombstone
    /// (`fauna_core::obligation::legal_takedown_tombstone(reference)`) in place of
    /// the quoted content — never a blank/broken embed. Set only on the
    /// [`crate::FeedManager::resolve_quoted_post`] `fauna.posts.get` fallback (a
    /// taken-down post is feed-excluded, so it is never in the loaded page); `None`
    /// for every normal quote. Folded through
    /// [`build_post_document`](crate::build_post_document) into
    /// [`RenderBlock::QuotedPost::legal_takedown_ref`](fauna_core::render::RenderBlock).
    pub legal_takedown_ref: Option<String>,
    /// `true` when the quoted post is **gone** — its author deleted it
    /// (`ui/feed.md` § Post deletion: a reference to a deleted post dangles by
    /// design and renders the not-found state). `body`/`author` are empty and the
    /// client paints `feed.post.post_not_found` in place of the quoted content.
    /// Projected by [`crate::quote::project_not_found`] — on the
    /// [`crate::FeedManager::resolve_quoted_post`] fallback when the nest answers
    /// `fauna.posts.not_found`, and by [`crate::FeedManager::delete_post`] for the
    /// post the user just deleted. Folded into
    /// [`RenderBlock::QuotedPost::not_found`](fauna_core::render::RenderBlock).
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub not_found: bool,
}

/// The Feed page's entire observable state. A cheap clone of the manager's
/// current state, re-read on every observer notification (`feed.md` §
/// Architectural rules #1: no client-side caching of post lists).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FeedSnapshot {
    /// The feeds the user has defined (selector list).
    pub feeds: Vec<FeedSummaryView>,
    /// The subscribed bridge feeds (`bridge-feed-unsubscribe-button` rows),
    /// from `fauna.bridges.feeds.list` — kept separate from `feeds` (a distinct
    /// nest table). Refreshed by `refresh_bridge_feeds`.
    pub bridge_feeds: Vec<BridgeFeedView>,
    /// The bridges the nest can actually serve — the `bridge-form-bridge-select`
    /// option set, from `fauna.bridges.list` (build+runtime gated). Refreshed by
    /// `refresh_available_bridges`. Empty ⇒ hide the subscribe form. Driving the
    /// selector from this set is the Dim 3 capability consumption
    /// (`version-compatibility.md`): no client offers an unsupported protocol.
    pub available_bridges: Vec<AvailableBridge>,
    /// The account's bridges roster — each consented third-party bridge's
    /// declared identity, projected by [`crate::bridge_roster`] from the same
    /// `fauna.bridges.list` reply `refresh_available_bridges` reads. What a
    /// paint hands [`crate::classify_sources`] so a bridged post's badge names
    /// its bridge (`ui/feed.md` § Implementation status today,
    /// `SourceKind::Bridged`). Additive.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    pub bridge_roster: Vec<fauna_core::source_glyph::BridgeIdentitySnapshot>,
    /// Selected feed id (hex); `None` ⇒ the nest's local feed
    /// (`fauna.feed.local.posts`) **unless** [`trending_selected`](Self::trending_selected)
    /// is set.
    pub selected_feed: Option<String>,
    /// Whether the built-in **Trending** virtual feed (`fauna.feed.trending.posts`)
    /// is the current selection (`trending.md` § The Trending feed) — the doc's
    /// "variant" of the selection, additive beside `selected_feed` rather than a
    /// type change, so every app stays wire/binding-compatible and adopts the
    /// `feed-trending-item` row on its own rollout slice (`linux → windows; web
    /// MAY omit; apple/android follow`). Set exclusively with `selected_feed`:
    /// `select_trending_feed()` sets this `true` + `selected_feed = None`, and
    /// `select_feed()` clears it — so `trending_selected && selected_feed.is_some()`
    /// is never representable. Drives the row's selected highlight, exactly as
    /// `selected_feed` drives a custom feed's.
    #[serde(default)]
    pub trending_selected: bool,
    /// The visible, ordered, deduplicated post list — never re-sorted by the
    /// client (`feed.md` § Anti-patterns; the nest supplies the order).
    pub posts: Vec<PostSummary>,
    /// A post addressed **by id from outside the timeline** — the deep-link
    /// target ([`crate::FeedManager::resolve_post`]; `ui/search.md` § Where logic
    /// lives → *Result navigation (deep link)*). A search hit, unlike every
    /// `post-card` click, can name a post the feed never loaded; that post is
    /// fetched by `fauna.posts.get`, projected, and held **here** rather than
    /// pushed into [`posts`](Self::posts), which is the paginated timeline —
    /// inserting into it would corrupt the load-more cursoring, show the post in
    /// the user's feed list, and lose it again on the next `reload` (which
    /// replaces the list wholesale).
    ///
    /// **Exactly one slot, deliberately**: a detail surface shows one post at a
    /// time, so there is no cache to grow and no eviction policy to get wrong.
    /// Re-opening the same post is free (the slot already holds it); alternating
    /// between two deep-linked posts costs one fetch each, which is the price of
    /// having no unbounded cache. `None` whenever the timeline already covers the
    /// post being shown.
    ///
    /// Read it through [`find_post`](Self::find_post) /
    /// [`rendered_posts`](Self::rendered_posts) — never by reaching for this
    /// field directly, so no app open-codes "timeline, then the slot".
    #[serde(default)]
    pub deep_linked_post: Option<PostSummary>,
    /// Active `feed-search-field` text (a re-query term, never a client-side
    /// filter — `feed.md` § Where logic lives).
    pub search_query: Option<String>,
    /// Drives empty/loading/error rendering.
    pub status: FeedStatus,
    /// A further page exists (the returned cursor is not yet exhausted).
    pub has_more: bool,
    pub compose: FeedComposeState,
    /// The local actor's own tiers — the `compose-gate-tier-select` option
    /// set (name + rank), from `fauna.subscriptions.tiers.list` via
    /// [`crate::FeedManager::refresh_own_tiers`]. Empty ⇒ the composer offers
    /// no gating (a reader-only account, or the fetch hasn't run).
    #[serde(default)]
    pub own_tiers: Vec<crate::compose::GateTierOption>,
    /// The rooms the local actor can address a post to — the composer's room
    /// options, beside `own_tiers`, via
    /// [`crate::FeedManager::refresh_own_rooms`]. Empty ⇒ no room is offered
    /// (no conversations plane installed, or the user sits on no room's floor).
    #[serde(default)]
    pub own_rooms: Vec<crate::compose::GateRoomOption>,
    pub bridge_form: BridgeFormState,
    /// Page-level error → `error-message`.
    pub error: Option<LocalizedText>,
}

impl FeedSnapshot {
    /// Every post this snapshot renders: the timeline list, then the
    /// [`deep_linked_post`](Self::deep_linked_post) slot. The **one** definition
    /// of that union — every render walk and every per-post lazy resolve reads it,
    /// so a deep-linked post gets its embeds, its badges and its unseal on exactly
    /// the same terms as a post the feed query delivered.
    ///
    /// Not the set for *ranking* questions (the scored window, exemplar picking,
    /// re-rank): those are about the ordered list and read
    /// [`posts`](Self::posts) directly — a post reached by deep link has no
    /// position in it.
    pub fn rendered_posts(&self) -> impl Iterator<Item = &PostSummary> {
        self.posts.iter().chain(self.deep_linked_post.iter())
    }

    /// The post `post_id` names, wherever the snapshot holds it — the timeline
    /// first, then the deep-link slot. The lookup every `post_detail` surface
    /// uses: `snapshot.posts.iter().find(…)` is the shape that made a search
    /// deep-link render a blank dialog, because a post the feed never loaded is
    /// not in that list.
    pub fn find_post(&self, post_id: &str) -> Option<&PostSummary> {
        self.rendered_posts().find(|p| p.post_id == post_id)
    }

    /// The empty state to paint, if any — the **one** place the empty-state
    /// decision is made (`feed.md` § Errors & edge cases), so no app open-codes
    /// `status == Loaded && posts.is_empty()` or picks the copy by its own
    /// reading of the search term. `None` while a read is in flight (the list
    /// is not yet authoritative), on an error (`error-message` explains it) and
    /// whenever posts are on screen.
    ///
    /// Keyed on [`posts`](Self::posts), not [`rendered_posts`](Self::rendered_posts):
    /// a deep-linked post is not part of the timeline, so it never fills it.
    pub fn empty_state(&self) -> Option<FeedEmptyState> {
        if self.status != FeedStatus::Loaded || !self.posts.is_empty() {
            return None;
        }
        Some(if self.search_query.is_some() {
            FeedEmptyState::NoMatches
        } else {
            FeedEmptyState::NoPosts
        })
    }
}

/// [`FeedSnapshot::empty_state`] for native (UniFFI) apps. A `uniffi::Record`
/// can't carry exported methods, so this free function is how Swift/Kotlin/C#
/// reach the one empty-state decision instead of re-deriving it from `status`,
/// `posts` and the search term (`feed.md` § Errors & edge cases).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn feed_empty_state(snapshot: FeedSnapshot) -> Option<FeedEmptyState> {
    snapshot.empty_state()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(status: FeedStatus, posts: usize, search: Option<&str>) -> FeedSnapshot {
        FeedSnapshot {
            status,
            posts: (0..posts)
                .map(|i| PostSummary {
                    post_id: format!("p{i}"),
                    ..Default::default()
                })
                .collect(),
            search_query: search.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn empty_state_is_no_posts_when_loaded_empty_without_a_search() {
        let s = snapshot(FeedStatus::Loaded, 0, None);
        assert_eq!(s.empty_state(), Some(FeedEmptyState::NoPosts));
    }

    #[test]
    fn empty_state_is_no_matches_when_a_search_matched_nothing() {
        let s = snapshot(FeedStatus::Loaded, 0, Some("needle"));
        assert_eq!(s.empty_state(), Some(FeedEmptyState::NoMatches));
    }

    #[test]
    fn empty_state_is_absent_while_loading_on_error_and_with_posts() {
        for search in [None, Some("needle")] {
            assert_eq!(snapshot(FeedStatus::Loading, 0, search).empty_state(), None);
            assert_eq!(snapshot(FeedStatus::Error, 0, search).empty_state(), None);
            assert_eq!(snapshot(FeedStatus::Loaded, 1, search).empty_state(), None);
        }
    }

    #[test]
    fn a_deep_linked_post_does_not_fill_the_timeline() {
        let mut s = snapshot(FeedStatus::Loaded, 0, None);
        s.deep_linked_post = Some(PostSummary::default());
        assert_eq!(s.empty_state(), Some(FeedEmptyState::NoPosts));
    }

    #[test]
    fn the_exported_free_fn_answers_as_the_method() {
        for (status, posts, search) in [
            (FeedStatus::Loaded, 0, None),
            (FeedStatus::Loaded, 0, Some("needle")),
            (FeedStatus::Loading, 0, None),
            (FeedStatus::Loaded, 1, None),
        ] {
            let s = snapshot(status, posts, search);
            assert_eq!(feed_empty_state(s.clone()), s.empty_state());
        }
    }
}
