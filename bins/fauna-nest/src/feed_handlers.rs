//! WS-RPC handlers for the user-facing `fauna.feed.*` surface — the
//! feed-CRUD + feed-query + discovery-contributor plane end-user clients
//! invoke from the feed-management + feed-render affordances. Part of the
//! WS-RPC-everywhere migration (tracked internally; sibling of the
//! `fauna.posts.*` cluster `posts_handlers.rs`).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (User-only arms for these kinds); the same gate the `fauna.posts.*`,
//! `fauna.email.*`, `fauna.search.*`, and `fauna.conversations.*` user-facing
//! kinds use, partitioned by `CallerClass`.
//!
//! The business logic is **not** duplicated here — each handler decodes its
//! wire request, then calls the shared `pub(crate)` core fns the HTTP twins
//! also call (`feed_routes::*_core`), and maps the result onto the
//! `fauna.feed.*` reply / `RpcError` shapes. The HTTP twins were **DELETED**
//! in the WS-RPC-everywhere rip — `feed_routes.rs` now holds only the shared
//! `pub(crate) *_core` fns the kinds (and the federation feed-query path)
//! call, no HTTP handlers.
//!
//! Two wire conventions (`feed.rs` documents both):
//! - Feed `rules` ride typed (`rules: Vec<FilterRule>`, decoded by the
//!   request's own dag-cbor decode — a rule of unknown shape is a malformed
//!   payload); the handler hands them straight to the `*_core` fns, which
//!   store canonical dag-cbor (`feed_routes.rs::validate_and_encode_rules`).
//! - Post `score` rides as `i64` micro-units (×1e6); the handler converts at
//!   the boundary (`(f64 * 1e6).round() as i64` / the inverse).

use std::time::Duration;

use fauna_core::scoring::CompositionEntry;
use fauna_protocol::{
    RpcError, decode_strict as decode,
    feed::{
        FeedCompositionEntry, FeedContributor, FeedContributorGrantReply,
        FeedContributorGrantRequest, FeedContributorRevokeReply, FeedContributorRevokeRequest,
        FeedContributorsListReply, FeedContributorsListRequest, FeedCreateReply, FeedCreateRequest,
        FeedDeleteReply, FeedDeleteRequest, FeedFactorsGetReply, FeedFactorsGetRequest,
        FeedFactorsSetReply, FeedFactorsSetRequest, FeedGetReply, FeedGetRequest, FeedListReply,
        FeedListRequest, FeedLocalPostsReply, FeedLocalPostsRequest, FeedPostItem, FeedPostsReply,
        FeedPostsRequest, FeedSummary, FeedTrendingPostsReply, FeedTrendingPostsRequest,
        FeedUpdateReply, FeedUpdateRequest,
    },
};

use crate::api_error::ApiError;
use crate::db::ScoreCursor;
use crate::feed_routes::{
    FeedPostOut, FeedReader, add_contributor_core, create_feed_core, delete_feed_core,
    get_feed_core, list_contributors_core, query_feed_core, query_local_feed_core,
    query_trending_feed_core, remove_contributor_core, update_feed_core,
};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

/// The scored feeds' keyset cursor: both halves or neither. A key alone (the
/// pre-keyset client's shape, which left the wire with the compat-remnant
/// sweep) or a tiebreak alone is refused rather than paged by a made-up
/// predicate.
fn score_cursor(
    key_micro: Option<i64>,
    created_at: Option<i64>,
) -> Result<Option<ScoreCursor>, RpcError> {
    match (key_micro, created_at) {
        (Some(micro), Some(created_at)) => Ok(Some(ScoreCursor {
            key: micro_to_score(micro),
            created_at,
        })),
        (None, None) => Ok(None),
        (Some(_), None) => Err(invalid_params(
            "score_cursor without score_cursor_created_at",
        )),
        (None, Some(_)) => Err(invalid_params(
            "score_cursor_created_at without score_cursor",
        )),
    }
}

fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("feed", reason)
}

/// Map an `ApiError` (from the `feed_routes::*_core` fns) onto an `RpcError`,
/// preserving the HTTP status semantics: 400 → invalid_params, 404 →
/// not_found, 403 → permission_denied, else internal.
fn rpc_error_from_api(api: ApiError) -> RpcError {
    crate::rpc_errors::rpc_error_from_api_ns("feed", api, |ns, reason| {
        crate::rpc_errors::permission_denied_ns(ns, reason)
    })
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// Wire ⇄ core mapping for feed-composition entries (frame § Composition).
/// The wire mirror (`FeedCompositionEntry`, with its forward-compat `extra`
/// map) and the domain type (`fauna_core::scoring::CompositionEntry`, the
/// at-rest/composition-math shape) are field-identical by construction.
fn composition_from_wire(entries: &[FeedCompositionEntry]) -> Vec<CompositionEntry> {
    entries
        .iter()
        .map(|e| CompositionEntry {
            factor: e.factor.clone(),
            weight_permille: e.weight_permille,
        })
        .collect()
}
fn composition_to_wire(entry: CompositionEntry) -> FeedCompositionEntry {
    FeedCompositionEntry {
        factor: entry.factor,
        weight_permille: entry.weight_permille,
        extra: std::collections::BTreeMap::new(),
    }
}

/// Micro-unit conversions for the score field (fixed-point ×1e6 on the wire).
fn score_to_micro(score: f64) -> i64 {
    (score * 1e6).round() as i64
}
fn micro_to_score(micro: i64) -> f64 {
    micro as f64 / 1e6
}

/// Map a plane-agnostic `FeedPostOut` to the wire item, scaling the optional
/// score to micro-units.
fn post_item(p: &FeedPostOut) -> FeedPostItem {
    FeedPostItem {
        post_id: hex::encode(&p.post_id),
        author: hex::encode(&p.author),
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
        score: p.score.map(score_to_micro),
        // The quoted-post embed: the 32-byte `Reference::Quote` target id,
        // projected into the feed index as a `content_links 'quote'` row at
        // ingest and read by `query_feed` without touching `content.payload`
        // (`feed.md` § The read model). Hex-encoded to match `post_id`'s shape
        // so the client resolves the embed against the already-loaded post set.
        quoted_post_id: p.quoted_post_id.as_ref().map(hex::encode),
        // The repost carrier + per-viewer pair (`feed.md` § Interaction bar →
        // Repost, ratified 2026-08-10): `reposted_post_id` marks a repost row
        // (attribution + embedded original), `viewer_repost_id` is the
        // connection actor's own repost of this row's post — `unrepost`'s
        // argument — and `viewer_liked` its like-toggle state. All additive.
        reposted_post_id: p.reposted_post_id.as_ref().map(hex::encode),
        viewer_repost_id: p.viewer_repost_id.as_ref().map(hex::encode),
        viewer_liked: p.viewer_liked,
        // Gated-to-tier marker (`content_meta.gated_tier` — plaintext-floor
        // attribute, `ui/feed.md` § Encryption at rest): drives the apps'
        // `gated-post-badge`. Additive + `None`-default on the wire.
        gated_tier: p.gated_tier.clone(),
        // The room a room-restricted post addresses (`content_meta.gated_room`,
        // the arm's channel id — the same floor as the tier): a member's card
        // names the room by its own label. Additive + `None`-default.
        gated_room: p.gated_room.as_ref().map(hex::encode),
        // Web-publish state (`content_links link_type='web_published'`): the
        // slug the post serves under, `None` when unpublished. Together with
        // `gated_tier` this is what the ⋯ overflow derives its own-post web
        // verbs from (`ui/feed.md` § User actions). Additive + `None`-default.
        web_slug: p.web_slug.clone(),
        // Per-category content-label verdicts (`moderation.md` § Per-row badge
        // data path): drives the apps' `content-label-badge`. Additive +
        // empty-default on the wire.
        labels: p.labels.clone(),
        // A bridged author's face, projected after the query from
        // `bridge_authors` (`bridges.md` § Unified feed ingestion → *Bridged
        // authors*). Additive + `None`-default on the wire.
        author_display: p.author_display.clone(),
        extra: std::collections::BTreeMap::new(),
    }
}

// ── fauna.feed.list ────────────────────────────────────────────

fn feed_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.list").await?;
            let _req: FeedListRequest = decode(&payload).map_err(malformed)?;

            // No core needed — list_feeds has no logic beyond the DB read +
            // the per-feed projection (rules omitted, matching the HTTP shape).
            let feeds = state
                .db
                .list_feeds()
                .await
                .map_err(|e| internal(format!("list_feeds: {e}")))?;
            let feeds = feeds
                .iter()
                .map(|f| FeedSummary {
                    feed_id: f.feed_id.clone(),
                    owner: hex::encode(&f.owner),
                    name: f.name.clone(),
                    combination: f.combination.clone(),
                    created_at: f.created_at,
                    scope: f.scope.clone(),
                    contributor_seeds: serde_json::from_str::<Vec<String>>(&f.contributor_seeds)
                        .unwrap_or_default(),
                    extra: std::collections::BTreeMap::new(),
                })
                .collect();
            encode_reply(&FeedListReply {
                feeds,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.create ──────────────────────────────────────────

fn feed_create_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.create").await?;
            let req: FeedCreateRequest = decode(&payload).map_err(malformed)?;
            let composition = req.composition.as_deref().map(composition_from_wire);

            let feed_id = create_feed_core(
                &state,
                actor_id,
                &req.name,
                &req.rules,
                &req.combination,
                req.scope.as_deref(),
                req.contributor_seeds.as_deref(),
                composition.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;

            encode_reply(&FeedCreateReply {
                feed_id,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.get ─────────────────────────────────────────────

fn feed_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.get").await?;
            let req: FeedGetRequest = decode(&payload).map_err(malformed)?;

            let (feed, rules, composition) = get_feed_core(&state, &req.feed_id)
                .await
                .map_err(rpc_error_from_api)?;
            encode_reply(&FeedGetReply {
                feed_id: feed.feed_id,
                owner: hex::encode(&feed.owner),
                name: feed.name,
                rules,
                combination: feed.combination,
                created_at: feed.created_at,
                scope: feed.scope,
                contributor_seeds: serde_json::from_str::<Vec<String>>(&feed.contributor_seeds)
                    .unwrap_or_default(),
                composition: composition
                    .map(|entries| entries.into_iter().map(composition_to_wire).collect()),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.update ──────────────────────────────────────────

fn feed_update_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.update").await?;
            let req: FeedUpdateRequest = decode(&payload).map_err(malformed)?;
            let composition = req.composition.as_deref().map(composition_from_wire);

            update_feed_core(
                &state,
                actor_id,
                &req.feed_id,
                &req.name,
                &req.rules,
                &req.combination,
                composition.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;

            encode_reply(&FeedUpdateReply {
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.delete ──────────────────────────────────────────

fn feed_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.delete").await?;
            let req: FeedDeleteRequest = decode(&payload).map_err(malformed)?;

            delete_feed_core(&state, actor_id, &req.feed_id)
                .await
                .map_err(rpc_error_from_api)?;

            encode_reply(&FeedDeleteReply {
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.posts ───────────────────────────────────────────

fn feed_posts_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.posts").await?;
            let req: FeedPostsRequest = decode(&payload).map_err(malformed)?;

            let out = query_feed_core(
                &state,
                actor_id,
                &req.feed_id,
                req.cursor,
                req.limit,
                req.order.as_deref(),
                score_cursor(req.score_cursor, req.score_cursor_created_at)?,
                req.search.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;

            encode_reply(&FeedPostsReply {
                posts: out.posts.iter().map(post_item).collect(),
                cursor: out.cursor,
                score_cursor: out.score_cursor.map(score_to_micro),
                score_cursor_created_at: out.score_cursor_created_at,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.factors.{get,set} — the caller's GLOBAL factor set ──

fn feed_factors_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.factors.get").await?;
            let _req: FeedFactorsGetRequest = decode(&payload).map_err(malformed)?;

            let factors = state
                .db
                .get_global_factors(&actor_id)
                .await
                .map_err(|e| internal(format!("get_global_factors: {e}")))?;

            encode_reply(&FeedFactorsGetReply {
                factors: factors.into_iter().map(composition_to_wire).collect(),
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

fn feed_factors_set_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.factors.set").await?;
            let req: FeedFactorsSetRequest = decode(&payload).map_err(malformed)?;

            let entries = composition_from_wire(&req.factors);
            fauna_core::scoring::validate_composition(&entries)
                .map_err(|e| invalid_params(&format!("invalid factors: {e}")))?;

            state
                .db
                .set_global_factors(&actor_id, &entries)
                .await
                .map_err(|e| internal(format!("set_global_factors: {e}")))?;

            encode_reply(&FeedFactorsSetReply {
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.local.posts ─────────────────────────────────────

fn feed_local_posts_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.local.posts").await?;
            let req: FeedLocalPostsRequest = decode(&payload).map_err(malformed)?;

            let out = query_local_feed_core(
                &state,
                FeedReader::Account(actor_id),
                req.cursor,
                req.limit,
                req.search.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;

            encode_reply(&FeedLocalPostsReply {
                posts: out.posts.iter().map(post_item).collect(),
                cursor: out.cursor,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.trending.posts ──────────────────────────────────

fn feed_trending_posts_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.trending.posts").await?;
            let req: FeedTrendingPostsRequest = decode(&payload).map_err(malformed)?;

            let out = query_trending_feed_core(
                &state,
                FeedReader::Account(actor_id),
                score_cursor(req.score_cursor, req.score_cursor_created_at)?,
                req.limit,
                req.search.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;

            encode_reply(&FeedTrendingPostsReply {
                posts: out.posts.iter().map(post_item).collect(),
                score_cursor: out.score_cursor.map(score_to_micro),
                score_cursor_created_at: out.score_cursor_created_at,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── the principal twins of the two public timelines ────────────
//
// `fauna:feed:read` (`authorization-server.md` § Scope grammar → *The Fauna
// family, exactly*): the same cores, read as `FeedReader::Principal` — public
// posts only under the off-box predicate, and nothing of the account's own
// state. The gate (ceiling + scopes) ran in `principal_handlers` before these.

pub(crate) fn principal_feed_local_posts_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, _caller, payload| {
        Box::pin(async move {
            let req: FeedLocalPostsRequest = decode(&payload).map_err(malformed)?;
            let out = query_local_feed_core(
                &state,
                FeedReader::Principal,
                req.cursor,
                req.limit,
                req.search.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;
            encode_reply(&FeedLocalPostsReply {
                posts: out.posts.iter().map(post_item).collect(),
                cursor: out.cursor,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

pub(crate) fn principal_feed_trending_posts_handler() -> crate::principal_handlers::PrincipalHandler
{
    Box::new(|state, _caller, payload| {
        Box::pin(async move {
            let req: FeedTrendingPostsRequest = decode(&payload).map_err(malformed)?;
            let out = query_trending_feed_core(
                &state,
                FeedReader::Principal,
                score_cursor(req.score_cursor, req.score_cursor_created_at)?,
                req.limit,
                req.search.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;
            encode_reply(&FeedTrendingPostsReply {
                posts: out.posts.iter().map(post_item).collect(),
                score_cursor: out.score_cursor.map(score_to_micro),
                score_cursor_created_at: out.score_cursor_created_at,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.contributors.list ───────────────────────────────

fn feed_contributors_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.contributors.list").await?;
            let req: FeedContributorsListRequest = decode(&payload).map_err(malformed)?;

            let rows = list_contributors_core(&state, actor_id, &req.feed_id)
                .await
                .map_err(rpc_error_from_api)?;

            let contributors = rows
                .iter()
                .map(|c| FeedContributor {
                    nest_url: c.nest_url.clone(),
                    author_id: c.author_id.as_ref().map(hex::encode),
                    hit_count: c.hit_count,
                    last_seen: c.last_seen,
                    poll_priority: c.poll_priority.clone(),
                    discovered_via: c.discovered_via.clone(),
                    extra: std::collections::BTreeMap::new(),
                })
                .collect();
            encode_reply(&FeedContributorsListReply {
                contributors,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.contributors.grant ──────────────────────────────

fn feed_contributors_grant_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.contributors.grant").await?;
            let req: FeedContributorGrantRequest = decode(&payload).map_err(malformed)?;

            let outcome = add_contributor_core(
                &state,
                actor_id,
                &req.feed_id,
                &req.nest_url,
                req.author_id.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;

            if outcome.added {
                // A peering event: a new peer nest URL entered the peer set —
                // nudge the federation exchange originator so the first
                // exchange doesn't wait for the hourly tick.
                state.notify_exchange_transition();
            }

            encode_reply(&FeedContributorGrantReply {
                added: outcome.added,
                reason: outcome.reason,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.feed.contributors.revoke ─────────────────────────────

fn feed_contributors_revoke_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.feed.contributors.revoke").await?;
            let req: FeedContributorRevokeRequest = decode(&payload).map_err(malformed)?;

            remove_contributor_core(
                &state,
                actor_id,
                &req.feed_id,
                &req.nest_url,
                req.author_id.as_deref(),
            )
            .await
            .map_err(rpc_error_from_api)?;

            encode_reply(&FeedContributorRevokeReply {
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_feed_handlers(b: &mut RpcRouterBuilder) {
    // Per-kind replay semantics + rationale: see
    // `KindRegistry::register_feed_kinds`. Every feed kind is
    // `forbid_replay = false` @5 s — pure reads, or owner-keyed mutations
    // with no score-increment hazard (unlike `fauna.posts.interact`).
    b.add(
        "fauna.feed.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_list_handler(),
        },
    );
    b.add(
        "fauna.feed.create",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_create_handler(),
        },
    );
    b.add(
        "fauna.feed.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_get_handler(),
        },
    );
    b.add(
        "fauna.feed.update",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_update_handler(),
        },
    );
    b.add(
        "fauna.feed.delete",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_delete_handler(),
        },
    );
    b.add(
        "fauna.feed.posts",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_posts_handler(),
        },
    );
    b.add(
        "fauna.feed.local.posts",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_local_posts_handler(),
        },
    );
    b.add(
        "fauna.feed.trending.posts",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_trending_posts_handler(),
        },
    );
    b.add(
        "fauna.feed.contributors.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_contributors_list_handler(),
        },
    );
    b.add(
        "fauna.feed.contributors.grant",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_contributors_grant_handler(),
        },
    );
    b.add(
        "fauna.feed.contributors.revoke",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_contributors_revoke_handler(),
        },
    );
    b.add(
        "fauna.feed.factors.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_factors_get_handler(),
        },
    );
    b.add(
        "fauna.feed.factors.set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: feed_factors_set_handler(),
        },
    );
}
