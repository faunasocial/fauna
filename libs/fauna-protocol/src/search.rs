//! User-facing WS-RPC payload types for full-text search —
//! `fauna.search.query`. A faithful transport migration of the
//! `GET /api/v1/search` HTTP route (`bins/fauna-nest/src/search_routes.rs`);
//! the wire shapes mirror `storage::SearchSpec` / `storage::SearchHit`
//! (all scalar fields — search returns hit *metadata*, not content
//! objects). Slice scoped and tracked internally.
//!
//! Kind registry entry lives in `kind.rs::register_search_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use fauna_cbor::Value;

// ── fauna.search.query page-size contract ──────────────────────────────
//
// The nest clamps every request's `limit` into `[MIN_LIMIT, MAX_LIMIT]` and
// substitutes `DEFAULT_LIMIT` when absent. These three constants are the ONE
// owner of that contract: `search_handlers.rs::query_handler` applies them
// nest-side, and `fauna_client_search::paging::SearchPaging` reads `MAX_LIMIT`
// so a client's page limit can never outrun what the nest will serve. Before
// they existed, the clamp was a bare `.clamp(1, 100)` in the handler that no
// client knew about — every app's "load more" bumped its limit past 100 and
// got a silently truncated page back (`docs/goal/ui/search.md` § Layout &
// flow).

/// Page size the nest substitutes when a request omits `limit`.
pub const DEFAULT_LIMIT: i64 = 20;

/// Smallest page size the nest will serve; smaller requests clamp up to it.
pub const MIN_LIMIT: i64 = 1;

/// Largest page size the nest will serve. A request for more is clamped DOWN
/// to this silently — there is no "you asked for too much" error — so a client
/// asking beyond it receives a full-looking page that is really the ceiling.
///
/// ⚠ **Raising this is a wire-behavior change, not a constant tweak.** Client
/// and nest compile it in separately and deploy at different times (a client
/// may talk to several nests of differing versions at once), so a client built
/// with a larger ceiling will meet nests still clamping to the old one: it
/// asks past the old ceiling, receives a silently truncated page, and its
/// paging predicate (`fauna_client_search::paging::SearchPaging::has_more`)
/// reads the short page as end-of-results — "load more" quietly stops at the
/// old ceiling with rows left on the nest. The reverse skew is benign (an
/// older client saturates at its own smaller compiled-in constant). Bump only
/// with a skew story per `docs/goal/architecture/version-compatibility.md` —
/// e.g. the reply carrying the serving nest's effective ceiling so clients
/// follow it instead of their compiled-in constant.
pub const MAX_LIMIT: i64 = 100;

// ── fauna.search.query ─────────────────────────────────────────────────

/// Full-text search scoped to the calling actor (implicit — the WS-RPC
/// connection knows its caller). Mirrors the HTTP twin's query params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchQueryRequest {
    /// The search query. Required — an empty/whitespace query is rejected
    /// with `fauna.search.invalid_params`.
    pub query: String,
    /// Optional content-type filter (e.g. `"post"`, `"profile"`). It names a
    /// CLASS: a hit matches when its type is the filter itself or any
    /// `<filter>/<subtype>` of it, so `"post"` finds `post/text` and
    /// `post/media` while `"post/media"` narrows to that subtype alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Only hits created strictly before this epoch-micros cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
    /// Only hits created strictly after this epoch-micros cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    /// Page size; the handler defaults to 20 and clamps to `[1, 100]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Page offset; the handler defaults to 0 (floored at 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchQueryReply {
    pub results: Vec<SearchResult>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One search hit — mirrors `storage::SearchHit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResult {
    /// Content-type tag (e.g. `"post"`).
    pub content_type: String,
    /// Content id (the storage layer's hit identifier).
    pub content_id: String,
    /// Creation timestamp (epoch micros).
    pub created_at: i64,
    /// Relevance score as fixed-point **micro-units** (negated BM25 × 1e6, so
    /// higher = more relevant). Integer because the dag-cbor wire forbids
    /// floats — see `docs/goal/architecture/serialization.md` § Floats.
    pub rank: i64,
    /// Highlighted snippet.
    pub snippet: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn sample_request() -> SearchQueryRequest {
        SearchQueryRequest {
            query: "hello world".into(),
            content_type: Some("post".into()),
            before: None,
            after: Some(1_700_000_000_000),
            limit: Some(50),
            offset: Some(10),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn request_round_trips() {
        let req = sample_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SearchQueryRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn request_minimal_round_trips() {
        // Only the required query; all options absent.
        let req = SearchQueryRequest {
            query: "q".into(),
            content_type: None,
            before: None,
            after: None,
            limit: None,
            offset: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SearchQueryRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn request_canonical_re_encodes_identically() {
        let req = sample_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: SearchQueryRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn reply_round_trips() {
        let reply = SearchQueryReply {
            results: vec![
                SearchResult {
                    content_type: "post".into(),
                    content_id: "abcd".into(),
                    created_at: 1_700_000_000_000,
                    rank: 12_500_000,
                    snippet: "…hello <b>world</b>…".into(),
                    extra: Default::default(),
                },
                SearchResult {
                    content_type: "profile".into(),
                    content_id: "ef01".into(),
                    created_at: 1_699_000_000_000,
                    rank: 3_000_000,
                    snippet: "…".into(),
                    extra: Default::default(),
                },
            ],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SearchQueryReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn empty_reply_round_trips() {
        let reply = SearchQueryReply {
            results: vec![],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SearchQueryReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }
}
