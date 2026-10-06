//! e2e test-helper surface: build a [`FeedSnapshot`] from minimal post specs so a
//! cross-app test (`tests/e2e-unified/`) can inject a post list — notably one
//! whose [`PostSummary::verification`] is
//! [`Failed`](fauna_core::render::VerificationStatus::Failed) — without standing
//! up a real nest or hand-building a [`RenderDocument`](fauna_core::render::RenderDocument).
//!
//! A real signed post is only ever `Unchecked`/`Verified` (the manager flips it
//! to `Failed` only where this client's own envelope verification *fails*, which
//! a real-nest tier_3 path can't produce on demand — `security.md` § Client
//! display of unverified content), so this seam is the only way a tier_2 test can
//! exercise the unverified-source-badge render shipped on linux + web.
//!
//! Gated behind the `test-helpers` feature (the exact mirror of
//! `fauna_conversations`'s test seam); inert in production. The `document` is
//! built **here, in shared Rust**, from the spec `body` via the same
//! [`markdown_to_document`](fauna_core::render::markdown_to_document) path the
//! manager's `map_post` uses — so callers never construct a `RenderDocument`, and
//! the injected post paints exactly as a real feed-index projection would
//! (priority #2: the build lives in shared Rust, not per-app glue, and every
//! future feed e2e reuses this primitive rather than re-deriving a snapshot).

use fauna_core::render::{AuthoringOriginStatus, VerificationStatus};
use serde::{Deserialize, Serialize};

use crate::snapshot::{
    FeedSnapshot, FeedStatus, PostSummary, QuotedPostView, TipView, UnlockOfferView,
};

/// A minimal post spec for e2e feed injection. Only `post_id` / `author` are
/// load-bearing for the post-card; every other field has a serde default, so a
/// test passes just what it asserts on (`verification` for the
/// unverified-source-badge). The post's render `document` is derived from `body`,
/// never carried on the wire — callers never construct a `RenderDocument`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TestPostSpec {
    /// Hex `[u8; 32]` post id.
    pub post_id: String,
    /// Hex `[u8; 32]` author.
    pub author: String,
    /// A bridged author's face (`PostSummary::author_display`), absent for a
    /// native author.
    #[serde(default)]
    pub author_display: Option<crate::AuthorDisplayView>,
    /// List-card body markdown; rendered into the post's `document`.
    #[serde(default)]
    pub body: String,
    /// `"Unchecked"` (default) / `"Verified"` / `"Failed"` — serialized as the
    /// plain [`VerificationStatus`] variant string (the unit enum carries no
    /// serde attrs), matching the snapshot wire shape every app's badge reads.
    #[serde(default)]
    pub verification: VerificationStatus,
    /// `"Unknown"` (default) / `"Direct"` / `"Delegated"` — the D10 audit answer
    /// (`atproto-pds-full.md` § D10 → *Audit*). `"Delegated"` paints the
    /// `delegated-origin-badge`: an external app authored this post as the
    /// account. Serialized as the plain [`AuthoringOriginStatus`] variant string,
    /// matching the snapshot wire shape every app's badge reads.
    #[serde(default)]
    pub authoring_origin: AuthoringOriginStatus,
    /// Epoch-millis at the client boundary. Defaults to 0.
    #[serde(default)]
    pub timestamp: i64,
    /// Comma-separated protocol list → `classify_sources()` source badges.
    /// Defaults empty (no source badge).
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub has_media: bool,
    #[serde(default)]
    pub is_reply: bool,
    /// Interaction-bar counters (ratified 2026-06-27) — drive each app's
    /// `icon + count` bar (count hidden at 0). Default 0, so a test that doesn't
    /// assert on counts gets the empty bar; a Phase-2 client test sets them to
    /// exercise the non-zero render.
    #[serde(default)]
    pub like_count: i64,
    #[serde(default)]
    pub reply_count: i64,
    #[serde(default)]
    pub repost_count: i64,
    #[serde(default)]
    pub quote_count: i64,
    /// The viewer's own like-toggle state (`feed.md` § Interaction bar →
    /// Repost, which ratifies the per-viewer pair). Drives the lit
    /// `feed-like-button` on every app — tui's `state=on/off` attr, linux's
    /// `.liked` class, web's `.liked` + `aria-pressed` — so an injection can
    /// paint an already-liked post without a round trip. Default `false`.
    #[serde(default)]
    pub viewer_liked: bool,
    /// Set on a **repost row** — the id of the original this row reposts
    /// (`feed.md` § Interaction bar → Repost). Presence is the whole render
    /// switch: the card paints `repost-attribution`, renders **no** interaction
    /// bar, and activates at the ORIGINAL. Default `None` = an ordinary post.
    #[serde(default)]
    pub reposted_post_id: Option<String>,
    /// Gated-to-tier marker (`gated-post-badge`); default `None` = public.
    #[serde(default)]
    pub gated_tier: Option<String>,
    /// Hex channel id of the room a room-restricted post addresses; default
    /// `None`. The card names the room only when the snapshot's `own_rooms`
    /// lists it (`FeedManager::snapshot` derives `room_label` on every read),
    /// so an injection painting a member's card sets this AND seeds
    /// `own_rooms` with the same id.
    #[serde(default)]
    pub gated_room: Option<String>,
    /// Web-publish slug driving the ⋯ overflow's own-post web verbs; default
    /// `None` = not published to the web.
    #[serde(default)]
    pub web_slug: Option<String>,
    /// An optional **pre-folded** quoted-post embed (Slice 2b). When set, the
    /// post's `document` carries a `RenderBlock::QuotedPost` (built via the exact
    /// production [`build_post_document`](crate::manager::build_post_document)
    /// fold), and `quoted_post_id` is set to match, so the quoted-embed
    /// `unverified-source-badge` paints **iff** the *quoted* post's
    /// `verification == Failed`. A real `resolve_quoted_post` fold is unreachable
    /// in a tier_2 seam (it needs a nest fetch + envelope decode), and the
    /// fire-once resolve guard on every app (resolve only when the block is
    /// *not* yet folded) means the pre-folded block triggers no resolve attempt.
    #[serde(default)]
    pub quoted: Option<TestQuotedSpec>,
    /// **Pre-resolved** link previews (render-model.md § D4), one entry per bare url
    /// in the spec `body` — each entry's `url` must match a bare url there (so the
    /// producer emitted a `RenderBlock::LinkPreview` block to resolve); this projects
    /// those blocks to `PreviewState::Resolved` with the given title/description/image,
    /// so a tier_2 test paints the `link-preview-card`s without a real
    /// `fauna.linkpreview.resolve` round-trip (the exact mirror of `quoted`,
    /// pre-folding what a real resolve can't produce on demand in tier_2). The
    /// client's fire-once resolve guard sees a non-`Resolving` block and makes no
    /// resolve attempt.
    ///
    /// **A list, not an `Option`,** because `link-preview-card` is `indexed: true`
    /// (ui.yaml § `link_preview_card`, ruled 2026-08-13): a body with several
    /// standalone bare urls paints a card each, and a single-preview seam could not
    /// seed that shape at all — the reason the multi-card case went untested until the
    /// ruling. Default empty = no resolved preview, so a body's bare url stays
    /// `Resolving` and paints no card.
    #[serde(default)]
    pub link_previews: Vec<TestLinkPreviewSpec>,
    /// Per-category content-label verdicts (`content-label-badge`;
    /// `moderation.md` § Per-row badge data path). Default empty = no badge.
    #[serde(default)]
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// A pre-resolved buyer's price read (`monetization.md` § Per-post
    /// pay-to-unlock → *the buyer's price read is post-addressed*), the
    /// `quoted`/`link_preview` pre-fold pattern: a real
    /// `fauna.subscriptions.post_unlock.get` round-trip needs a live nest with
    /// a designated tier, unreachable in tier_2, so this projects the
    /// resolved state directly. Default `None` leaves
    /// [`PostSummary::unlock_offer`] unresolved, matching a fresh feed load —
    /// the client's fire-once resolve guard would then attempt the real read.
    #[serde(default)]
    pub unlock_offer: Option<UnlockOfferView>,
    /// A pre-resolved tip surface (`monetization.md` § Tips) — the same
    /// pre-fold pattern as `unlock_offer` above, and for the same reason: a
    /// real `fauna.tips.list` round-trip needs a live nest that has *ingested a
    /// believed payment receipt* (a designated signer, a materialized post, a
    /// crafted receipt), which no tier_2 seam can arrange.
    ///
    /// Default `None` leaves [`PostSummary::tips`] unresolved, matching a fresh
    /// feed load — the client's fire-once guard would then attempt the real
    /// read. Set it to paint the surface; note the two counters move
    /// independently on purpose, so `TipView { total_msats: 0, tip_count: 3,
    /// .. }` injects the ratified odd state where a post has real tips and no
    /// renderable amount.
    #[serde(default)]
    pub tips: Option<TipView>,
}

/// A pre-resolved link preview to project onto a [`TestPostSpec`]'s document
/// (render-model.md § D4). `url` must match the bare url in the spec `body` (so a
/// `LinkPreview` block exists to resolve); the card reads
/// title/description/image_hash off the projected `PreviewState::Resolved`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TestLinkPreviewSpec {
    /// The bare url — must equal the `LinkPreview` block's url (the body's bare url).
    pub url: String,
    /// `og:title` (possibly empty).
    #[serde(default)]
    pub title: String,
    /// `og:description` (possibly empty).
    #[serde(default)]
    pub description: String,
    /// Hex BLAKE3 hash of the og:image blob, or `None` for no preview image.
    #[serde(default)]
    pub image_hash: Option<String>,
    /// Whether the post's remote content is revealed (render-model.md § D4): the og:image
    /// is blocked-by-default like any `RemoteImage`, so it paints only when `true`. Defaults
    /// `false` (blocked) — the resolution default; set `true` to inject a revealed card.
    #[serde(default)]
    pub revealed: bool,
}

/// A quoted-post embed to fold into a [`TestPostSpec`]'s document (Slice 2b). The
/// quoted-embed badge reads `RenderBlock::QuotedPost::verification`, folded from
/// this spec's `verification`, so a tier_2 test sets `verification: "Failed"` to
/// exercise the badge's `Failed` arm on the quoted card.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TestQuotedSpec {
    /// Hex `[u8; 32]` id of the quoted post.
    pub post_id: String,
    /// Hex `[u8; 32]` author of the quoted post.
    pub author: String,
    /// The quoted body (already within the 280-char cap).
    #[serde(default)]
    pub body: String,
    /// The quoted post's verification — `"Failed"` paints the quoted-embed badge.
    #[serde(default)]
    pub verification: VerificationStatus,
    /// The quoted post's D10 audit answer — `"Delegated"` paints the
    /// `delegated-origin-badge` scoped under the quoted-post embed.
    #[serde(default)]
    pub authoring_origin: AuthoringOriginStatus,
    /// A legal-takedown reference on the QUOTED post (`moderation.md`
    /// § Categories & enforcement item 1). When set, the nest withheld the
    /// quoted envelope, so a client paints the shared tombstone in place of the
    /// (empty) body and omits the verification badge — there was no envelope to
    /// verify. Default `None` = an ordinary quote, so existing fixtures are
    /// unchanged.
    #[serde(default)]
    pub legal_takedown_ref: Option<String>,
    /// The QUOTED post is gone — deleted by its author (`ui/feed.md` § Post
    /// deletion). When set, a client paints the not-found state in place of the
    /// (empty) body. Default `false` = an ordinary quote.
    #[serde(default)]
    pub not_found: bool,
}

impl TestPostSpec {
    /// Project to a [`PostSummary`], building `document` from `body` (and any
    /// `quoted` embed) via the same `build_post_document` fold the manager's
    /// resolve path uses — so the injected post paints exactly as a real resolved
    /// feed post would (priority #2). With no `quoted`, the document is the plain
    /// `markdown_to_document(body)` and `quoted_post_id`/`media_hash` are `None`.
    pub fn into_summary(self) -> PostSummary {
        let quote = self.quoted.as_ref().map(|q| QuotedPostView {
            post_id: q.post_id.clone(),
            author: q.author.clone(),
            body: q.body.clone(),
            verification: q.verification,
            authoring_origin: q.authoring_origin,
            legal_takedown_ref: q.legal_takedown_ref.clone(),
            not_found: q.not_found,
        });
        let mut document = crate::manager::build_post_document(&self.body, quote.as_ref(), &[]);
        // Project any pre-resolved link preview onto the matching `LinkPreview`
        // block the producer emitted from the body's bare url — the tier_2 twin of
        // the manager's `snapshot()` resolved-preview projection (render-model.md § D4).
        for lp in &self.link_previews {
            use fauna_core::render::{PreviewState, RenderBlock};
            for block in &mut document.blocks {
                if let RenderBlock::LinkPreview { url, state } = block
                    && *url == lp.url
                {
                    *state = PreviewState::Resolved {
                        title: lp.title.clone(),
                        description: lp.description.clone(),
                        image_hash: lp.image_hash.clone(),
                        revealed: lp.revealed,
                    };
                }
            }
        }
        PostSummary {
            post_id: self.post_id,
            author: self.author,
            author_display: self.author_display,
            body: self.body,
            document,
            timestamp: self.timestamp,
            tags: self.tags,
            has_media: self.has_media,
            is_reply: self.is_reply,
            source: self.source,
            quoted_post_id: self.quoted.map(|q| q.post_id),
            media_hash: None,
            verification: self.verification,
            authoring_origin: self.authoring_origin,
            like_count: self.like_count,
            reply_count: self.reply_count,
            repost_count: self.repost_count,
            quote_count: self.quote_count,
            viewer_liked: self.viewer_liked,
            reposted_post_id: self.reposted_post_id,
            gated_tier: self.gated_tier,
            gated_room: self.gated_room,
            web_slug: self.web_slug,
            gated_unlocked: false,
            labels: self.labels,
            unlock_offer: self.unlock_offer,
            tips: self.tips,
            // No spec field: a taken-down post has no body/author/tags to
            // specify, so a test wanting one mints it with the single-purpose
            // `PostSummary::taken_down` rather than by hollowing out a spec.
            legal_takedown_ref: None,
            // Struct-update per the growing-wire-type fixture convention —
            // covers viewer_repost_id (no spec field yet; grow one the way
            // `viewer_liked` and `reposted_post_id` grew above, when an
            // injection needs to paint an already-reposted row).
            ..Default::default()
        }
    }
}

/// Build a `Loaded` [`FeedSnapshot`] carrying `specs` as the visible post list,
/// in the given order. The status is [`FeedStatus::Loaded`] (not the default
/// `Loading`) so the list actually paints — empty-state is `Loaded` with no
/// posts, never `Loading`. Every other field defaults (no feeds selected, no
/// error). Pair with [`FeedManager::set_feed_snapshot_for_test`](crate::FeedManager::set_feed_snapshot_for_test).
pub fn feed_snapshot_with_posts(specs: Vec<TestPostSpec>) -> FeedSnapshot {
    FeedSnapshot {
        status: FeedStatus::Loaded,
        posts: specs.into_iter().map(TestPostSpec::into_summary).collect(),
        ..FeedSnapshot::default()
    }
}

/// A [`TopicModel`](fauna_text_model::topic::TopicModel) trained hard enough
/// to clear the cold-start damp (30 samples for full confidence): cats good,
/// finance bad. Shared by `personalization`'s and `manager`'s test modules —
/// found byte-identical (bar the per-example key text, which no test reads)
/// in both by the containment arm of the dev-fleet near-duplicate-function
/// scanner; a same-crate pair, so `--cross-crate-only` runs never paired them
/// directly against each other, only each against unrelated cross-crate noise.
pub fn cats_model() -> fauna_text_model::topic::TopicModel {
    let mut m = fauna_text_model::topic::TopicModel::new();
    for i in 0..20 {
        m.train(
            &format!("cat{i}"),
            "a soft fluffy cat purring on a warm windowsill",
            fauna_text_model::topic::ExampleLabel::MoreLikeThis,
        );
        m.train(
            &format!("fin{i}"),
            "quarterly earnings guidance and shareholder dividends",
            fauna_text_model::topic::ExampleLabel::LessLikeThis,
        );
    }
    m
}
