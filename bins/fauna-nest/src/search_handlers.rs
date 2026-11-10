//! WS-RPC handler for full-text search — `fauna.search.query`. A faithful
//! transport migration of the old `GET /api/v1/search` route; routes through
//! `state.storage().search`, which every nest serves over the **floor-derived**
//! corpus (public post bodies, restricted-post public previews, profiles). The
//! storage-mode fork it used to carry — encrypted → `not_server_side` — retired
//! with the axis in Phase 4 (`nest/storage-modes.md`): the index reads nothing
//! sealed, so there is no posture on which it could leak, and every box now
//! answers. Search over *sealed* content runs at a capability position (the MDA
//! session / the user's client) against the `__index` segments — with one
//! carve-in: for an actor whose materialization grant this nest's materializer
//! holds, the handler also queries that actor's readable views (mail, calendar,
//! cards — one per granted kind) and ranks their hits into the same page, for the connection actor alone
//! (`encryption-at-rest.md` § Capability tiering → *The materialization
//! grant*, part (d); [`crate::materializer`]). The
//! HTTP twin was **DELETED** in the WS-RPC-everywhere rip (`search_routes.rs`
//! gone, Batch 1).

use std::time::Duration;

use fauna_protocol::decode_strict as decode;
use fauna_protocol::search as protocol_search;
use fauna_protocol::{
    RpcError,
    search::{SearchQueryReply, SearchQueryRequest, SearchResult},
};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use crate::storage::SearchSpec;

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

use crate::rpc_errors::internal;

fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns("search", reason)
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── fauna.search.query ─────────────────────────────────────────

/// The `extra` key a readable mail-index hit carries its nest message id
/// under — the local doc's own field name (`fauna_index::IndexedDoc::secondary_id`).
pub const MATERIALIZED_SECONDARY_ID_KEY: &str = "secondary_id";

/// The materialized kinds a request's `content_type` filter asks for: every
/// kind when unfiltered, else each kind whose `content_type` the filter names
/// exactly or as a `<type>/` prefix. The second value says the filter names a
/// materialized kind exactly — the floor corpus holds none of those, so the
/// readable views answer alone.
fn materialized_kinds(content_type: Option<&str>) -> (Vec<crate::materializer::MatKind>, bool) {
    use crate::materializer::MatKind;
    let Some(t) = content_type else {
        return (MatKind::ALL.to_vec(), false);
    };
    let kinds = MatKind::ALL
        .into_iter()
        .filter(|k| {
            let ct = k.content_type();
            t == ct || t.strip_prefix(ct).is_some_and(|rest| rest.starts_with('/'))
        })
        .collect();
    let only = MatKind::ALL.iter().any(|k| k.content_type() == t);
    (kinds, only)
}

/// The request's strictly-after / strictly-before epoch-micros cursors as the
/// index's half-open nanosecond range.
fn time_range(after: Option<i64>, before: Option<i64>) -> Option<fauna_index::TimeRange> {
    if after.is_none() && before.is_none() {
        return None;
    }
    Some(fauna_index::TimeRange {
        start_ns: after.map_or(i64::MIN, |a| a.saturating_mul(1_000).saturating_add(1)),
        end_ns: before.map_or(i64::MAX, |b| b.saturating_mul(1_000)),
    })
}

/// Rank the merged hits (higher first, newer on a tie) and cut one page.
fn page(mut results: Vec<SearchResult>, offset: i64, limit: i64) -> Vec<SearchResult> {
    results.sort_by(|a, b| b.rank.cmp(&a.rank).then(b.created_at.cmp(&a.created_at)));
    results
        .into_iter()
        .skip(usize::try_from(offset).unwrap_or(0))
        .take(usize::try_from(limit).unwrap_or(0))
        .collect()
}

fn query_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.search.query").await?;
            let req: SearchQueryRequest = decode(&payload).map_err(malformed)?;

            if req.query.trim().is_empty() {
                return Err(invalid_params("query is required"));
            }

            // Page-size contract lives in `fauna_protocol::search` (the one
            // owner) so clients can read the same ceiling — see
            // `fauna_client_search::paging::SearchPaging`. These values are the
            // ones the deleted HTTP twin `search_routes::search` applied.
            let limit = req
                .limit
                .unwrap_or(protocol_search::DEFAULT_LIMIT)
                .clamp(protocol_search::MIN_LIMIT, protocol_search::MAX_LIMIT);
            let offset = req
                .offset
                .unwrap_or(0)
                .clamp(0, protocol_search::MAX_OFFSET);
            // The owner's readable views (`encryption-at-rest.md`
            // § Capability tiering → *The materialization grant*, part (d)):
            // queried only for the CONNECTION actor — the ACL — and only for
            // the kinds whose materialization grant of theirs stands here.
            // Their hits merge into the one reply ranked beside the floor
            // corpus's, so each arm is asked for the first `offset + limit`
            // and the merged ranking is paged once.
            let (kinds, materialized_only) = materialized_kinds(req.content_type.as_deref());
            let window = usize::try_from(offset + limit).unwrap_or(usize::MAX);
            let materialized = match &state.materializer {
                Some(m) if !kinds.is_empty() => {
                    m.search(
                        &actor_id,
                        &kinds,
                        &req.query,
                        time_range(req.after, req.before),
                        window,
                    )
                    .await
                }
                _ => None,
            };
            let mut results: Vec<SearchResult> = materialized
                .unwrap_or_default()
                .into_iter()
                .map(|h| {
                    let mut extra = std::collections::BTreeMap::new();
                    if let Some(nest_id) = h.nest_message_id {
                        extra.insert(
                            MATERIALIZED_SECONDARY_ID_KEY.to_string(),
                            fauna_cbor::Value::Bytes(nest_id),
                        );
                    }
                    SearchResult {
                        content_type: h.kind.content_type().to_string(),
                        content_id: h.content_id,
                        created_at: h.created_at,
                        rank: (f64::from(h.score) * 1_000_000.0).round() as i64,
                        snippet: h.snippet,
                        extra,
                    }
                })
                .collect();
            let merged = !results.is_empty() || materialized_only;
            if materialized_only {
                return encode_reply(&SearchQueryReply {
                    results: page(results, offset, limit),
                    extra: std::collections::BTreeMap::new(),
                });
            }
            let spec = SearchSpec {
                query: req.query,
                content_type: req.content_type,
                before: req.before,
                after: req.after,
                limit: if merged { offset + limit } else { limit },
                offset: if merged { 0 } else { offset },
            };

            let storage = state.storage();
            match storage.search(&actor_id, &spec).await {
                Ok(hits) => {
                    results.extend(hits.into_iter().map(|h| SearchResult {
                        content_type: h.content_type,
                        content_id: h.content_id,
                        created_at: h.created_at,
                        // Negate BM25 (higher = more relevant) and scale to
                        // fixed-point micro-units — the dag-cbor wire forbids
                        // floats (see `fauna_protocol::search::SearchResult`).
                        rank: (-h.rank * 1_000_000.0).round() as i64,
                        snippet: h.snippet,
                        extra: Default::default(),
                    }));
                    let results = if merged {
                        page(results, offset, limit)
                    } else {
                        results
                    };
                    encode_reply(&SearchQueryReply {
                        results,
                        extra: std::collections::BTreeMap::new(),
                    })
                }
                // `SealedStorage::search` wraps an fts5 syntax error as Internal
                // with "fts5: syntax error" in `reason` — surface it as a client
                // input error, matching the twin's 400.
                Err(e) if e.reason.contains("fts5: syntax error") => {
                    Err(invalid_params("invalid search query syntax"))
                }
                Err(e) => {
                    tracing::error!("search error: {e}");
                    Err(internal("search error"))
                }
            }
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_search_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.search.query",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: query_handler(),
        },
    );
}
