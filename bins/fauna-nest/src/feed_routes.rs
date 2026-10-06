//! HTTP route handlers for feed CRUD and query.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use fauna_core::scoring::{FilterCombination, FilterRule};

use crate::api_error::ApiError;
use crate::routes::{AppState, FeedEvent};

fn default_combination() -> String {
    "all".into()
}

fn parse_combination(s: &str) -> Option<FilterCombination> {
    match s {
        "all" => Some(FilterCombination::All),
        "any" => Some(FilterCombination::Any),
        _ => None,
    }
}

/// Ensure the query enforces spam suppression: unless `existing` (the feed's
/// own rules) already carries an explicit spam filter — a feed may
/// deliberately set a looser threshold — push the default
/// `LabelBelow { "spam", 0.5 }` onto `mandatory`, the always-AND group.
/// Mandatory, not a feed rule: pushed into the feed's own rule set under an
/// `Any` combination it was a mere OR-alternative, so any post matching one
/// content rule bypassed spam suppression.
fn ensure_spam_filter(existing: &[FilterRule], mandatory: &mut Vec<FilterRule>) {
    let has_spam_filter = existing.iter().any(|r| match r {
        FilterRule::LabelBelow { category, .. } | FilterRule::LabelAbove { category, .. } => {
            category.starts_with("spam")
        }
        _ => false,
    });
    if !has_spam_filter {
        mandatory.push(FilterRule::LabelBelow {
            category: "spam".into(),
            max_confidence_permille: 500,
        });
    }
}

/// The viewer's block, applied where the viewer becomes known (`moderation.md`
/// § Corollary — block also hides): the authors `caller` blocked leave the
/// caller's own feed read. A MANDATORY rule, so a feed's `Any` combination can
/// never OR it away, and never inside the viewer-independent query fns a peer
/// nest also calls — the federated feed read adds no such rule. Unblocking
/// clears the edge, so the posts return on the next read.
fn push_viewer_block(caller: &[u8; 32], mandatory: &mut Vec<FilterRule>) {
    if caller != &[0u8; 32] {
        mandatory.push(FilterRule::AuthorNotInSet { actors: *caller });
    }
}

/// Parse a comma-separated search string into a BodyContains filter rule.
fn search_to_filter(search: &str) -> Option<FilterRule> {
    let terms: Vec<String> = search
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(FilterRule::BodyContains { terms })
    }
}

// ════════════════════════════════════════════════════════════════════════
// Shared `*_core` fns — the bodies behind both the (deprecated) HTTP twins
// and the `fauna.feed.*` WS-RPC handlers (`feed_handlers.rs`), part of the
// WS-RPC-everywhere migration (tracked internally). Each core does all the
// validation/DB/event logic; the twins/handlers only render the outcome onto
// their plane's response shape. Behavior-preserving.
// ════════════════════════════════════════════════════════════════════════

/// A single post in a feed-query result, plane-agnostic (the HTTP twin renders
/// it as JSON; the WS handler maps it to `fauna_protocol::feed::FeedPostItem`,
/// converting `score` to micro-units). Mirrors `db::FeedPostRow` minus the
/// storage representation.
pub(crate) struct FeedPostOut {
    pub post_id: Vec<u8>,
    pub author: Vec<u8>,
    pub body: String,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub has_media: bool,
    pub is_reply: bool,
    pub source: String,
    /// Interaction-bar counters (ratified 2026-06-27) carried straight from
    /// `content_meta` via `FeedPostRow`; the WS handler copies them onto
    /// `FeedPostItem.{like,reply,repost,quote}_count`.
    pub like_count: i64,
    pub reply_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
    /// 32-byte content id of the quote target (`None` for non-quoting posts);
    /// the WS handler hex-encodes it into `FeedPostItem.quoted_post_id`.
    pub quoted_post_id: Option<Vec<u8>>,
    /// 32-byte content id of the repost target — the quote twin; the WS
    /// handler hex-encodes it into `FeedPostItem.reposted_post_id`.
    pub reposted_post_id: Option<Vec<u8>>,
    /// The viewer's own live repost of this row's post (filled by the
    /// viewer-state augment on the local paths only); hex-encoded into
    /// `FeedPostItem.viewer_repost_id` — `unrepost`'s argument.
    pub viewer_repost_id: Option<Vec<u8>>,
    /// The viewer's like-toggle state → `FeedPostItem.viewer_liked`.
    pub viewer_liked: bool,
    /// Composite score as the raw `f64` from `query_feed_scored` (only the
    /// score branch populates it). The WS handler scales it to micro-units.
    pub score: Option<f64>,
    /// Tier name of a gated-to-tier post (`content_meta.gated_tier`); the WS
    /// handler copies it onto `FeedPostItem.gated_tier` → `gated-post-badge`.
    pub gated_tier: Option<String>,
    /// Channel id of the room a room-restricted post addresses
    /// (`content_meta.gated_room`); the WS handler hex-encodes it onto
    /// `FeedPostItem.gated_room` → a member's card names the room.
    pub gated_room: Option<Vec<u8>>,
    /// The post's web-publish slug (`content_links link_type='web_published'`,
    /// `None` = unpublished); the WS handler copies it onto
    /// `FeedPostItem.web_slug` → the ⋯-menu's own-post web verbs.
    pub web_slug: Option<String>,
    /// Per-category content-label verdicts (`moderation.md` § Per-row badge
    /// data path); the WS handler copies them onto `FeedPostItem.labels` →
    /// the apps' `content-label-badge`.
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// A **bridged** author's face (`bridges.md` § Unified feed ingestion →
    /// *Bridged authors*), filled by [`decorate_bridged_authors_best_effort`]
    /// after the page is queried — never by the query itself; the WS handler
    /// copies it onto `FeedPostItem.author_display`. `None` for a native
    /// author.
    pub author_display: Option<fauna_protocol::feed::AuthorDisplay>,
}

impl From<&crate::db::FeedPostRow> for FeedPostOut {
    fn from(p: &crate::db::FeedPostRow) -> Self {
        Self {
            post_id: p.post_id.clone(),
            author: p.author.clone(),
            body: p.body.clone(),
            created_at: p.created_at,
            tags: p.tags.clone(),
            has_media: p.has_media,
            is_reply: p.is_reply,
            source: p.source.clone(),
            like_count: p.like_count,
            reply_count: p.reply_count,
            repost_count: p.repost_count,
            quote_count: p.quote_count,
            quoted_post_id: p.quoted_post_id.clone(),
            reposted_post_id: p.reposted_post_id.clone(),
            viewer_repost_id: p.viewer_repost_id.clone(),
            viewer_liked: p.viewer_liked,
            score: p.score,
            gated_tier: p.gated_tier.clone(),
            gated_room: p.gated_room.clone(),
            web_slug: p.web_slug.clone(),
            labels: p.labels.clone(),
            author_display: None,
        }
    }
}

/// Fill `author_display` on every bridged row of a queried page from the
/// `bridge_authors` projection — the post-query decoration `bridges.md`
/// § Unified feed ingestion → *Bridged authors* rules: one batched read for the
/// page's distinct bridged authors, never a JOIN in `query_feed` (`feed.md`
/// § The read model's index-only `SELECT` is untouched). A page with no
/// bridged row costs nothing. Best-effort, mirroring
/// [`augment_viewer_state_best_effort`]: the face is a display nicety, so a
/// read failure serves the feed faceless rather than failing a query that
/// already succeeded.
///
/// Only rows whose `source` is not `fauna` are looked up: a native author has
/// no projection row today. Should a native profile projection ever fill the
/// same field, this filter is the one line to widen.
pub(crate) async fn decorate_bridged_authors_best_effort(
    state: &Arc<AppState>,
    posts: &mut [FeedPostOut],
) {
    let mut ids: Vec<[u8; 32]> = posts
        .iter()
        .filter(|p| p.source != "fauna")
        .filter_map(|p| <[u8; 32]>::try_from(p.author.as_slice()).ok())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return;
    }
    let conn = state.db.conn().await;
    let faces = match crate::db::bridge_authors::get_many(&conn, &ids) {
        Ok(faces) => faces,
        Err(e) => {
            tracing::warn!("bridge_authors decoration: {e}");
            return;
        }
    };
    for post in posts.iter_mut() {
        if let Ok(id) = <[u8; 32]>::try_from(post.author.as_slice())
            && let Some(author) = faces.get(&id)
        {
            post.author_display = Some(author.display());
        }
    }
}

/// Result of a feed-posts query. Exactly one cursor is `Some`, set by the
/// order branch that ran: chronological ⇒ `cursor` carries the trailing
/// `created_at` (`score_cursor` is `None`); score-ordered ⇒ `score_cursor`
/// carries the trailing `f64` score (the WS handler scales it to micro-units,
/// `cursor` is `None`). The `fauna.feed.posts` reply passes both through, so
/// the `Some`/`None` split distinguishes the shapes.
pub(crate) struct FeedPostsOut {
    pub posts: Vec<FeedPostOut>,
    pub cursor: Option<i64>,
    pub score_cursor: Option<f64>,
    /// The `created_at` of the same row `score_cursor` came from — the
    /// **tiebreak half** of the score-order keyset cursor. The scored sort is
    /// `ORDER BY key DESC, created_at DESC`, so a key-only cursor skips every
    /// row sharing the boundary key (and never advances at all when the key is
    /// flat, as it is for a composition of only sealed factors — every nest-side
    /// term is 0). Both halves travel together; see `query_feed_scored`.
    pub score_cursor_created_at: Option<i64>,
}

/// Shared rules-validation + storage-encode for create/update. Returns the
/// canonical dag-cbor-encoded rule bytes or a `bad_request` `ApiError`
/// mirroring the twins (Layer-6 at-rest encoding — serialization.md:29).
fn validate_and_encode_rules(combination: &str, rules: &[FilterRule]) -> Result<Vec<u8>, ApiError> {
    if parse_combination(combination).is_none() {
        return Err(ApiError::bad_request(
            "invalid combination: must be \"all\" or \"any\"",
        ));
    }
    // `canonical_encode` is `Sized`-bounded (unlike `serde_bare::to_vec`'s
    // `?Sized`), so pass `&rules` to encode the slice — serde serializes
    // `&&[FilterRule]` byte-identically to the `[FilterRule]` array.
    fauna_core::encoding::canonical_encode(&rules)
        .map_err(|e| ApiError::bad_request(format!("invalid rules: {e}")))
}

/// The rule a newer writer added that this nest cannot evaluate
/// (`transport.md` § Rule 3 in full) may reach the store only as a carried
/// value: an update that echoes a rule the feed **already stores** keeps it
/// byte for byte, while a rule this nest has never stored is refused typed —
/// a feed authored around a condition it cannot evaluate would silently yield
/// nothing. `stored` is the feed's current rule set (empty on create).
fn refuse_new_unknown_rules(rules: &[FilterRule], stored: &[FilterRule]) -> Result<(), ApiError> {
    match rules.iter().find(|r| !r.is_known() && !stored.contains(r)) {
        Some(_) => Err(ApiError::bad_request(
            "invalid rules: this nest cannot evaluate one of the rules",
        )),
        None => Ok(()),
    }
}

/// Validate + canonical-dag-cbor-encode a feed composition for at-rest
/// storage (`feeds.composition`), mirroring [`validate_and_encode_rules`].
/// An empty slice maps to `None` (NULL at rest) so "explicitly cleared" and
/// "never had one" are the same at-rest state (frame § Composition — no
/// composition ⇒ legacy single-score `order=score`).
fn validate_and_encode_composition(
    entries: &[fauna_core::scoring::CompositionEntry],
) -> Result<Option<Vec<u8>>, ApiError> {
    fauna_core::scoring::validate_composition(entries)
        .map_err(|e| ApiError::bad_request(format!("invalid composition: {e}")))?;
    if entries.is_empty() {
        return Ok(None);
    }
    fauna_core::encoding::canonical_encode(&entries)
        .map(Some)
        .map_err(|e| ApiError::bad_request(format!("invalid composition: {e}")))
}

/// Shared core behind `POST /api/v1/feeds` and `fauna.feed.create`.
pub(crate) async fn create_feed_core(
    state: &Arc<AppState>,
    owner: [u8; 32],
    name: &str,
    rules: &[FilterRule],
    combination: &str,
    scope: Option<&str>,
    contributor_seeds: Option<&[String]>,
    composition: Option<&[fauna_core::scoring::CompositionEntry]>,
) -> Result<String, ApiError> {
    refuse_new_unknown_rules(rules, &[])?;
    let rules_bytes = validate_and_encode_rules(combination, rules)?;
    let composition_bytes = match composition {
        None => None,
        Some(entries) => validate_and_encode_composition(entries)?,
    };

    // Tier-based feed limit check (off on the single-user desktop nest).
    if *state.enforce_tier_quotas.read().await {
        let max_feeds = state
            .db
            .get_user_tier_max_feeds(&owner)
            .await
            .map_err(|e| {
                tracing::error!("get_user_tier_max_feeds error: {e}");
                ApiError::forbidden("user not registered or tier lookup failed")
            })?;
        let current_count = state.db.count_feeds_by_owner(&owner).await.map_err(|e| {
            tracing::error!("count_feeds_by_owner error: {e}");
            ApiError::internal("storage error")
        })?;
        if current_count >= max_feeds {
            return Err(ApiError::forbidden("feed limit reached for your tier"));
        }
    }

    // Validate and extract scope
    let scope = scope.unwrap_or("local");
    if scope != "local" && scope != "discovery" {
        return Err(ApiError::bad_request("invalid scope"));
    }

    // Validate seed URLs
    let seeds = contributor_seeds.unwrap_or(&[]);
    for seed in seeds {
        if !seed.starts_with("http://") && !seed.starts_with("https://") {
            return Err(ApiError::bad_request(format!("invalid seed URL: {seed}")));
        }
    }
    let seeds_json = serde_json::to_string(seeds).unwrap_or_else(|_| "[]".to_string());

    let feed_id = state
        .db
        .create_feed(
            &owner,
            name,
            &rules_bytes,
            combination,
            scope,
            &seeds_json,
            composition_bytes.as_deref(),
        )
        .await
        .map_err(|e| {
            tracing::error!("create_feed error: {e}");
            ApiError::internal("storage error")
        })?;

    if scope == "discovery" {
        if let Some(seeds) = contributor_seeds {
            for seed_url in seeds {
                let _ = state
                    .db
                    .upsert_contributor(&feed_id, seed_url, None, "seed")
                    .await;
            }
            if !seeds.is_empty() {
                // A peering event: new peer nest URLs entered the exchange
                // originator's peer set (the contributors.grant twin).
                state.notify_exchange_transition();
            }
        }
        let _ = state
            .feed_event_tx
            .send(FeedEvent::Created {
                feed_id: feed_id.clone(),
            })
            .await;
    }
    Ok(feed_id)
}

/// Shared core behind `GET /api/v1/feeds/{id}` and `fauna.feed.get`. Returns
/// the feed row plus the dag-cbor-decoded rules and composition (both
/// decodes live here).
pub(crate) async fn get_feed_core(
    state: &Arc<AppState>,
    feed_id: &str,
) -> Result<
    (
        crate::db::FeedRow,
        Vec<FilterRule>,
        Option<Vec<fauna_core::scoring::CompositionEntry>>,
    ),
    ApiError,
> {
    let feed = match state.db.get_feed(feed_id).await {
        Ok(Some(f)) => f,
        Ok(None) => return Err(ApiError::not_found("feed not found")),
        Err(e) => {
            tracing::error!("get_feed error: {e}");
            return Err(ApiError::internal("storage error"));
        }
    };
    let rules: Vec<FilterRule> =
        fauna_core::encoding::canonical_decode(&feed.rules).map_err(|e| {
            tracing::error!("decode feed rules error: {e}");
            ApiError::internal("corrupt feed rules")
        })?;
    let composition: Option<Vec<fauna_core::scoring::CompositionEntry>> = match &feed.composition {
        None => None,
        Some(bytes) => Some(fauna_core::encoding::canonical_decode(bytes).map_err(|e| {
            tracing::error!("decode feed composition error: {e}");
            ApiError::internal("corrupt feed composition")
        })?),
    };
    Ok((feed, rules, composition))
}

/// Shared core behind `PUT /api/v1/feeds/{id}` and `fauna.feed.update`.
/// Returns `Ok(())` on success; `not_found` when no owned row matched.
pub(crate) async fn update_feed_core(
    state: &Arc<AppState>,
    owner: [u8; 32],
    feed_id: &str,
    name: &str,
    rules: &[FilterRule],
    combination: &str,
    composition: Option<&[fauna_core::scoring::CompositionEntry]>,
) -> Result<(), ApiError> {
    if rules.iter().any(|r| !r.is_known()) {
        // Only the stored set can vouch for a carried rule; an unreadable or
        // missing row vouches for none (the owner check below still decides
        // not_found for a feed that is not the caller's).
        let stored: Vec<FilterRule> = match state.db.get_feed(feed_id).await {
            Ok(Some(feed)) => {
                fauna_core::encoding::canonical_decode(&feed.rules).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        refuse_new_unknown_rules(rules, &stored)?;
    }
    let rules_bytes = validate_and_encode_rules(combination, rules)?;
    // Tri-state (the wire contract): `None` = leave the stored composition
    // unchanged; `Some([])` = clear (NULL at rest); `Some(entries)` = set.
    let composition_update = match composition {
        None => None,
        Some(entries) => Some(validate_and_encode_composition(entries)?),
    };

    match state
        .db
        .update_feed(
            feed_id,
            &owner,
            name,
            &rules_bytes,
            combination,
            composition_update.as_ref().map(|opt| opt.as_deref()),
        )
        .await
    {
        Ok(true) => {
            if let Ok(Some(feed)) = state.db.get_feed(feed_id).await
                && feed.scope == "discovery"
            {
                let _ = state.db.reset_contributor_stats_for_feed(feed_id).await;
                let _ = state
                    .feed_event_tx
                    .send(FeedEvent::Updated {
                        feed_id: feed_id.to_string(),
                    })
                    .await;
            }
            Ok(())
        }
        Ok(false) => Err(ApiError::not_found("feed not found or not owned")),
        Err(e) => {
            tracing::error!("update_feed error: {e}");
            Err(ApiError::internal("storage error"))
        }
    }
}

/// Shared core behind `DELETE /api/v1/feeds/{id}` and `fauna.feed.delete`.
/// Returns `Ok(())` on success; `not_found` when no owned row matched.
pub(crate) async fn delete_feed_core(
    state: &Arc<AppState>,
    owner: [u8; 32],
    feed_id: &str,
) -> Result<(), ApiError> {
    // Check scope before deleting so we can clean up contributors after.
    let was_discovery = matches!(
        state.db.get_feed(feed_id).await,
        Ok(Some(ref f)) if f.scope == "discovery"
    );

    match state.db.delete_feed(feed_id, &owner).await {
        Ok(true) => {
            // Ownership verified — safe to clean up discovery state.
            if was_discovery {
                let _ = state.db.delete_contributors_for_feed(feed_id).await;
                let _ = state
                    .feed_event_tx
                    .send(FeedEvent::Deleted {
                        feed_id: feed_id.to_string(),
                    })
                    .await;
            }
            Ok(())
        }
        Ok(false) => Err(ApiError::not_found("feed not found or not owned")),
        Err(e) => {
            tracing::error!("delete_feed error: {e}");
            Err(ApiError::internal("storage error"))
        }
    }
}

/// Who is reading one of the nest's public timelines (local, Trending).
///
/// The two timelines have one core each, whoever reads them; the reader
/// decides only what of the **account's own state** the read may use.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FeedReader {
    /// The account on its own app: its block list filters authors out, its
    /// global factors re-weight Trending, and each post carries its viewer
    /// state (liked, reposted).
    Account([u8; 32]),
    /// A third-party principal under `fauna:feed:read`
    /// (`authorization-server.md` § Scope grammar → *The Fauna family,
    /// exactly*): public posts only and nothing of the account's own state —
    /// no viewer state, no block list, no factors. Held to the off-box
    /// predicate ([`crate::db::feeds::FeedAudience::OffBox`]).
    Principal,
}

impl FeedReader {
    fn account(self) -> Option<[u8; 32]> {
        match self {
            FeedReader::Account(actor) => Some(actor),
            FeedReader::Principal => None,
        }
    }
}

/// Shared core behind `GET /api/v1/feeds/local/posts` and
/// `fauna.feed.local.posts` (the account's and the principal's). Always
/// chronological. Returns the posts + trailing `created_at` cursor.
pub(crate) async fn query_local_feed_core(
    state: &Arc<AppState>,
    reader: FeedReader,
    cursor: Option<i64>,
    limit: Option<i64>,
    search: Option<&str>,
) -> Result<FeedPostsOut, ApiError> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    // Spam guard + search are MANDATORY constraints (always AND) — the local
    // feed has no rules of its own.
    let mut mandatory: Vec<FilterRule> = Vec::new();
    ensure_spam_filter(&[], &mut mandatory);
    if let Some(caller) = reader.account() {
        push_viewer_block(&caller, &mut mandatory);
    }
    if let Some(search) = search
        && let Some(filter) = search_to_filter(search)
    {
        mandatory.push(filter);
    }
    let read = match reader {
        FeedReader::Account(_) => {
            state
                .db
                .query_feed(&[], FilterCombination::All, &mandatory, cursor, limit)
                .await
        }
        FeedReader::Principal => state.db.query_feed_off_box(&mandatory, cursor, limit).await,
    };
    match read {
        Ok(mut posts) => {
            if let Some(caller) = reader.account() {
                augment_viewer_state_best_effort(state, &caller, &mut posts).await;
            }
            let cursor = posts.last().map(|p| p.created_at);
            let mut posts: Vec<FeedPostOut> = posts.iter().map(FeedPostOut::from).collect();
            decorate_bridged_authors_best_effort(state, &mut posts).await;
            Ok(FeedPostsOut {
                posts,
                cursor,
                score_cursor: None,
                score_cursor_created_at: None,
            })
        }
        Err(e) => {
            tracing::error!("query_local_feed error: {e}");
            Err(ApiError::internal("query error"))
        }
    }
}

/// Shared core behind `fauna.feed.trending.posts` — the built-in **Trending**
/// virtual feed (`trending.md` § The Trending feed). No feed row: always
/// score-ordered over **public posts** by the implicit composition
/// `[(trending, 1000)]` plus the caller's global factor set, paginated by the
/// compound `(score_cursor, score_cursor_created_at)` keyset (`feed.md` § The
/// read model).
pub(crate) async fn query_trending_feed_core(
    state: &Arc<AppState>,
    reader: FeedReader,
    score_cursor: Option<crate::db::ScoreCursor>,
    limit: Option<i64>,
    search: Option<&str>,
) -> Result<FeedPostsOut, ApiError> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    // Spam guard + search are MANDATORY constraints (always AND) — the
    // Trending virtual feed has no rules of its own.
    let mut mandatory: Vec<FilterRule> = Vec::new();
    ensure_spam_filter(&[], &mut mandatory);
    if let Some(caller) = reader.account() {
        push_viewer_block(&caller, &mut mandatory);
    }
    if let Some(search) = search
        && let Some(filter) = search_to_filter(search)
    {
        mandatory.push(filter);
    }

    // Implicit `[(trending, 1000)]` + the caller's global factor set (scope is
    // by container: the trending term is this virtual feed's implicit
    // composition, the global entries fold in on top — the same factor in both
    // containers sums). A global promote/mute therefore composes with the trend
    // order (`feed.md` § The read model — the frame's arithmetic conflict
    // resolution). A sealed tier-1 factor (muted keywords, topic models) has no
    // nest-side `content_scores` row → contributes 0 here and is applied
    // client-side (the sealed-factor seam).
    // A principal folds in no factors: they are the account's own state.
    let mut composition = vec![fauna_core::scoring::CompositionEntry {
        factor: fauna_core::scoring::factor::TRENDING.to_string(),
        weight_permille: 1000,
    }];
    if let Some(caller) = reader.account() {
        let global = state.db.get_global_factors(&caller).await.map_err(|e| {
            tracing::error!("get_global_factors error: {e}");
            ApiError::internal("storage error")
        })?;
        composition.extend(global);
    }

    let read = match reader {
        FeedReader::Account(_) => {
            state
                .db
                .query_feed_scored_public(
                    &[],
                    FilterCombination::All,
                    &mandatory,
                    score_cursor,
                    limit,
                    &composition,
                )
                .await
        }
        FeedReader::Principal => {
            state
                .db
                .query_feed_scored_off_box(&mandatory, score_cursor, limit, &composition)
                .await
        }
    };
    match read {
        Ok(mut posts) => {
            if let Some(caller) = reader.account() {
                augment_viewer_state_best_effort(state, &caller, &mut posts).await;
            }
            // Both halves of the keyset cursor come from the SAME last row, so
            // the next page resumes exactly where this one stopped under the
            // compound `key DESC, created_at DESC` sort.
            let last = posts.last();
            let score_cursor = last.and_then(|p| p.score);
            let score_cursor_created_at = last.map(|p| p.created_at);
            let mut posts: Vec<FeedPostOut> = posts.iter().map(FeedPostOut::from).collect();
            decorate_bridged_authors_best_effort(state, &mut posts).await;
            Ok(FeedPostsOut {
                posts,
                cursor: None,
                score_cursor,
                score_cursor_created_at,
            })
        }
        Err(e) => {
            tracing::error!("query_trending_feed error: {e}");
            Err(ApiError::internal("query error"))
        }
    }
}

/// Shared core behind `GET /api/v1/feeds/{id}/posts` and `fauna.feed.posts`.
/// Loads the feed, decodes its rules, applies spam-suppression + the optional
/// search filter, then branches on `order=="score"` (score-ordered, paginated
/// via `score_cursor` as an `f64`) vs chronological (`cursor` as `created_at`).
pub(crate) async fn query_feed_core(
    state: &Arc<AppState>,
    caller: [u8; 32],
    feed_id: &str,
    cursor: Option<i64>,
    limit: Option<i64>,
    order: Option<&str>,
    score_cursor: Option<crate::db::ScoreCursor>,
    search: Option<&str>,
) -> Result<FeedPostsOut, ApiError> {
    // Load the feed definition + decode its rules + composition. The spam
    // guard + search go in the MANDATORY group, never into the feed's own
    // rules: under an `Any` combination they'd become OR-alternatives — search
    // could never narrow (the default e2e "General" feed is empty-rules+any:
    // the month-old test_feed_search_filters_posts apple red) and one matching
    // content rule bypassed the spam guard.
    let (feed, rules, composition) = get_feed_core(state, feed_id).await?;
    let mut mandatory: Vec<FilterRule> = Vec::new();
    ensure_spam_filter(&rules, &mut mandatory);
    push_viewer_block(&caller, &mut mandatory);
    if let Some(search) = search
        && let Some(filter) = search_to_filter(search)
    {
        mandatory.push(filter);
    }

    // Parse combination
    let combination = match parse_combination(&feed.combination) {
        Some(c) => c,
        None => {
            tracing::error!("invalid stored combination: {}", feed.combination);
            return Err(ApiError::internal("corrupt feed combination"));
        }
    };

    // Clamp limit to 1..200
    let limit = limit.unwrap_or(50).clamp(1, 200);

    // Branch on order mode: "score" uses score-based ordering & cursor
    if order == Some("score") {
        // A feed with a composition orders by the composed key over the
        // scoring bus (frame § Composition); without one, the legacy single
        // nest-computed recency/engagement score. The CALLER's global factor
        // set folds into every feed (scope is by container): a feed without
        // its own composition behaves as the implicit `[(engagement, 1000)]`
        // when global factors join it, so one global promote never silently
        // discards recency ordering. Same factor in both containers → the
        // two terms sum (conflicts resolve by arithmetic).
        let global = state.db.get_global_factors(&caller).await.map_err(|e| {
            tracing::error!("get_global_factors error: {e}");
            ApiError::internal("storage error")
        })?;
        let mut composition = match (composition, global.is_empty()) {
            (Some(c), _) => c,
            (None, false) => vec![fauna_core::scoring::CompositionEntry {
                factor: fauna_core::scoring::factor::ENGAGEMENT.to_string(),
                weight_permille: 1000,
            }],
            (None, true) => Vec::new(),
        };
        composition.extend(global);
        match state
            .db
            .query_feed_scored(
                &rules,
                combination,
                &mandatory,
                score_cursor,
                limit,
                &composition,
            )
            .await
        {
            Ok(mut posts) => {
                augment_viewer_state_best_effort(state, &caller, &mut posts).await;
                // Both halves of the keyset cursor come from the SAME last row,
                // so the next page resumes exactly where this one stopped under
                // the compound `key DESC, created_at DESC` sort.
                let last = posts.last();
                let score_cursor = last.and_then(|p| p.score);
                let score_cursor_created_at = last.map(|p| p.created_at);
                let mut posts: Vec<FeedPostOut> = posts.iter().map(FeedPostOut::from).collect();
                decorate_bridged_authors_best_effort(state, &mut posts).await;
                Ok(FeedPostsOut {
                    posts,
                    cursor: None,
                    score_cursor,
                    score_cursor_created_at,
                })
            }
            Err(e) => {
                tracing::error!("query_feed (scored) error: {e}");
                Err(ApiError::internal("query error"))
            }
        }
    } else {
        match state
            .db
            .query_feed(&rules, combination, &mandatory, cursor, limit)
            .await
        {
            Ok(mut posts) => {
                augment_viewer_state_best_effort(state, &caller, &mut posts).await;
                let cursor = posts.last().map(|p| p.created_at);
                let mut posts: Vec<FeedPostOut> = posts.iter().map(FeedPostOut::from).collect();
                decorate_bridged_authors_best_effort(state, &mut posts).await;
                Ok(FeedPostsOut {
                    posts,
                    cursor,
                    score_cursor: None,
                    score_cursor_created_at: None,
                })
            }
            Err(e) => {
                tracing::error!("query_feed error: {e}");
                Err(ApiError::internal("query error"))
            }
        }
    }
}

/// Fill the per-viewer pair on queried rows for the connection actor —
/// best-effort, mirroring `interact_routes::counts_after_act`'s stance: the
/// viewer state is a display nicety, so a read failure serves the feed
/// without it (defaults = the pre-repost rendering) rather than failing a
/// query that already succeeded. The federated `remote_query_feed` path
/// deliberately never calls this (it authenticates a peer nest, not the end
/// viewer — `feed.md` § Interaction bar → Repost).
async fn augment_viewer_state_best_effort(
    state: &Arc<AppState>,
    caller: &[u8; 32],
    posts: &mut [crate::db::FeedPostRow],
) {
    if let Err(e) = state.db.augment_viewer_state(caller, posts).await {
        tracing::warn!("augment_viewer_state: {e}");
    }
}

/// Parse an optional hex-encoded author_id string into a fixed 32-byte array.
/// `Err` carries the `bad_request` `ApiError` mirroring the HTTP twins.
pub(crate) fn parse_author_id_core(hex_str: Option<&str>) -> Result<Option<[u8; 32]>, ApiError> {
    match hex_str {
        None => Ok(None),
        Some(h) => {
            let arr: [u8; 32] = fauna_core::hex32::decode(h).map_err(|e| match e {
                fauna_core::hex32::Hex32Error::NotHex(_) => {
                    ApiError::bad_request("invalid author_id: not valid hex")
                }
                fauna_core::hex32::Hex32Error::WrongLength(_) => {
                    ApiError::bad_request("invalid author_id: must be 32 bytes")
                }
            })?;
            Ok(Some(arr))
        }
    }
}

/// Load a feed and verify `owner` owns it. Shared ownership gate for the
/// contributor cores. Returns the row or the matching `ApiError`
/// (`not_found` / `forbidden`).
async fn load_owned_feed(
    state: &Arc<AppState>,
    owner: [u8; 32],
    feed_id: &str,
) -> Result<crate::db::FeedRow, ApiError> {
    let feed = match state.db.get_feed(feed_id).await {
        Ok(Some(f)) => f,
        Ok(None) => return Err(ApiError::not_found("feed not found")),
        Err(e) => {
            tracing::error!("get_feed error: {e}");
            return Err(ApiError::internal("storage error"));
        }
    };
    if feed.owner != owner.to_vec() {
        return Err(ApiError::forbidden("not your feed"));
    }
    Ok(feed)
}

/// Outcome of `add_contributor_core`: whether the contributor was newly added
/// (HTTP twin's `201 {added:true}`) or already existed (`200
/// {added:false,reason}`).
pub(crate) struct AddContributorOutcome {
    pub added: bool,
    pub reason: Option<String>,
}

/// Shared core behind `POST /api/v1/feeds/{id}/contributors` and
/// `fauna.feed.contributors.grant`.
pub(crate) async fn add_contributor_core(
    state: &Arc<AppState>,
    owner: [u8; 32],
    feed_id: &str,
    nest_url: &str,
    author_id: Option<&str>,
) -> Result<AddContributorOutcome, ApiError> {
    let feed = load_owned_feed(state, owner, feed_id).await?;
    if feed.scope != "discovery" {
        return Err(ApiError::bad_request(
            "contributors only apply to discovery feeds",
        ));
    }

    let author_arr = parse_author_id_core(author_id)?;

    // Check if contributor already exists (to decide 200 vs 201)
    let existing = state.db.list_contributors(feed_id).await.map_err(|e| {
        tracing::error!("list_contributors error: {e}");
        ApiError::internal("storage error")
    })?;
    let already_exists = existing.iter().any(|c| {
        c.nest_url == nest_url
            && c.author_id.as_deref().map(|b| b.to_vec()) == author_arr.map(|a| a.to_vec())
    });

    state
        .db
        .upsert_contributor(feed_id, nest_url, author_arr.as_ref(), "manual")
        .await
        .map_err(|e| {
            tracing::error!("upsert_contributor error: {e}");
            ApiError::internal("storage error")
        })?;

    let _ = state
        .feed_event_tx
        .send(FeedEvent::ContributorAdded {
            feed_id: feed_id.to_string(),
            nest_url: nest_url.to_string(),
            author_id: author_arr.map(|a| a.to_vec()),
        })
        .await;

    Ok(if already_exists {
        AddContributorOutcome {
            added: false,
            reason: Some("already exists".into()),
        }
    } else {
        AddContributorOutcome {
            added: true,
            reason: None,
        }
    })
}

/// Shared core behind `GET /api/v1/feeds/{id}/contributors` and
/// `fauna.feed.contributors.list`.
pub(crate) async fn list_contributors_core(
    state: &Arc<AppState>,
    owner: [u8; 32],
    feed_id: &str,
) -> Result<Vec<crate::db::ContributorRow>, ApiError> {
    load_owned_feed(state, owner, feed_id).await?;
    state.db.list_contributors(feed_id).await.map_err(|e| {
        tracing::error!("list_contributors error: {e}");
        ApiError::internal("storage error")
    })
}

/// Shared core behind `DELETE /api/v1/feeds/{id}/contributors` and
/// `fauna.feed.contributors.revoke`. Returns `Ok(())` on success;
/// `not_found` when no matching contributor row existed (the twin's `404`).
pub(crate) async fn remove_contributor_core(
    state: &Arc<AppState>,
    owner: [u8; 32],
    feed_id: &str,
    nest_url: &str,
    author_id: Option<&str>,
) -> Result<(), ApiError> {
    let feed = load_owned_feed(state, owner, feed_id).await?;
    if feed.scope != "discovery" {
        return Err(ApiError::bad_request(
            "contributors only apply to discovery feeds",
        ));
    }

    let author_arr = parse_author_id_core(author_id)?;

    match state
        .db
        .remove_contributor(feed_id, nest_url, author_arr.as_ref())
        .await
    {
        Ok(true) => {
            let _ = state
                .feed_event_tx
                .send(FeedEvent::ContributorRemoved {
                    feed_id: feed_id.to_string(),
                    nest_url: nest_url.to_string(),
                    author_id: author_arr.map(|a| a.to_vec()),
                })
                .await;
            Ok(())
        }
        Ok(false) => Err(ApiError::not_found("contributor not found")),
        Err(e) => {
            tracing::error!("remove_contributor error: {e}");
            Err(ApiError::internal("storage error"))
        }
    }
}

/// A reference from a post to another post, included in remote query responses.
///
/// `pub(crate)` + dual-derive so the Track-D federation handler
/// (`fauna.federation.feed.query`) can reuse it as part of the channel reply —
/// the cross-nest feed query rides the federation channel, not just HTTP (§4.E).
/// Fields are `pub(crate)` so the consumer side (`peer_query::parse_peer_candidate`)
/// converts a fetched candidate into a `ScoredCandidate` directly off this shared
/// wire type — both the producer (this nest) and the consumer (the querying nest)
/// use one struct family, no duplicate.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PostReferenceResponse {
    /// Hex of the full 36-byte CID (codec byte included).
    pub(crate) post_id: String,
    pub(crate) author: String,
    pub(crate) nest_url: Option<String>,
    pub(crate) ref_type: String,
}

/// Response item for a single candidate in the remote query.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RemoteQueryCandidate {
    /// Hex of the full 36-byte CID (codec byte included), so the querying
    /// nest decodes the codec faithfully instead of assuming dag-cbor.
    pub(crate) post_id: String,
    pub(crate) author: String,
    pub(crate) source: String,
    pub(crate) created_at: i64,
    pub(crate) score: Option<f64>,
    pub(crate) scorer_version: Option<u64>,
    pub(crate) metadata: RemoteQueryMetadata,
    #[serde(default)]
    pub(crate) references: Vec<PostReferenceResponse>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RemoteQueryMetadata {
    pub(crate) tags: Vec<String>,
    pub(crate) has_media: bool,
    pub(crate) is_reply: bool,
}

/// Full response for the `fauna.federation.feed.query` channel kind (the former
/// `POST /api/v1/feeds/query` HTTP route was retired in Spec Y2 slice 5). The struct itself is `pub` so the channel originator wrapper
/// (`federation_pool::originate_feed_query`, a `pub` fn the tier_3 conformance
/// test calls) can name it as an opaque "the channel served a reply" value; its
/// fields stay `pub(crate)` (peer_query reads them in-crate, externals can't).
#[derive(Clone, Serialize, Deserialize)]
pub struct RemoteQueryResponse {
    pub(crate) candidates: Vec<RemoteQueryCandidate>,
    pub(crate) cursor: Option<i64>,
}

/// Request body for the `fauna.federation.feed.query` channel kind (remote nest
/// queries).
#[derive(Clone, Serialize, Deserialize)]
pub struct RemoteQueryRequest {
    pub rules: Vec<FilterRule>,
    #[serde(default = "default_combination")]
    pub combination: String,
    /// If provided, only return posts by these authors (hex-encoded ActorIds).
    pub authors: Option<Vec<String>>,
    pub limit: Option<i64>,
    pub cursor: Option<i64>,
}

/// Encode a local post's 32-byte index digest as the peer-query wire CID.
///
/// A post rests as canonical dag-cbor wire bytes keyed by their `blake3` digest
/// (`routes::ingest_post_core`; its segment record CID is `Cid::of_dag_cbor` of
/// the same bytes), so this nest — the authority on its own posts' codec — stamps
/// the dag-cbor codec onto the full 36-byte CID it puts on the wire. The querying nest then
/// decodes the codec faithfully (`peer_query::cid_from_wire_hex`) instead of
/// assuming it. A non-32-byte digest (data corruption) is emitted as-is and
/// dropped downstream by the CID validator.
fn candidate_post_id_wire(digest: &[u8]) -> String {
    match <[u8; 32]>::try_from(digest) {
        Ok(d) => crate::peer_query::cid_to_wire_hex(&fauna_cbor::Cid::from_digest_dag_cbor(d)),
        Err(_) => hex::encode(digest),
    }
}

/// Core for the `fauna.federation.feed.query` channel kind (§4.E): query this
/// nest's post index with the peer's filter rules (optionally author-scoped),
/// returning scored candidates + their post references. Read-only over public
/// post data; the peer-auth (the channel handshake) is the caller's concern.
pub(crate) async fn remote_query_feed_core(
    state: &Arc<AppState>,
    req: &RemoteQueryRequest,
) -> Result<RemoteQueryResponse, ApiError> {
    // Validate combination
    let combination = parse_combination(&req.combination)
        .ok_or_else(|| ApiError::bad_request("invalid combination: must be \"all\" or \"any\""))?;

    // Validate limits
    let limit = req.limit.unwrap_or(50).clamp(1, 200);
    if req.rules.len() > 20 {
        return Err(ApiError::bad_request("too many rules (max 20)"));
    }

    // Parse author hex strings to bytes
    let authors: Option<Vec<Vec<u8>>> = match &req.authors {
        Some(hex_authors) => {
            if hex_authors.len() > 500 {
                return Err(ApiError::bad_request("too many authors (max 500)"));
            }
            let mut parsed = Vec::with_capacity(hex_authors.len());
            for h in hex_authors {
                let bytes =
                    hex::decode(h).map_err(|_| ApiError::bad_request("invalid author hex"))?;
                parsed.push(bytes);
            }
            Some(parsed)
        }
        None => None,
    };

    // Apply the default spam guard as a MANDATORY (always-AND) constraint
    // unless the caller's rules carry their own spam filter.
    let rules = req.rules.clone();
    let mut mandatory: Vec<FilterRule> = Vec::new();
    ensure_spam_filter(&rules, &mut mandatory);

    // Query the post index
    let posts = if let Some(authors) = &authors {
        state
            .db
            .query_feed_for_authors(&rules, combination, &mandatory, authors, req.cursor, limit)
            .await
    } else {
        state
            .db
            .query_feed(&rules, combination, &mandatory, req.cursor, limit)
            .await
    }
    .map_err(|e| {
        tracing::error!("remote_query_feed error: {e}");
        ApiError::internal("query error")
    })?;

    let cursor = posts.last().map(|p| p.created_at);
    let mut candidates: Vec<RemoteQueryCandidate> = Vec::with_capacity(posts.len());
    for p in &posts {
        // Load the post body segment-first (inline `content.payload`
        // fallback) and extract its references from the bytes — `content
        // .payload` is empty for posts after the segment-store cutover.
        let refs = match <[u8; 32]>::try_from(p.post_id.as_slice()) {
            Ok(pid) => {
                match crate::segments::post::load_post_body(&state.post_segments, &state.db, &pid)
                    .await
                {
                    Ok(Some(body)) => state
                        .db
                        .get_post_references(&body)
                        .await
                        .unwrap_or_default(),
                    _ => Vec::new(),
                }
            }
            Err(_) => Vec::new(),
        };
        let references: Vec<PostReferenceResponse> = refs
            .into_iter()
            .map(|(pid, author, rt, nest_url)| PostReferenceResponse {
                // `pid` is already the full 36-byte CID
                // (`db::posts::get_post_references`) — hex of the
                // full CID is the peer-query wire form.
                post_id: hex::encode(&pid),
                author: hex::encode(&author),
                nest_url,
                ref_type: rt,
            })
            .collect();
        candidates.push(RemoteQueryCandidate {
            post_id: candidate_post_id_wire(&p.post_id),
            author: hex::encode(&p.author),
            source: p.source.clone(),
            created_at: p.created_at,
            score: None,
            scorer_version: None,
            metadata: RemoteQueryMetadata {
                tags: p.tags.clone(),
                has_media: p.has_media,
                is_reply: p.is_reply,
            },
            references,
        });
    }
    Ok(RemoteQueryResponse { candidates, cursor })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layer-6 (Domain F) discriminating red: the at-rest `feed.rules` BLOB
    /// produced by the create/update path MUST be canonical dag-cbor
    /// (serialization.md:29 — "every byte … on disk … goes through one
    /// canonical encoder"), not BARE. Pre-flip `serde_bare` bytes fail strict
    /// `canonical_decode` with `NotCanonical`; post-flip they round-trip.
    #[test]
    fn feed_rules_at_rest_is_canonical_dagcbor() {
        let rules = vec![
            FilterRule::HasHashtag {
                tags: vec!["cat".into()],
            },
            FilterRule::MinReplies { count: 3 },
            FilterRule::LabelBelow {
                category: "spam".into(),
                max_confidence_permille: 800,
            },
        ];
        let bytes = validate_and_encode_rules("all", &rules)
            .ok()
            .expect("encode rules");
        // Strict canonical dag-cbor decode — BARE bytes fail with `NotCanonical`.
        let decoded: Vec<FilterRule> = fauna_core::encoding::canonical_decode(&bytes)
            .expect("at-rest feed rules must be canonical dag-cbor");
        assert_eq!(decoded.len(), rules.len());
        assert!(matches!(decoded[0], FilterRule::HasHashtag { .. }));
    }

    // ── fauna.feed.trending.posts (Phase 2 slice 5) ────────────────────
    //
    // The virtual Trending read: `query_trending_feed_core` orders public
    // posts by the implicit composition `[(trending, 1000)]` + the caller's
    // global factor set (`trending.md` § The Trending feed / `feed.md` § The
    // read model). Exercises the whole core (glue + public-only filter +
    // compound cursor) via `AppState::for_test`.
    use crate::db::{CacheDb, ScoreCursor};
    use fauna_core::scoring::{CompositionEntry, ScoreEntry, TIER_COMMUNITY, factor};

    /// Seed a public post carrying a `trending` bus row of `trend_pm` (‰).
    async fn seed_trending_post(
        db: &CacheDb,
        post_id: [u8; 32],
        author: [u8; 32],
        created_at: i64,
        trend_pm: i64,
    ) {
        db.seed_scored_post_for_test(&post_id, &author, created_at, factor::TRENDING, trend_pm)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn trending_feed_orders_by_trend_and_folds_global_factors() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let caller = [9u8; 32];
        let author = [7u8; 32];

        // A hottest, C coolest; distinct created_at exercises the tiebreak.
        let (pa, pb, pc) = ([0xA1u8; 32], [0xB1u8; 32], [0xC1u8; 32]);
        seed_trending_post(&db, pa, author, 1_000_000, 800).await;
        seed_trending_post(&db, pb, author, 2_000_000, 500).await;
        seed_trending_post(&db, pc, author, 3_000_000, 200).await;

        // Implicit [(trending, 1000)], no global factors → pure trend order.
        let out =
            query_trending_feed_core(&state, FeedReader::Account(caller), None, Some(10), None)
                .await
                .ok()
                .expect("trending read");
        let ids: Vec<_> = out.posts.iter().map(|p| p.post_id.clone()).collect();
        assert_eq!(
            ids,
            vec![pa.to_vec(), pb.to_vec(), pc.to_vec()],
            "posts come back in descending trend order"
        );
        assert_eq!(
            out.posts[0].score,
            Some(800.0),
            "composed key = w·trend/1000"
        );

        // The caller mutes an otherwise-trending post via a GLOBAL factor — a
        // transparent, nest-readable entry (NOT the sealed muted-keyword list,
        // which composes client-side with a 0 nest term). It folds into the
        // composition and composes arithmetically with the trend order
        // (`feed.md` § The read model): A's 800 trend + (−1000)·1000/1000 =
        // −200, sinking A below B (500) and C (200).
        db.insert_content_scores(
            &pa,
            "post",
            None,
            1_000_000,
            &[ScoreEntry {
                factor: "labeler:mute".to_string(),
                score: 1000,
                tier: TIER_COMMUNITY,
                scorer_version: 1,
            }],
        )
        .await
        .unwrap();
        db.set_global_factors(
            &caller,
            &[CompositionEntry {
                factor: "labeler:mute".to_string(),
                weight_permille: -1000,
            }],
        )
        .await
        .unwrap();

        let out =
            query_trending_feed_core(&state, FeedReader::Account(caller), None, Some(10), None)
                .await
                .ok()
                .expect("trending read");
        let ids: Vec<_> = out.posts.iter().map(|p| p.post_id.clone()).collect();
        assert_eq!(
            ids,
            vec![pb.to_vec(), pc.to_vec(), pa.to_vec()],
            "a global mute sinks the trending post below the un-muted ones"
        );
    }

    /// The post-query decoration (`bridges.md` § Unified feed ingestion →
    /// *Bridged authors*): a bridged row whose author has a projection row
    /// comes back with its face; a native row, and a bridged row whose author
    /// was never projected, come back faceless — across the local and the
    /// feed read alike, the `SELECT` untouched.
    #[tokio::test]
    async fn local_feed_decorates_bridged_authors_with_their_projected_face() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let caller = [9u8; 32];
        let (native, bridged, faceless) = ([0x11u8; 32], [0x22u8; 32], [0x33u8; 32]);
        db.insert_post_index_entry(
            &[0xA1u8; 32],
            &native,
            1_000_000,
            false,
            false,
            "fauna",
            &[],
        )
        .await
        .unwrap();
        db.insert_post_index_entry(
            &[0xB1u8; 32],
            &bridged,
            2_000_000,
            false,
            false,
            "activitypub",
            &[],
        )
        .await
        .unwrap();
        db.insert_post_index_entry(
            &[0xC1u8; 32],
            &faceless,
            3_000_000,
            false,
            false,
            "nostr",
            &[],
        )
        .await
        .unwrap();
        {
            let conn = db.conn().await;
            crate::db::bridge_authors::upsert(
                &conn,
                &crate::db::bridge_authors::BridgeAuthor {
                    actor_id: bridged,
                    bridge: crate::db::bridge_authors::BRIDGE_ACTIVITYPUB.into(),
                    external_id: "https://remote.example/users/bob".into(),
                    handle: Some("@bob@remote.example".into()),
                    display_name: Some("Bob".into()),
                    avatar_url: None,
                    updated_at: 1,
                },
            )
            .unwrap();
        }

        let out = query_local_feed_core(&state, FeedReader::Account(caller), None, Some(10), None)
            .await
            .ok()
            .expect("local read");
        let by_author = |id: [u8; 32]| {
            out.posts
                .iter()
                .find(|p| p.author == id.to_vec())
                .expect("the row is on the page")
        };
        let face = by_author(bridged)
            .author_display
            .as_ref()
            .expect("the bridged row carries its author's face");
        assert_eq!(face.handle.as_deref(), Some("@bob@remote.example"));
        assert_eq!(face.display_name.as_deref(), Some("Bob"));
        assert!(by_author(native).author_display.is_none());
        assert!(by_author(faceless).author_display.is_none());
    }

    /// A gated (paywalled) post never surfaces in the public Trending read even
    /// when it carries the highest trend row (`trending.md` § The Trending feed
    /// — public posts only; the read goes through `query_feed_scored_public`).
    #[tokio::test]
    async fn trending_feed_excludes_gated_posts() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let author = [7u8; 32];

        let public = [0xA2u8; 32];
        let gated = [0xB2u8; 32];
        seed_trending_post(&db, public, author, 1_000_000, 300).await;
        seed_trending_post(&db, gated, author, 2_000_000, 900).await; // higher trend
        // Paywall the second post (monetization gated tier). `gated_tier IS NOT
        // NULL` ⇒ non-public ⇒ excluded by the trending read's public filter.
        db.execute_batch(&format!(
            "UPDATE content_meta SET gated_tier = 'premium' WHERE content_id = x'{}'",
            hex::encode(gated)
        ))
        .await
        .unwrap();

        let out =
            query_trending_feed_core(&state, FeedReader::Account([9u8; 32]), None, Some(10), None)
                .await
                .ok()
                .expect("trending read");
        let ids: Vec<_> = out.posts.iter().map(|p| p.post_id.clone()).collect();
        assert_eq!(
            ids,
            vec![public.to_vec()],
            "the gated post is excluded despite its higher trend score"
        );
    }

    /// `fauna:feed:read`'s entry condition (`authorization-server.md` § Scope
    /// grammar → *The Fauna family, exactly*): a non-public post never appears
    /// in a principal's reply — on either timeline. A gated post and an
    /// archive import are both on the account's own local read and absent from
    /// the principal's (the off-box predicate); the gated one is the highest
    /// trend row and still absent from the principal's Trending.
    #[tokio::test]
    async fn a_principal_reads_public_posts_only_on_both_timelines() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let author = [7u8; 32];
        let (public, gated, archive) = ([0xA4u8; 32], [0xB4u8; 32], [0xC4u8; 32]);
        seed_trending_post(&db, public, author, 1_000_000, 300).await;
        seed_trending_post(&db, gated, author, 2_000_000, 900).await;
        seed_trending_post(&db, archive, author, 3_000_000, 600).await;
        db.execute_batch(&format!(
            "UPDATE content_meta SET gated_tier = 'premium' WHERE content_id = x'{}';
             UPDATE content SET source = '{}' WHERE id = x'{}';",
            hex::encode(gated),
            fauna_core::source::ARCHIVE_PLATFORMS[0],
            hex::encode(archive),
        ))
        .await
        .unwrap();
        let ids = |out: &FeedPostsOut| -> Vec<Vec<u8>> {
            out.posts.iter().map(|p| p.post_id.clone()).collect()
        };

        let own = query_local_feed_core(&state, FeedReader::Account(author), None, Some(10), None)
            .await
            .ok()
            .expect("account local read");
        assert_eq!(
            ids(&own).len(),
            3,
            "the account's own read keeps all three — the control"
        );

        let local = query_local_feed_core(&state, FeedReader::Principal, None, Some(10), None)
            .await
            .ok()
            .expect("principal local read");
        assert_eq!(ids(&local), vec![public.to_vec()]);

        let trending =
            query_trending_feed_core(&state, FeedReader::Principal, None, Some(10), None)
                .await
                .ok()
                .expect("principal trending read");
        assert_eq!(ids(&trending), vec![public.to_vec()]);
        assert!(
            trending.posts.iter().all(|p| !p.viewer_liked),
            "no viewer state on a principal's read"
        );
    }

    /// The compound keyset cursor paginates the trending read with no dupes or
    /// gaps — `(score_cursor, score_cursor_created_at)` echoed from the last row.
    #[tokio::test]
    async fn trending_feed_cursor_paginates_without_dupes() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db.clone()));
        let caller = [9u8; 32];
        let author = [7u8; 32];

        let (pa, pb, pc) = ([0xA3u8; 32], [0xB3u8; 32], [0xC3u8; 32]);
        seed_trending_post(&db, pa, author, 1_000_000, 800).await;
        seed_trending_post(&db, pb, author, 2_000_000, 500).await;
        seed_trending_post(&db, pc, author, 3_000_000, 200).await;

        let page1 =
            query_trending_feed_core(&state, FeedReader::Account(caller), None, Some(2), None)
                .await
                .ok()
                .expect("trending read page 1");
        let ids1: Vec<_> = page1.posts.iter().map(|p| p.post_id.clone()).collect();
        assert_eq!(ids1, vec![pa.to_vec(), pb.to_vec()]);

        // Resume from the last row's compound cursor.
        let cursor = ScoreCursor {
            key: page1.score_cursor.unwrap(),
            created_at: page1.score_cursor_created_at.unwrap(),
        };
        let page2 = query_trending_feed_core(
            &state,
            FeedReader::Account(caller),
            Some(cursor),
            Some(2),
            None,
        )
        .await
        .ok()
        .expect("trending read page 2");
        let ids2: Vec<_> = page2.posts.iter().map(|p| p.post_id.clone()).collect();
        assert_eq!(
            ids2,
            vec![pc.to_vec()],
            "second page picks up exactly where the first stopped"
        );
    }
}
