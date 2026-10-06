//! Typed-call wrapper for the full-text search WS-RPC kind —
//! `fauna.search.query`. The user-facing search surface clients hit from
//! the search page (query + content-type filter + pagination).
//!
//! Faithful transport migration of `GET /api/v1/search` (tracked
//! internally — client seam + linux migration). The HTTP twin is **deleted**:
//! `fauna.search.query` is the sole search surface (`search.md` § The page's
//! wire surface).
//!
//! Pattern: same shape as the sibling per-feature client wrappers
//! (`fauna-client-bridges`, `-conversations`, `-subscriptions`) — a thin
//! `pub struct SearchClient { nest: R }`, one async method per kind, no
//! state machine. Generic over the WS-RPC transport (`R: RpcRequester`):
//! native call sites pass `Arc<NestClient>`, the wasm SPA passes its
//! `WsRpcClient`. The kind-composition logic is written once here and
//! shared across native + wasm (priority #2).

// Defines this crate's `UniFfiTag`, without which the feature-gated
// `uniffi::Record`/`Enum` derives on the snapshot types (and the `with_foreign`
// observer trait) have no namespace to register into. Mirrors
// `fauna_feed`/`fauna_core`; the concrete async export still lives on
// `fauna-ffi`'s façade, because the generic `SearchManager<R>` can't be
// `#[uniffi::export]`ed.
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_search");

use fauna_protocol::RpcRequester;
use fauna_protocol::search::{SearchQueryReply, SearchQueryRequest};

pub use fauna_protocol::search;

/// Canonical search-result card formatters (snippet cleanup + `content_type →
/// badge` map), shared so every app renders identically. See [`render`].
pub mod render;

/// The Search page's cursor-less paging policy — initial page size, what
/// "load more" does, and when its button shows. See [`paging`].
pub mod paging;

/// The kind class — the one vocabulary both backends are read through, and the
/// single mapping the type filter applies to both arms. See [`kind`].
pub mod kind;

/// The local (sealed-index) arm's seam. See [`local`].
pub mod local;

/// The shared `SearchManager`. See [`manager`].
pub mod manager;

/// Snapshot reactivity callback. See [`observer`].
pub mod observer;

/// The typed read model the Search page renders. See [`snapshot`].
pub mod snapshot;

pub use kind::{SearchKindClass, TYPE_FILTER_ALL, TYPE_FILTER_OPTIONS, type_filter_label};
pub use local::{
    CompositeLocalSearch, LocalIndexResolver, LocalSearchHit, LocalSearchIndex,
    OwnPostIndexObserver,
};
pub use manager::SearchManager;
pub use observer::SearchSnapshotObserver;
pub use snapshot::{SearchNav, SearchResultRow, SearchSnapshot, SearchSource};

/// Typed `fauna.search.*` call surface. Errors propagate as the transport's
/// `R::Error` (native `NestClientError`, wasm rpc-wasm error); the namespaced
/// `RpcError`s the handler emits (`fauna.search.invalid_params`,
/// `fauna.search.permission_denied`) surface through that error channel.
///
/// The storage-mode-era `not_server_side` / `mode_not_configured` refusals
/// retired with the axis itself (no-modes) — `search_handlers.rs` constructs
/// neither.
pub struct SearchClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> SearchClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.search.query` — full-text search scoped to the calling actor
    /// (implicit — the connection knows its caller). Replay-safe pure read
    /// (`forbid_replay=false`, 5 s deadline per `register_search_kinds`). The
    /// caller builds the `SearchQueryRequest` (query + optional content-type
    /// filter + before/after/limit/offset pagination); the handler defaults
    /// `limit` to 20 (clamp 1–100) and `offset` to 0 when absent.
    pub async fn query(&self, req: SearchQueryRequest) -> Result<SearchQueryReply, R::Error> {
        self.nest.request("fauna.search.query", req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = SearchClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The construction-only `MockRequester` above can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: the `SearchClient::query` method
    // must send its exact `fauna.search.query` kind and a payload that
    // round-trips back to the typed request. No nest-side conformance test
    // routes through this adapter's literal kind string, so a kind rename here
    // would otherwise break it silently. The pattern mirrors
    // `fauna-client-events`'s `RecordingRequester` (transport-free, so it runs
    // on every target including wasm); real end-to-end round-trip conformance
    // lives in `bins/fauna-nest/tests/conformance_search.rs`.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes — one arm
        // per kind, each the minimal valid shape.
        match kind {
            "fauna.search.query" => fauna_protocol::encode_canonical(&SearchQueryReply {
                results: vec![],
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn query_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SearchClient::new(rec.clone());
        block_on(client.query(SearchQueryRequest {
            query: "hello world".into(),
            content_type: Some("post".into()),
            before: None,
            after: None,
            limit: Some(50),
            offset: None,
            extra: Default::default(),
        }))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.search.query");
        let req: SearchQueryRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.query, "hello world");
        assert_eq!(req.content_type, Some("post".into()));
        assert_eq!(req.limit, Some(50));
    }
}
