//! UniFFI façade for the `fauna.feed.*` WS-RPC kinds — the feed-CRUD +
//! feed-query + discovery-contributor plane the feed-management + feed-render
//! affordances drive.
//!
//! [`FfiFeedClient`] wraps `fauna_client_feed::FeedClient` (which in turn wraps
//! the shared `NestClient`); the mirror records below are the FFI-visible shape
//! of `fauna_protocol::feed::*`. The Rust-native Linux app calls the same
//! `FeedClient` directly — this seam gives Apple / Windows / Android the
//! identical surface over UniFFI. Construct via
//! [`crate::nest_client::FfiNestClient::feed`].
//!
//! Wire convention (matching `events_client.rs`): **inputs** ride as method
//! args; **outputs** are full record mirrors so a reply field can't silently
//! drop (the freeform `extra` forward-compat map is dropped at the boundary).
//! Two cross-cutting wire rules from `fauna_protocol::feed`: feed `rules` ride
//! typed (`Vec<FilterRule>`), crossing this boundary as the create-feed form's
//! [`FfiFilterRule`] triples — the shared `fauna-client-feed` codec converts
//! both ways — and post scores ride as `i64` micro-units (×1e6). Hex ids
//! (`feed_id` / `owner` / `post_id` / `author`) cross as `String` — the feed
//! protocol already hex-encodes them (unlike events' raw `ByteBuf`).

use std::sync::Arc;

use crate::{FfiError, stringify};
use fauna_client::NestClient;
use fauna_client_feed::FeedClient;
use fauna_client_feed::feed::{
    FeedContributor, FeedGetReply, FeedLocalPostsReply, FeedPostItem, FeedPostsReply, FeedSummary,
};
use fauna_core::scoring::FilterRule;

// ── feed summary / get mirrors ───────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::feed::FeedSummary`] — the list-view feed
/// (omits `rules`, which ride only on `feed_get`).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFeedSummary {
    pub feed_id: String,
    /// Hex-encoded owner `[u8; 32]`.
    pub owner: String,
    pub name: String,
    pub combination: String,
    pub created_at: i64,
    /// `local` or `discovery`.
    pub scope: String,
    pub contributor_seeds: Vec<String>,
}

impl From<FeedSummary> for FfiFeedSummary {
    fn from(f: FeedSummary) -> Self {
        FfiFeedSummary {
            feed_id: f.feed_id,
            owner: f.owner,
            name: f.name,
            combination: f.combination,
            created_at: f.created_at,
            scope: f.scope,
            contributor_seeds: f.contributor_seeds,
        }
    }
}

/// FFI mirror of [`fauna_protocol::feed::FeedGetReply`] — a single feed
/// including its rules, decoded to the form's triples.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFeedGetReply {
    pub feed_id: String,
    /// Hex-encoded owner `[u8; 32]`.
    pub owner: String,
    pub name: String,
    /// The feed's typed rules, decoded by the shared codec
    /// (`fauna_client_feed::decode_filter_rules`).
    pub rules: Vec<FfiFilterRule>,
    pub combination: String,
    pub created_at: i64,
    pub scope: String,
    pub contributor_seeds: Vec<String>,
}

impl From<FeedGetReply> for FfiFeedGetReply {
    fn from(f: FeedGetReply) -> Self {
        FfiFeedGetReply {
            feed_id: f.feed_id,
            owner: f.owner,
            name: f.name,
            rules: fauna_client_feed::decode_filter_rules(&f.rules)
                .into_iter()
                .map(FfiFilterRule::from)
                .collect(),
            combination: f.combination,
            created_at: f.created_at,
            scope: f.scope,
            contributor_seeds: f.contributor_seeds,
        }
    }
}

// ── post item / query reply mirrors ──────────────────────────────────────

/// FFI mirror of [`fauna_protocol::feed::AuthorDisplay`] — a bridged author's
/// face (`bridges.md` § Unified feed ingestion → *Bridged authors*), minus the
/// wire `extra` map. Additive Record; consume-safe.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiAuthorDisplay {
    pub handle: Option<String>,
    pub display_name: Option<String>,
    /// Nest-relative, already proxied.
    pub avatar_url: Option<String>,
}

impl From<fauna_protocol::feed::AuthorDisplay> for FfiAuthorDisplay {
    fn from(a: fauna_protocol::feed::AuthorDisplay) -> Self {
        Self {
            handle: a.handle,
            display_name: a.display_name,
            avatar_url: a.avatar_url,
        }
    }
}

/// FFI mirror of [`fauna_protocol::feed::FeedPostItem`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFeedPostItem {
    /// Hex-encoded post id `[u8; 32]`.
    pub post_id: String,
    /// Hex-encoded author `[u8; 32]`.
    pub author: String,
    pub body: String,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub has_media: bool,
    pub is_reply: bool,
    pub source: String,
    /// Interaction-bar counters (ratified 2026-06-27) — drive the native
    /// apps' icon+count bar (count hidden at 0). Additive Record fields:
    /// consume-safe (clients receive `FfiFeedPostItem`, never construct it).
    pub like_count: i64,
    pub reply_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
    /// Composite score in fixed-point micro-units (×1e6); `None` outside the
    /// `order=score` branch (the dag-cbor wire forbids floats).
    pub score: Option<i64>,
    /// Hex-encoded `[u8; 32]` post id this post *quotes* (`Reference::Quote`
    /// target) — drives the embedded quoted-post card. `None` when the post
    /// quotes nothing, or when talking to a nest that doesn't yet project it.
    pub quoted_post_id: Option<String>,
    /// Hex-encoded `[u8; 32]` post id this post *reposts* — `Some` marks a
    /// repost row (attribution + embedded original; `feed.md` § Interaction
    /// bar → Repost, ratified 2026-08-10). Additive Record field,
    /// consume-safe.
    pub reposted_post_id: Option<String>,
    /// The connection actor's own live repost of this row's post —
    /// `unrepost`'s argument; presence = "reposted by me". `None` on bridged
    /// rows and on rows the actor has not reposted.
    pub viewer_repost_id: Option<String>,
    /// The connection actor's like-toggle state on this post.
    pub viewer_liked: bool,
    /// A bridged author's face; `None` for a native author. Additive Record field, consume-safe.
    pub author_display: Option<FfiAuthorDisplay>,
}

impl From<FeedPostItem> for FfiFeedPostItem {
    fn from(p: FeedPostItem) -> Self {
        FfiFeedPostItem {
            post_id: p.post_id,
            author: p.author,
            body: p.body,
            created_at: p.created_at,
            tags: p.tags,
            has_media: p.has_media,
            is_reply: p.is_reply,
            source: p.source,
            like_count: p.like_count,
            reply_count: p.reply_count,
            repost_count: p.repost_count,
            quote_count: p.quote_count,
            score: p.score,
            quoted_post_id: p.quoted_post_id,
            reposted_post_id: p.reposted_post_id,
            viewer_repost_id: p.viewer_repost_id,
            viewer_liked: p.viewer_liked,
            author_display: p.author_display.map(Into::into),
        }
    }
}

/// FFI mirror of [`fauna_protocol::feed::FeedPostsReply`] — a feed's posts plus
/// the pagination cursors (chronological `cursor` in the default branch, score
/// `score_cursor` in the `order=score` branch).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFeedPostsReply {
    pub posts: Vec<FfiFeedPostItem>,
    pub cursor: Option<i64>,
    pub score_cursor: Option<i64>,
    /// The tiebreak half of the score-order keyset cursor — echo it back beside
    /// `score_cursor` on the next page (`fauna_protocol::feed::FeedPostsReply`).
    pub score_cursor_created_at: Option<i64>,
}

impl From<FeedPostsReply> for FfiFeedPostsReply {
    fn from(r: FeedPostsReply) -> Self {
        FfiFeedPostsReply {
            posts: r.posts.into_iter().map(Into::into).collect(),
            cursor: r.cursor,
            score_cursor: r.score_cursor,
            score_cursor_created_at: r.score_cursor_created_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::feed::FeedLocalPostsReply`] — the local feed
/// (always chronological, so no `score_cursor`).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFeedLocalPostsReply {
    pub posts: Vec<FfiFeedPostItem>,
    pub cursor: Option<i64>,
}

impl From<FeedLocalPostsReply> for FfiFeedLocalPostsReply {
    fn from(r: FeedLocalPostsReply) -> Self {
        FfiFeedLocalPostsReply {
            posts: r.posts.into_iter().map(Into::into).collect(),
            cursor: r.cursor,
        }
    }
}

// ── contributor mirrors ──────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::feed::FeedContributor`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFeedContributor {
    pub nest_url: String,
    /// Hex-encoded author id `[u8; 32]`, when scoped to a specific author.
    pub author_id: Option<String>,
    pub hit_count: i64,
    pub last_seen: i64,
    pub poll_priority: String,
    pub discovered_via: String,
}

impl From<FeedContributor> for FfiFeedContributor {
    fn from(c: FeedContributor) -> Self {
        FfiFeedContributor {
            nest_url: c.nest_url,
            author_id: c.author_id,
            hit_count: c.hit_count,
            last_seen: c.last_seen,
            poll_priority: c.poll_priority,
            discovered_via: c.discovered_via,
        }
    }
}

/// FFI mirror of [`fauna_protocol::feed::FeedContributorGrantReply`].
/// `added=false` with a `reason` means the contributor already existed.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFeedContributorGrantReply {
    pub added: bool,
    pub reason: Option<String>,
}

// ── FfiFeedClient ──────────────────────────────────────────────────────────

/// UniFFI handle for the `fauna.feed.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::feed`]; methods are exposed to Swift as
/// `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiFeedClient {
    nest: Arc<NestClient>,
}

impl FfiFeedClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> FeedClient<Arc<NestClient>> {
        FeedClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiFeedClient {
    /// `fauna.feed.list` — list all feeds (summary view, no rules).
    pub async fn feed_list(&self) -> Result<Vec<FfiFeedSummary>, FfiError> {
        let reply = self.client().feed_list().await.map_err(stringify)?;
        Ok(reply.feeds.into_iter().map(Into::into).collect())
    }

    /// `fauna.feed.create` — create a feed. `rules` are the form's triples,
    /// encoded by the shared codec (an unknown `rule_type` is an `Err` before
    /// any nest call). `scope` is `local` (default) or
    /// `discovery`; `contributor_seeds` apply only to discovery feeds. Returns
    /// the generated `feed_id`.
    pub async fn feed_create(
        &self,
        name: String,
        rules: Vec<FfiFilterRule>,
        combination: String,
        scope: Option<String>,
        contributor_seeds: Option<Vec<String>>,
    ) -> Result<String, FfiError> {
        let rules = encode_rules(rules)?;
        let reply = self
            .client()
            .feed_create(name, rules, combination, scope, contributor_seeds, None)
            .await
            .map_err(stringify)?;
        Ok(reply.feed_id)
    }

    /// `fauna.feed.get` — fetch a single feed including its rules.
    pub async fn feed_get(&self, feed_id: String) -> Result<FfiFeedGetReply, FfiError> {
        let reply = self.client().feed_get(feed_id).await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.feed.update` — overwrite a feed's `name` / `rules` /
    /// `combination` (scope + seeds are immutable on update).
    pub async fn feed_update(
        &self,
        feed_id: String,
        name: String,
        rules: Vec<FfiFilterRule>,
        combination: String,
    ) -> Result<(), FfiError> {
        let rules = encode_rules(rules)?;
        self.client()
            .feed_update(feed_id, name, rules, combination, None)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.feed.delete` — delete an owned feed.
    pub async fn feed_delete(&self, feed_id: String) -> Result<(), FfiError> {
        self.client()
            .feed_delete(feed_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.feed.posts` — query a feed's posts. `order = Some("score")`
    /// orders by score and paginates on the keyset pair `(score_cursor,
    /// score_cursor_created_at)` — both echoed back from the previous reply,
    /// both or neither; otherwise chronological (paginate via `cursor`, epoch
    /// micros). `search` appends a comma-separated body-contains filter.
    // One arg per `fauna.feed.posts` wire param: this is a UniFFI method, so a
    // params struct would churn the generated C#/Swift/Kotlin call sites for
    // zero wire change.
    #[allow(clippy::too_many_arguments)]
    pub async fn feed_posts(
        &self,
        feed_id: String,
        cursor: Option<i64>,
        limit: Option<i64>,
        order: Option<String>,
        score_cursor: Option<i64>,
        score_cursor_created_at: Option<i64>,
        search: Option<String>,
    ) -> Result<FfiFeedPostsReply, FfiError> {
        let reply = self
            .client()
            .feed_posts(
                feed_id,
                cursor,
                limit,
                order,
                score_cursor,
                score_cursor_created_at,
                search,
            )
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.feed.local.posts` — query this nest's local feed (always
    /// chronological).
    pub async fn feed_local_posts(
        &self,
        cursor: Option<i64>,
        limit: Option<i64>,
        search: Option<String>,
    ) -> Result<FfiFeedLocalPostsReply, FfiError> {
        let reply = self
            .client()
            .feed_local_posts(cursor, limit, search)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.feed.contributors.list` — list a discovery feed's contributors
    /// (owner-only).
    pub async fn feed_contributors_list(
        &self,
        feed_id: String,
    ) -> Result<Vec<FfiFeedContributor>, FfiError> {
        let reply = self
            .client()
            .feed_contributors_list(feed_id)
            .await
            .map_err(stringify)?;
        Ok(reply.contributors.into_iter().map(Into::into).collect())
    }

    /// `fauna.feed.contributors.grant` — add a contributor to a discovery feed.
    /// `added=false` with a `reason` means it already existed.
    pub async fn feed_contributors_grant(
        &self,
        feed_id: String,
        nest_url: String,
        author_id: Option<String>,
    ) -> Result<FfiFeedContributorGrantReply, FfiError> {
        let reply = self
            .client()
            .feed_contributors_grant(feed_id, nest_url, author_id)
            .await
            .map_err(stringify)?;
        Ok(FfiFeedContributorGrantReply {
            added: reply.added,
            reason: reply.reason,
        })
    }

    /// `fauna.feed.contributors.revoke` — remove a contributor from a discovery
    /// feed. A missing contributor surfaces as a `fauna.feed.not_found` error.
    pub async fn feed_contributors_revoke(
        &self,
        feed_id: String,
        nest_url: String,
        author_id: Option<String>,
    ) -> Result<(), FfiError> {
        self.client()
            .feed_contributors_revoke(feed_id, nest_url, author_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }
}

/// UniFFI façade for [`fauna_client_feed::encode_filter_rule`] — the shared
/// feed-rule encoder every native app uses. Builds one real
/// [`fauna_core::scoring::FilterRule`] from the create-feed form's
/// `(rule_type, value, required)` triple and renders it as JSON — the same
/// externally-tagged shape each element of the typed `rules` wire list takes
/// (e.g. `{"BodyContains":{"terms":["x"]}}`), exposed for inspection and the
/// apps' cross-language conformance tests (the feed calls themselves take
/// [`FfiFilterRule`] triples). Because it builds the *real* `FilterRule`, it
/// can't drift from what the nest decodes; an unknown `rule_type` is an
/// `Err`, not a `{}`.
///
/// The encoding logic lives in `fauna-client-feed` so the Rust-native Linux
/// app can call it directly (no FFI hop); this thin wrapper just maps the
/// shared fn's `String` error onto [`FfiError`] for the Apple / Windows /
/// Android binding. Input conventions (`docs/goal/ui/feed.md` § Filter rule
/// types): comma-separated lists; integer `count` for `MinReplies`/`MinReposts`;
/// `CreatedAfter` input is **hours**; the label rules pack `"category:threshold"`
/// with `threshold` on a **0–10** scale (`× 100` → per-mille `u16`); the toggles
/// (`HasMedia`/`IsReply`) read `required`.
#[uniffi::export]
pub fn encode_filter_rule(
    rule_type: String,
    value: String,
    required: bool,
) -> Result<String, FfiError> {
    let rule = fauna_client_feed::encode_filter_rule(&rule_type, &value, required)
        .map_err(|msg| FfiError::General { msg })?;
    serde_json::to_string(&rule).map_err(|e| FfiError::General { msg: e.to_string() })
}

/// FFI mirror of [`fauna_client_feed::DecodedFilterRule`] — one decoded feed-rule
/// `(rule_type, value, required)` triple a feed-edit form binds to. The inverse of
/// [`encode_filter_rule`]'s input.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFilterRule {
    pub rule_type: String,
    pub value: String,
    pub required: bool,
}

impl From<fauna_client_feed::DecodedFilterRule> for FfiFilterRule {
    fn from(r: fauna_client_feed::DecodedFilterRule) -> Self {
        FfiFilterRule {
            rule_type: r.rule_type,
            value: r.value,
            required: r.required,
        }
    }
}

impl From<FfiFilterRule> for fauna_client_feed::DecodedFilterRule {
    fn from(r: FfiFilterRule) -> Self {
        fauna_client_feed::DecodedFilterRule {
            rule_type: r.rule_type,
            value: r.value,
            required: r.required,
        }
    }
}

/// Encode the form's triples into the typed wire `rules` via the shared
/// [`fauna_client_feed::encode_filter_rules`], mapping its `String` error.
fn encode_rules(rules: Vec<FfiFilterRule>) -> Result<Vec<FilterRule>, FfiError> {
    let triples: Vec<fauna_client_feed::DecodedFilterRule> =
        rules.into_iter().map(Into::into).collect();
    fauna_client_feed::encode_filter_rules(&triples).map_err(|msg| FfiError::General { msg })
}

/// Normalize the create-feed form's triples by round-tripping them through the
/// real `FilterRule` — [`fauna_client_feed::encode_filter_rules`] then
/// [`fauna_client_feed::decode_filter_rules`], exactly what a create followed
/// by a `fauna.feed.get` hands back (CSV spacing re-joined with `", "`, the
/// label 0–10 scale and `CreatedAfter` hours inverted in Rust). Keeps the
/// Apple / Windows / Android bindings off a hand-rolled per-app normalizer
/// that would drift from the codec (priority #2/#3); an unknown `rule_type` is
/// an `Err`, not a silent drop.
#[uniffi::export]
pub fn normalize_filter_rules(rules: Vec<FfiFilterRule>) -> Result<Vec<FfiFilterRule>, FfiError> {
    let typed = encode_rules(rules)?;
    Ok(fauna_client_feed::decode_filter_rules(&typed)
        .into_iter()
        .map(FfiFilterRule::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_feed::feed::{FeedPostItem, FeedSummary};
    use std::collections::BTreeMap;

    #[test]
    fn summary_maps_all_fields() {
        let s = FeedSummary {
            feed_id: "feed-1".into(),
            owner: "ab".repeat(32),
            name: "Home".into(),
            combination: "all".into(),
            created_at: 1_700_000_000,
            scope: "local".into(),
            contributor_seeds: vec!["https://seed.example".into()],
            extra: BTreeMap::new(),
        };
        let ffi: FfiFeedSummary = s.into();
        assert_eq!(ffi.feed_id, "feed-1");
        assert_eq!(ffi.owner, "ab".repeat(32));
        assert_eq!(ffi.scope, "local");
        assert_eq!(
            ffi.contributor_seeds,
            vec!["https://seed.example".to_string()]
        );
    }

    #[test]
    fn post_item_maps_score_and_flags() {
        let scored = FeedPostItem {
            post_id: "11".repeat(32),
            author: "22".repeat(32),
            body: "hello".into(),
            created_at: 1_700_000_000_000_000,
            tags: vec!["rust".into()],
            has_media: true,
            is_reply: false,
            source: "fauna".into(),
            like_count: 5,
            reply_count: 4,
            repost_count: 3,
            quote_count: 2,
            score: Some(12_500_000),
            quoted_post_id: Some("99".repeat(32)),
            gated_tier: None,
            ..Default::default()
        };
        let ffi: FfiFeedPostItem = scored.into();
        assert_eq!(ffi.post_id, "11".repeat(32));
        assert!(ffi.has_media);
        assert!(!ffi.is_reply);
        assert_eq!(ffi.score, Some(12_500_000));
        assert_eq!(ffi.quoted_post_id, Some("99".repeat(32)));
        assert_eq!(ffi.like_count, 5);
        assert_eq!(ffi.reply_count, 4);
        assert_eq!(ffi.repost_count, 3);
        assert_eq!(ffi.quote_count, 2);

        let unscored = FeedPostItem {
            post_id: "33".repeat(32),
            author: "44".repeat(32),
            body: "x".into(),
            created_at: 1,
            tags: vec![],
            has_media: false,
            is_reply: true,
            source: "bluesky".into(),
            like_count: 0,
            reply_count: 0,
            repost_count: 0,
            quote_count: 0,
            score: None,
            quoted_post_id: None,
            gated_tier: None,
            ..Default::default()
        };
        let ffi: FfiFeedPostItem = unscored.into();
        assert!(ffi.score.is_none());
        assert!(ffi.is_reply);
        assert_eq!(ffi.source, "bluesky");
        assert!(ffi.quoted_post_id.is_none());
    }

    // ── encode_filter_rule (FFI wrapper contract) ────────────────────────
    //
    // The encoding semantics (`docs/goal/ui/feed.md` § Filter rule types — hours
    // for `CreatedAfter`, the 0–10 → per-mille label scale, CSV splitting, etc.)
    // are exhaustively tested in `fauna-client-feed`'s `encoder` module, which
    // owns the logic. These two tests pin only what the FFI wrapper adds: it
    // delegates a known rule to the shared encoder verbatim, and maps the shared
    // `String` error onto an `FfiError` for an unknown rule type.

    #[test]
    fn wrapper_delegates_known_rule_to_shared_encoder() {
        let s = encode_filter_rule("BodyContains".into(), "rust, svelte".into(), false)
            .expect("encode should succeed for a known rule type");
        let v: serde_json::Value = serde_json::from_str(&s).expect("output must be valid JSON");
        assert_eq!(
            v,
            serde_json::json!({ "BodyContains": { "terms": ["rust", "svelte"] } })
        );
    }

    #[test]
    fn wrapper_maps_unknown_rule_type_to_ffi_error() {
        assert!(encode_filter_rule("Bogus".into(), "x".into(), false).is_err());
    }

    fn ffi_rule(rule_type: &str, value: &str, required: bool) -> FfiFilterRule {
        FfiFilterRule {
            rule_type: rule_type.into(),
            value: value.into(),
            required,
        }
    }

    #[test]
    fn normalize_round_trips_through_the_shared_codec() {
        let out = normalize_filter_rules(vec![
            ffi_rule("BodyContains", "rust,svelte", false),
            ffi_rule("LabelBelow", "spam:5", false),
        ])
        .expect("known rule types");
        assert_eq!(
            out,
            vec![
                ffi_rule("BodyContains", "rust, svelte", false),
                ffi_rule("LabelBelow", "spam:5", false),
            ]
        );
    }

    #[test]
    fn normalize_maps_unknown_rule_type_to_ffi_error() {
        assert!(normalize_filter_rules(vec![ffi_rule("Bogus", "x", false)]).is_err());
    }

    #[test]
    fn get_reply_decodes_typed_rules_to_triples() {
        let reply = FfiFeedGetReply::from(FeedGetReply {
            rules: vec![FilterRule::HasMedia { required: true }],
            ..Default::default()
        });
        assert_eq!(reply.rules, vec![ffi_rule("HasMedia", "", true)]);
    }
}
