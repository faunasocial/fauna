//! WS-RPC handler for full-text search — `fauna.search.query`. A faithful
//! transport migration of the old `GET /api/v1/search` route; routes through
//! `state.storage().search`, which every nest serves over the **floor-derived**
//! corpus (public post bodies, restricted-post public previews, profiles). The
//! storage-mode fork it used to carry — encrypted → `not_server_side` — retired
//! with the axis in Phase 4 (`nest/storage-modes.md`): the index reads nothing
//! sealed, so there is no posture on which it could leak, and every box now
//! answers. Search over *sealed* content runs at a capability position (the MDA
//! session / the user's client) against the `__index` segments, never here. The
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
            let offset = req.offset.unwrap_or(0).max(0);
            let spec = SearchSpec {
                query: req.query,
                content_type: req.content_type,
                before: req.before,
                after: req.after,
                limit,
                offset,
            };

            let storage = state.storage();
            match storage.search(&actor_id, &spec).await {
                Ok(hits) => {
                    let results = hits
                        .into_iter()
                        .map(|h| SearchResult {
                            content_type: h.content_type,
                            content_id: h.content_id,
                            created_at: h.created_at,
                            // Negate BM25 (higher = more relevant) and scale to
                            // fixed-point micro-units — the dag-cbor wire forbids
                            // floats (see `fauna_protocol::search::SearchResult`).
                            rank: (-h.rank * 1_000_000.0).round() as i64,
                            snippet: h.snippet,
                            extra: Default::default(),
                        })
                        .collect();
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
