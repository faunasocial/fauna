//! User-facing WS-RPC payload types for the feed surface — the feed-CRUD +
//! feed-query + discovery-contributor plane end-user clients call from the
//! feed-management + feed-render affordances. T2 of
//! the WS-RPC-everywhere migration (tracked internally; the sibling posts cluster
//! `fauna.posts.*` shipped in T1).
//!
//! This is a **behavior-preserving** transport migration of the existing
//! HTTP routes (`/api/v1/feeds`, `/api/v1/feeds/{id}`,
//! `/api/v1/feeds/{id}/posts`, `/api/v1/feeds/local/posts`,
//! `/api/v1/feeds/{id}/contributors`); the request/reply shapes mirror those
//! routes exactly. The handlers reuse the existing feed CRUD/query pipeline
//! (`feed_routes.rs` `*_core` fns) — no logic is duplicated.
//!
//! **Excluded from this slice (left 100% as-is):**
//! - `feed.query` (`POST /api/v1/feeds/query`, `remote_query_feed`) — that is
//!   **nest↔nest federation** (the only caller is `peer_query.rs` polling a
//!   remote nest), not a client path.
//!   (`context.get` / `GET /api/v1/context` — the engagement-derived interest
//!   profile — used to be listed here as a future migration. It was **never**
//!   migrated: the whole nest-side plaintext personalization path was deleted
//!   with the pre-frame behavioral surface, frame D9, 2026-07-12. See
//!   `docs/goal/behavior/engagement-cues.md` § Retirement.)
//!
//! Kind registry entries live in `kind.rs::register_feed_kinds`.
//!
//! Two cross-cutting wire rules (the dag-cbor wire forbids floats —
//! `decode_strict` rejects ALL floats; see `search.rs` `rank` and the
//! `dag-cbor-wire-forbids-floats` memo):
//!
//! 1. Feed `rules` ride TYPED — `rules: Vec<fauna_core::scoring::FilterRule>`,
//!    the externally-tagged serde form dag-cbor carries natively (`FilterRule`
//!    is float-free: per-mille `u16` confidences). The JSON-string
//!    `rules_json` field that preceded it was removed in place 2026-09-24
//!    (user-ruled under the compat-remnant sweep's fourth ratification —
//!    `version-compatibility.md` § Dimension 2, the rulings paragraph;
//!    `docs/goal/ui/feed.md` § Where logic lives: the typed form always wins
//!    over a JSON string inside the dag-cbor wire), each removal recorded in
//!    `schemas/ratified-breaks.txt`. A request still carrying `rules_json`
//!    and no `rules` is malformed.
//! 2. Post scores ride as scaled `i64` micro-units (×1e6) — same integer
//!    fixed-point convention `search.rs` `rank` uses, converted at the
//!    handler boundary (`(f64 * 1e6).round() as i64` and the inverse).

use fauna_core::scoring::FilterRule;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.feed.list ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedListReply {
    pub feeds: Vec<FeedSummary>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A feed in the list view. Omits `rules` — matches the `list_feeds` HTTP
/// shape (the full rules ride only on `fauna.feed.get`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedSummary {
    pub feed_id: String,
    /// Hex-encoded owner `[u8; 32]`.
    pub owner: String,
    pub name: String,
    pub combination: String,
    pub created_at: i64,
    pub scope: String,
    pub contributor_seeds: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── feed composition (frame § Composition, ratified 2026-07-06) ───────

/// One factor's term in a feed's composition — the wire mirror of
/// `fauna_core::scoring::CompositionEntry` (which owns the semantics: the
/// composed ordering key is `Σ (weight_permille · factor_value) / 1000`;
/// factor keys are opaque bus keys, scope is by container). Typed on the
/// wire — no JSON-string dodge needed: both fields are float-free by
/// construction, so the dag-cbor float blocker that forced `rules_json`
/// never applied.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedCompositionEntry {
    /// Opaque factor key on the scoring-metadata bus (`content_scores.factor`
    /// / `"engagement"`).
    pub factor: String,
    /// Signed per-mille weight (1000 = 1.0×).
    pub weight_permille: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.create ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedCreateRequest {
    pub name: String,
    /// The feed's filter rules, typed on the wire (module doc, rule 1). The
    /// nest validates and stores them as canonical dag-cbor.
    pub rules: Vec<FilterRule>,
    pub combination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributor_seeds: Option<Vec<String>>,
    /// The feed's composition (frame § Composition). Additive:
    /// `None` → no composition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<Vec<FeedCompositionEntry>>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedCreateReply {
    /// The generated `feed_id` (16-byte hex). The HTTP twin returned it as
    /// `{"feed_id": ...}` (201).
    pub feed_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.get ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedGetRequest {
    pub feed_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedGetReply {
    pub feed_id: String,
    /// Hex-encoded owner `[u8; 32]`.
    pub owner: String,
    pub name: String,
    /// The feed's stored filter rules — see `FeedCreateRequest::rules`.
    pub rules: Vec<FilterRule>,
    pub combination: String,
    pub created_at: i64,
    pub scope: String,
    pub contributor_seeds: Vec<String>,
    /// The feed's composition (frame § Composition). `None` = the feed has
    /// none (`order=score` reads the single engagement score).
    /// Additive: absent when the feed has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<Vec<FeedCompositionEntry>>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.update ──────────────────────────────────────────────────

/// Mirrors the HTTP `update_feed` — only `name` / `rules` / `combination`
/// are mutated (the HTTP twin shared `CreateFeedRequest` but ignored
/// scope/seeds on update), so this request omits scope/seeds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedUpdateRequest {
    pub feed_id: String,
    pub name: String,
    /// Replaces the stored rules — see `FeedCreateRequest::rules`.
    pub rules: Vec<FilterRule>,
    pub combination: String,
    /// The feed's composition (frame § Composition). **`None` = leave the
    /// stored composition unchanged** — an update editing only a feed's
    /// name/rules must not silently destroy a composition it does not send;
    /// `Some([])` = explicitly clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<Vec<FeedCompositionEntry>>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Empty success — the HTTP twin returned `204 No Content`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedUpdateReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.delete ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedDeleteRequest {
    pub feed_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Empty success — the HTTP twin returned `204 No Content`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedDeleteReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── shared post item ───────────────────────────────────────────────────

/// A **bridged** author's face — the handle, display name and avatar the
/// origin bridge served for a synthetic actor (`docs/goal/behavior/bridges.md`
/// § Unified feed ingestion → *Bridged authors*, ruled 2026-09-26). Projected
/// by the nest from its `bridge_authors` table onto every local feed page
/// after the query, never joined into it; a native author has no row and the
/// carrying field stays `None`.
///
/// **Trust class: bridge-asserted, nest-relayed** — the same class as the
/// post's `source` badge, never a signed `Profile`. Apps hand `display_name`
/// and `handle` to `fauna_core::format::peer_display_label` in the
/// self-published-name slot, so the viewer's own nickname still wins.
/// `avatar_url` is nest-relative and already rewritten through the bridge's
/// privacy proxy (fetched exactly like a `MediaItem.remote_url`); every field
/// is optional because no bridge guarantees all three.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AuthorDisplay {
    /// The bridge's own user-facing handle: `@user@host` (ActivityPub), a
    /// NIP-05 address or the kind-0 `name` (nostr), `alice.bsky.social`
    /// (Bluesky).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A single post in a feed-query result. Mirrors the per-post JSON the HTTP
/// twins emitted (`query_feed` / `query_local_feed`).
///
/// `Default` exists for struct-update fixtures (`..Default::default()`) so two
/// branches independently growing this wire type merge cleanly instead of
/// colliding on hand-listed fields (the growing-wire-type fixture convention).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedPostItem {
    /// Hex-encoded post id `[u8; 32]`.
    pub post_id: String,
    /// Hex-encoded author `[u8; 32]`.
    pub author: String,
    pub body: String,
    /// Epoch **microseconds** — the `content.created_at` column's own
    /// invariant (`db/schema.rs` `SCHEMA_CONTENT`: "epoch MICROSECONDS, for
    /// every writer without exception"). Every client-side projection of this
    /// field divides by 1000 before treating it as millis
    /// (`fauna_feed::PostSummary::timestamp`, `libs/fauna-client-search`'s
    /// `nest_row`) — this struct itself carried no unit doc, the exact gap
    /// class `value-formatting.md`'s adoption-claim sweeps warn decays
    /// silently.
    pub created_at: i64,
    pub tags: Vec<String>,
    pub has_media: bool,
    pub is_reply: bool,
    pub source: String,
    /// Interaction-bar counters projected from `content_meta` (ratified
    /// 2026-06-27, `docs/goal/ui/feed.md` § Interaction bar) — drive the
    /// icon+count bar all seven apps render (count hidden at 0). Each is
    /// additive and `#[serde(default)]`-tagged, so an absent counter
    /// deserializes 0 (identical to a post with no activity).
    #[serde(default)]
    pub like_count: i64,
    #[serde(default)]
    pub reply_count: i64,
    #[serde(default)]
    pub repost_count: i64,
    #[serde(default)]
    pub quote_count: i64,
    /// Composite score in fixed-point micro-units (×1e6); integer because
    /// the dag-cbor wire forbids floats — see `search.rs` `rank`. Populated
    /// only by the `order=score` branch of `fauna.feed.posts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    /// Hex-encoded `[u8; 32]` post id this post *quotes* — the
    /// `Reference::Quote` target from `Post.references`
    /// (`libs/fauna-core::data::Reference`), drives the embedded quoted-post
    /// card the apps render in the list card and `post_detail`
    /// (`docs/goal/ui/feed.md` § State & data shape, ratified 2026-06-14). The
    /// nest projects it without reading `content.payload` (the segment-store
    /// load-bearing decision, `feed.md` § The read model). Additive +
    /// `None`-default (the post is not a quote).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted_post_id: Option<String>,
    /// Hex-encoded `[u8; 32]` post id this post *reposts* — the
    /// `Reference::Repost` target, projected from the `content_links
    /// link_type='repost'` row exactly as `quoted_post_id` is from its `'quote'`
    /// twin (`docs/goal/ui/feed.md` § Interaction bar → Repost, ratified
    /// 2026-08-10). `Some` marks the row a REPOST ROW: the apps render
    /// attribution + the embedded original (the same `quoted-post` embed), no
    /// own interaction bar, and card activation opens the original's detail.
    /// Additive + `None`-default, wire-compatible both directions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reposted_post_id: Option<String>,
    /// Hex-encoded post id of the **connection actor's own live repost post**
    /// naming this row's post — presence means "reposted by me", and the value
    /// is exactly what `fauna.posts.interact` action `unrepost` takes (the
    /// caller's own repost post's id), which is what makes `unrepost` reachable
    /// from a client at all. Projected per viewer from the actor-keyed
    /// `content_links 'repost'` row; `None` on bridged rows (their interactions
    /// live in the origin protocol). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_repost_id: Option<String>,
    /// Whether the **connection actor** has this post liked — the
    /// `engagement_events` like-toggle row (`compute_toggle_event_id`), the
    /// same state `like`/`unlike` maintain. Drives the like button's toggle
    /// state (per-app consumption is a follow-on; carrier ratified 2026-08-10).
    /// Additive + `false`-default: an absent key reads as un-liked.
    #[serde(default)]
    pub viewer_liked: bool,
    /// Tier name of a gated-to-tier post (`Post.gated.tier` — a
    /// plaintext-floor attribute, `docs/goal/ui/feed.md` § Encryption at
    /// rest), projected from `content_meta.gated_tier` so the list card can
    /// render the `gated-post-badge` without decoding the post body. `None` =
    /// public post. Additive + `None`-default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gated_tier: Option<String>,
    /// Hex-encoded 32-byte channel id of the room a **room-restricted** post
    /// addresses (`KeyAccess::Room.group_id` — the same plaintext floor as
    /// the tier; `docs/goal/ui/feed.md` § Encryption at rest → *Room-restricted
    /// — the app half*, the card bullet), projected from
    /// `content_meta.gated_room` so a member's list card can name the room by
    /// their own label without decoding the post body. `None` = not a room
    /// post (the card then paints the reserved tier, ruling 3's honest
    /// degrade). Additive + `None`-default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gated_room: Option<String>,
    /// The post's web-publish slug — the `content_links` `link_type='web_published'`
    /// row's slug, projected without touching `content.payload` exactly like
    /// `quoted_post_id`. `None` = not published to the web. The **publish-state twin of
    /// `gated_tier`**: together the two derive which own-post web verbs the
    /// `feed-post-actions-menu` ⋯ overflow offers — publish vs. unpublish, and
    /// whether *Copy paywall link* applies (published **and** gated only).
    /// Additive + `None`-default, wire-compatible both directions
    /// (`docs/goal/ui/feed.md` § User actions; surface owner
    /// `docs/goal/behavior/web-content-hosting.md` § Published-post management).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_slug: Option<String>,
    /// Per-row content-label verdicts (`moderation.md` § Per-row badge data
    /// path, ratified 2026-07-16), projected from `content_labels` — one entry
    /// per category, the highest-confidence row. Additive + empty-default, so
    /// a post with no labels deserializes an empty `Vec` (no badge).
    #[serde(default)]
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// The face of a **bridged** author — see [`AuthorDisplay`]. `None` for a
    /// native author (the card then paints the short id). Additive +
    /// `None`-default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_display: Option<AuthorDisplay>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.posts ───────────────────────────────────────────────────

/// The `{feed_id}/posts` query. When `order == "score"`, results are ordered
/// by score descending and paginated via the `(score_cursor,
/// score_cursor_created_at)` keyset pair; otherwise chronological, paginated
/// via `cursor` (epoch micros).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedPostsRequest {
    pub feed_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    /// Score-based cursor in fixed-point micro-units (×1e6) — see
    /// `FeedPostItem::score`. Carries the *key* half of the keyset cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_cursor: Option<i64>,
    /// The **tiebreak half** of the score-order keyset cursor: the `created_at`
    /// (epoch micros) of the very row `score_cursor` was read from. Sent with
    /// `score_cursor` or not at all: a nest refuses either half alone
    /// (`invalid_params`) — the pre-keyset key-only shape left the wire with the
    /// compat-remnant sweep.
    ///
    /// It exists because the scored sort is compound (`key DESC, created_at
    /// DESC`) while the cursor was not: a key-only `key < cursor` predicate
    /// drops every row sharing the boundary key, and stalls outright when the
    /// key is flat — the ordinary case for a composition of only *sealed*
    /// tier-1 factors, whose nest-side terms are all 0
    /// (`topic-factors.md` § Scoring).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_cursor_created_at: Option<i64>,
    /// Optional comma-separated full-text search terms (appended as a
    /// `BodyContains` filter to the feed's rules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedPostsReply {
    pub posts: Vec<FeedPostItem>,
    /// Chronological cursor (epoch micros) — set in the default order branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    /// Score cursor in micro-units — set in the `order=score` branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_cursor: Option<i64>,
    /// The tiebreak half of the keyset cursor (see
    /// [`FeedPostsRequest::score_cursor_created_at`]) — the `created_at` of the
    /// same row `score_cursor` came from. Both halves are echoed back on the
    /// next request — both or neither; a half cursor is refused as
    /// `invalid_params`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_cursor_created_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.local.posts ─────────────────────────────────────────────

/// The local-feed query (`/api/v1/feeds/local/posts`). `query_local_feed`
/// only reads `cursor` / `limit` / `search` — always chronological.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedLocalPostsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedLocalPostsReply {
    pub posts: Vec<FeedPostItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.trending.posts ──────────────────────────────────────────

/// The built-in **Trending** virtual-feed query (`fauna.feed.trending.posts`):
/// the *scored* sibling of `fauna.feed.local.posts`. No feed row, works out of
/// the box with zero configuration (`trending.md` § The Trending feed). Always
/// score-ordered — the nest applies the implicit composition
/// `[(trending, 1000)]` plus the caller's global factor set over **public
/// posts only** — so it carries no `order` / chronological `cursor` (unlike
/// `fauna.feed.posts`); pagination is the same `(score_cursor,
/// score_cursor_created_at)` keyset pair as the `order=score` branch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedTrendingPostsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Score-based cursor in fixed-point micro-units (×1e6) — the *key* half of
    /// the keyset cursor. See [`FeedPostsRequest::score_cursor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_cursor: Option<i64>,
    /// The **tiebreak half** of the keyset cursor — the `created_at` (epoch
    /// micros) of the row `score_cursor` was read from. See
    /// [`FeedPostsRequest::score_cursor_created_at`] — both halves or neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_cursor_created_at: Option<i64>,
    /// Optional comma-separated full-text search terms (appended as a
    /// `BodyContains` filter over the trending post set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedTrendingPostsReply {
    pub posts: Vec<FeedPostItem>,
    /// Score cursor in micro-units (the trending read is always score-ordered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_cursor: Option<i64>,
    /// The tiebreak half of the keyset cursor (see
    /// [`FeedTrendingPostsRequest::score_cursor_created_at`]) — the `created_at`
    /// of the same row `score_cursor` came from. Both halves are echoed back on
    /// the next request. `has_more` is derived client-side from whether this
    /// cursor is present (a page shorter than `limit` ends the feed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_cursor_created_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.factors.{get,set} — the user's GLOBAL factor set ────────

/// `fauna.feed.factors.get` — read the caller's global factor set: the
/// per-user `(factor, weight)` entries folded into **every** one of their
/// feeds' composed `order=score` orderings (frame § Composition — scope is
/// by container: feed-level entries live on the feed definition, global
/// entries live here). Transparent factor preferences only — sealed tier-1
/// factor data (e.g. the muted-keyword list) stays sealed client-side and
/// never appears here.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedFactorsGetRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedFactorsGetReply {
    /// The caller's global factor set (empty = none).
    pub factors: Vec<FeedCompositionEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.feed.factors.set` — replace the caller's global factor set
/// (idempotent whole-set overwrite, the whole-record latest-wins shape;
/// an empty `factors` clears it).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedFactorsSetRequest {
    pub factors: Vec<FeedCompositionEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FeedFactorsSetReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.contributors.list ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedContributorsListRequest {
    pub feed_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedContributorsListReply {
    pub contributors: Vec<FeedContributor>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A discovery-feed contributor row. Mirrors the per-row JSON
/// `list_contributors_handler` emitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedContributor {
    pub nest_url: String,
    /// Hex-encoded author id `[u8; 32]`, when this contributor is scoped to a
    /// specific author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    pub hit_count: i64,
    pub last_seen: i64,
    pub poll_priority: String,
    pub discovered_via: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.contributors.grant ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedContributorGrantRequest {
    pub feed_id: String,
    pub nest_url: String,
    /// Hex-encoded author id `[u8; 32]`, optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `added=true` → newly added (HTTP twin's `201 {added:true}`);
/// `added=false` with `reason` → already existed (HTTP twin's
/// `200 {added:false,reason}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedContributorGrantReply {
    pub added: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.feed.contributors.revoke ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedContributorRevokeRequest {
    pub feed_id: String,
    pub nest_url: String,
    /// Hex-encoded author id `[u8; 32]`, optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Empty success — the HTTP twin returned `204 No Content`. A missing
/// contributor surfaces as a `fauna.feed.not_found` `RpcError` (the twin's
/// `404`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedContributorRevokeReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    // ── list ───────────────────────────────────────────────────────────

    fn sample_list_reply() -> FeedListReply {
        FeedListReply {
            feeds: vec![
                FeedSummary {
                    feed_id: "feed-1".into(),
                    owner: "ab".repeat(32),
                    name: "Home".into(),
                    combination: "all".into(),
                    created_at: 1_700_000_000,
                    scope: "local".into(),
                    contributor_seeds: vec![],
                    extra: BTreeMap::new(),
                },
                FeedSummary {
                    feed_id: "feed-2".into(),
                    owner: "cd".repeat(32),
                    name: "Discovery".into(),
                    combination: "any".into(),
                    created_at: 1_700_000_100,
                    scope: "discovery".into(),
                    contributor_seeds: vec!["https://example.org".into()],
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn feed_list_request_round_trips() {
        let req = FeedListRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedListRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_list_reply_round_trips() {
        let reply = sample_list_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedListReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn feed_list_reply_canonical_re_encodes_identically() {
        let reply = sample_list_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: FeedListReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    // ── create ─────────────────────────────────────────────────────────

    fn sample_composition() -> Vec<FeedCompositionEntry> {
        vec![
            FeedCompositionEntry {
                factor: "engagement".into(),
                weight_permille: 1000,
                extra: BTreeMap::new(),
            },
            FeedCompositionEntry {
                factor: format!("labeler:{}", "ab".repeat(32)),
                weight_permille: -1000,
                extra: BTreeMap::new(),
            },
        ]
    }

    fn sample_create_request() -> FeedCreateRequest {
        FeedCreateRequest {
            name: "My Feed".into(),
            rules: vec![
                FilterRule::HasHashtag {
                    tags: vec!["rust".into()],
                },
                FilterRule::AuthorInSet { actors: [7u8; 32] },
            ],
            combination: "all".into(),
            scope: Some("discovery".into()),
            contributor_seeds: Some(vec!["https://peer.example".into()]),
            composition: Some(sample_composition()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn feed_create_request_round_trips() {
        let req = sample_create_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedCreateRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_create_request_canonical_re_encodes_identically() {
        let req = sample_create_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: FeedCreateRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn feed_create_request_minimal_omits_optionals() {
        let req = FeedCreateRequest {
            name: "Minimal".into(),
            rules: vec![],
            combination: "all".into(),
            scope: None,
            contributor_seeds: None,
            composition: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedCreateRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert!(decoded.scope.is_none());
        assert!(decoded.contributor_seeds.is_none());
        // A payload that never carried `composition` decodes to `None` (the
        // field is omitted from these bytes, not encoded as null).
        assert!(decoded.composition.is_none());
        assert!(decoded.extra.is_empty());
    }

    /// The rules ride typed: each `FilterRule` is an externally-tagged
    /// dag-cbor map (`{"HasHashtag": {"tags": [...]}}`), not a JSON string.
    #[test]
    fn feed_create_request_rules_ride_as_typed_dag_cbor() {
        let bytes = encode_canonical(&sample_create_request()).unwrap();
        let value: Value = decode(&bytes).unwrap();
        let Value::Map(map) = value else {
            panic!("request must encode as a map");
        };
        let Some(Value::List(rules)) = map.get("rules") else {
            panic!(
                "`rules` must be a dag-cbor list, got {:?}",
                map.get("rules")
            );
        };
        let Value::Map(first) = &rules[0] else {
            panic!("a rule must be an externally-tagged map");
        };
        assert!(first.contains_key("HasHashtag"));
        assert!(!map.contains_key("rules_json"));
    }

    /// Removed in place 2026-09-24 (`schemas/ratified-breaks.txt`): a payload
    /// carrying only the retired `rules_json` string is malformed — `rules`
    /// is required and the string never stands in for it.
    #[test]
    fn feed_create_request_with_only_rules_json_is_malformed() {
        let mut map = BTreeMap::new();
        map.insert("name".to_string(), Value::String("Legacy".into()));
        map.insert("rules_json".to_string(), Value::String("[]".into()));
        map.insert("combination".to_string(), Value::String("all".into()));
        let bytes = encode_canonical(&Value::Map(map)).unwrap();
        assert!(decode::<FeedCreateRequest>(&bytes).is_err());
    }

    #[test]
    fn feed_create_reply_round_trips() {
        let reply = FeedCreateReply {
            feed_id: "0011223344556677".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedCreateReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    // ── get ────────────────────────────────────────────────────────────

    fn sample_get_reply() -> FeedGetReply {
        FeedGetReply {
            feed_id: "0011223344556677".into(),
            owner: "ef".repeat(32),
            name: "My Feed".into(),
            rules: vec![FilterRule::LabelBelow {
                category: "spam".into(),
                max_confidence_permille: 500,
            }],
            combination: "all".into(),
            created_at: 1_700_000_000,
            scope: "local".into(),
            contributor_seeds: vec![],
            composition: Some(sample_composition()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn feed_get_request_round_trips() {
        let req = FeedGetRequest {
            feed_id: "abc".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedGetRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_get_reply_round_trips() {
        let reply = sample_get_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedGetReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn feed_get_reply_canonical_re_encodes_identically() {
        let reply = sample_get_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: FeedGetReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    // ── update / delete ────────────────────────────────────────────────

    #[test]
    fn feed_update_request_round_trips() {
        let req = FeedUpdateRequest {
            feed_id: "0011223344556677".into(),
            name: "Renamed".into(),
            rules: vec![],
            combination: "any".into(),
            composition: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedUpdateRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        // `None` (no touch) is omitted from the wire and
        // decodes back to `None` — distinct from `Some([])` (explicit clear).
        assert!(decoded.composition.is_none());
    }

    #[test]
    fn feed_update_request_composition_clear_vs_absent() {
        // `Some([])` — the explicit clear — must survive the round trip as
        // an *empty list*, never collapse to `None` (leave-unchanged).
        let req = FeedUpdateRequest {
            feed_id: "0011223344556677".into(),
            name: "Renamed".into(),
            rules: vec![],
            combination: "any".into(),
            composition: Some(vec![]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedUpdateRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.composition, Some(vec![]));
    }

    #[test]
    fn feed_factors_get_set_round_trip() {
        let get_reply = FeedFactorsGetReply {
            factors: sample_composition(),
            extra: BTreeMap::new(),
        };
        let decoded: FeedFactorsGetReply = decode(&encode_canonical(&get_reply).unwrap()).unwrap();
        assert_eq!(get_reply, decoded);

        let set_req = FeedFactorsSetRequest {
            factors: vec![],
            extra: BTreeMap::new(),
        };
        let decoded: FeedFactorsSetRequest = decode(&encode_canonical(&set_req).unwrap()).unwrap();
        assert_eq!(set_req, decoded);
        assert!(decoded.factors.is_empty(), "empty set = clear");
    }

    #[test]
    fn feed_composition_entry_round_trips_and_re_encodes_identically() {
        let entries = sample_composition();
        let bytes1 = encode_canonical(&entries).unwrap();
        let decoded: Vec<FeedCompositionEntry> = decode(&bytes1).unwrap();
        assert_eq!(entries, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn feed_update_reply_round_trips() {
        let reply = FeedUpdateReply {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedUpdateReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn feed_delete_request_round_trips() {
        let req = FeedDeleteRequest {
            feed_id: "0011223344556677".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedDeleteRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_delete_reply_round_trips() {
        let reply = FeedDeleteReply {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedDeleteReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    // ── posts / local.posts ────────────────────────────────────────────

    fn sample_post_item(with_score: bool) -> FeedPostItem {
        FeedPostItem {
            post_id: "11".repeat(32),
            author: "22".repeat(32),
            body: "hello feed".into(),
            created_at: 1_700_000_000_000_000,
            tags: vec!["rust".into(), "fauna".into()],
            has_media: true,
            is_reply: false,
            source: "fauna".into(),
            like_count: 3,
            reply_count: 2,
            repost_count: 1,
            quote_count: 4,
            score: if with_score { Some(12_500_000) } else { None },
            // Vary with `with_score` so the shared round-trip / canonical
            // re-encode tests cover both the present and omitted cases
            // (gated_tier rides the same toggle).
            quoted_post_id: if with_score {
                Some("99".repeat(32))
            } else {
                None
            },
            gated_tier: if with_score {
                Some("gold".into())
            } else {
                None
            },
            gated_room: if with_score {
                Some("c7".repeat(32))
            } else {
                None
            },
            web_slug: if with_score {
                Some("hello-world".into())
            } else {
                None
            },
            author_display: if with_score {
                Some(AuthorDisplay {
                    handle: Some("@bob@remote.example".into()),
                    display_name: Some("Bob".into()),
                    avatar_url: Some(
                        "/api/v1/media/proxy?url=https%3A%2F%2Fr.example%2Fa.png".into(),
                    ),
                    extra: BTreeMap::new(),
                })
            } else {
                None
            },
            ..Default::default()
        }
    }

    /// An item — the map with no `author_display` key at all —
    /// decodes with the field `None`, and an item whose face carries only a
    /// handle round-trips without inventing the absent fields (the
    /// bidirectional compat the additive field promises).
    #[test]
    fn author_display_is_additive_and_partial() {
        let mut item = sample_post_item(false);
        assert!(item.author_display.is_none());
        let bytes = encode_canonical(&item).unwrap();
        let decoded: FeedPostItem = decode(&bytes).unwrap();
        assert!(decoded.author_display.is_none());

        item.author_display = Some(AuthorDisplay {
            handle: Some("alice.bsky.social".into()),
            ..Default::default()
        });
        let bytes = encode_canonical(&item).unwrap();
        let decoded: FeedPostItem = decode(&bytes).unwrap();
        assert_eq!(decoded, item);
        let face = decoded.author_display.unwrap();
        assert_eq!(face.handle.as_deref(), Some("alice.bsky.social"));
        assert!(face.display_name.is_none() && face.avatar_url.is_none());
    }

    #[test]
    fn feed_posts_request_round_trips() {
        let req = FeedPostsRequest {
            feed_id: "0011223344556677".into(),
            cursor: None,
            limit: Some(50),
            order: Some("score".into()),
            score_cursor: Some(3_000_000),
            score_cursor_created_at: Some(1_700_000_000_000_000),
            search: Some("rust,fauna".into()),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedPostsRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_posts_request_minimal_omits_optionals() {
        let req = FeedPostsRequest {
            feed_id: "0011223344556677".into(),
            cursor: None,
            limit: None,
            order: None,
            score_cursor: None,
            score_cursor_created_at: None,
            search: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedPostsRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_posts_reply_round_trips() {
        let reply = FeedPostsReply {
            posts: vec![sample_post_item(true), sample_post_item(false)],
            cursor: None,
            score_cursor: Some(3_000_000),
            score_cursor_created_at: Some(1_700_000_000_000_000),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedPostsReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn feed_posts_reply_canonical_re_encodes_identically() {
        let reply = FeedPostsReply {
            posts: vec![sample_post_item(true)],
            cursor: None,
            score_cursor: Some(3_000_000),
            score_cursor_created_at: Some(1_700_000_000_000_000),
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: FeedPostsReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn feed_post_item_quoted_post_id_round_trips_and_is_forward_compatible() {
        // Present: a `Some` quoted_post_id survives the canonical round-trip.
        let quoting = sample_post_item(true);
        assert_eq!(quoting.quoted_post_id, Some("99".repeat(32)));
        let decoded: FeedPostItem = decode(&encode_canonical(&quoting).unwrap()).unwrap();
        assert_eq!(decoded.quoted_post_id, Some("99".repeat(32)));

        // Absent: a `None` is omitted from the wire (skip_serializing_if), and
        // a payload that never carried the field decodes to `None` (the post
        // is not a quote).
        let plain = sample_post_item(false);
        assert_eq!(plain.quoted_post_id, None);
        let bytes = encode_canonical(&plain).unwrap();
        let decoded: FeedPostItem = decode(&bytes).unwrap();
        assert_eq!(decoded.quoted_post_id, None);
        // The field defaults in when an old encoder omits it: drop it from the
        // map form and confirm it still decodes (extra-map carries nothing).
        assert!(decoded.extra.is_empty());
    }

    #[test]
    fn feed_post_item_web_slug_is_the_publish_state_projection() {
        // Present: the `content_links` web-publish slug — the publish-state
        // twin of `gated_tier` (`ui/feed.md` § User actions). Its presence is
        // what the ⋯-menu derives publish/unpublish/copy-link from.
        let published = sample_post_item(true);
        assert_eq!(published.web_slug.as_deref(), Some("hello-world"));
        let decoded: FeedPostItem = decode(&encode_canonical(&published).unwrap()).unwrap();
        assert_eq!(decoded.web_slug.as_deref(), Some("hello-world"));

        // Absent = unpublished: omitted from the wire, and decodes to `None`
        // rather than failing.
        let unpublished = sample_post_item(false);
        assert_eq!(unpublished.web_slug, None);
        let bytes = encode_canonical(&unpublished).unwrap();
        let decoded: FeedPostItem = decode(&bytes).unwrap();
        assert_eq!(decoded.web_slug, None);
        assert!(decoded.extra.is_empty());

        // The key is genuinely absent from an unpublished post's encoding —
        // `None` must not travel as an explicit null.
        let as_map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert!(
            !as_map.contains_key("web_slug"),
            "an unpublished post must not emit the key at all"
        );
        let published_map: BTreeMap<String, Value> =
            decode(&encode_canonical(&published).unwrap()).unwrap();
        assert!(
            matches!(published_map.get("web_slug"), Some(Value::String(s)) if s == "hello-world"),
            "the additive key is on the wire under its own name, got {:?}",
            published_map.get("web_slug")
        );
    }

    #[test]
    fn feed_local_posts_request_round_trips() {
        let req = FeedLocalPostsRequest {
            cursor: Some(1_700_000_000_000_000),
            limit: Some(25),
            search: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedLocalPostsRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_local_posts_reply_round_trips() {
        let reply = FeedLocalPostsReply {
            posts: vec![sample_post_item(false)],
            cursor: Some(1_700_000_000_000_000),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedLocalPostsReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    // ── contributors ───────────────────────────────────────────────────

    fn sample_contributor() -> FeedContributor {
        FeedContributor {
            nest_url: "https://peer.example".into(),
            author_id: Some("33".repeat(32)),
            hit_count: 7,
            last_seen: 1_700_000_000,
            poll_priority: "high".into(),
            discovered_via: "manual".into(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn feed_contributors_list_request_round_trips() {
        let req = FeedContributorsListRequest {
            feed_id: "0011223344556677".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedContributorsListRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_contributors_list_reply_round_trips() {
        let reply = FeedContributorsListReply {
            contributors: vec![
                sample_contributor(),
                FeedContributor {
                    nest_url: "https://seed.example".into(),
                    author_id: None,
                    hit_count: 0,
                    last_seen: 0,
                    poll_priority: "low".into(),
                    discovered_via: "seed".into(),
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedContributorsListReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn feed_contributors_list_reply_canonical_re_encodes_identically() {
        let reply = FeedContributorsListReply {
            contributors: vec![sample_contributor()],
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: FeedContributorsListReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn feed_contributor_grant_request_round_trips() {
        let req = FeedContributorGrantRequest {
            feed_id: "0011223344556677".into(),
            nest_url: "https://peer.example".into(),
            author_id: Some("44".repeat(32)),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedContributorGrantRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_contributor_grant_reply_added_round_trips() {
        let reply = FeedContributorGrantReply {
            added: true,
            reason: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedContributorGrantReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert!(decoded.reason.is_none());
    }

    #[test]
    fn feed_contributor_grant_reply_already_exists_round_trips() {
        let reply = FeedContributorGrantReply {
            added: false,
            reason: Some("already exists".into()),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedContributorGrantReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn feed_contributor_revoke_request_round_trips() {
        let req = FeedContributorRevokeRequest {
            feed_id: "0011223344556677".into(),
            nest_url: "https://peer.example".into(),
            author_id: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FeedContributorRevokeRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn feed_contributor_revoke_reply_round_trips() {
        let reply = FeedContributorRevokeReply {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FeedContributorRevokeReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }
}
