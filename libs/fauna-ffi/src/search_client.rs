//! UniFFI façade for the `fauna.search.query` kind — the user-facing full-text
//! search surface clients hit from the search bar.
//!
//! [`FfiSearchClient`] wraps `fauna_client_search::SearchClient` (which wraps
//! the shared `NestClient`); it is the native-client twin of the wire query the
//! Rust-native Linux app calls `SearchClient` for directly — letting Apple /
//! Windows / Android reach `fauna.search.query` over WS-RPC instead of the
//! legacy `GET /api/v1/search` HTTP twin. No search logic client-side
//! (priority #2) — the nest ranks and snippets; this just composes the request
//! and maps the reply rows.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_search::SearchClient;
use fauna_client_search::search::{SearchQueryRequest, SearchResult};

use crate::{FfiError, stringify};

/// FFI mirror of [`fauna_protocol::search::SearchResult`] — one search hit. The
/// protocol type's forward-compat `extra` overflow has no counterpart here
/// (clients only consume the named fields). `rank` is fixed-point micro-units
/// (higher = more relevant; the dag-cbor wire forbids floats), `created_at` is
/// epoch micros.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSearchResult {
    pub content_type: String,
    pub content_id: String,
    pub created_at: i64,
    pub rank: i64,
    pub snippet: String,
}

impl From<SearchResult> for FfiSearchResult {
    fn from(r: SearchResult) -> Self {
        FfiSearchResult {
            content_type: r.content_type,
            content_id: r.content_id,
            created_at: r.created_at,
            rank: r.rank,
            snippet: r.snippet,
        }
    }
}

/// UniFFI handle for the `fauna.search.query` kind. Construct via
/// [`crate::nest_client::FfiNestClient::search`]; the method is exposed to Swift
/// as `async throws` and Kotlin as `suspend fun`. Thin wrapper over the shared
/// `fauna_client_search::SearchClient`.
#[derive(uniffi::Object)]
pub struct FfiSearchClient {
    nest: Arc<NestClient>,
}

impl FfiSearchClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> SearchClient<Arc<NestClient>> {
        SearchClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiSearchClient {
    /// `fauna.search.query` — full-text search scoped to the calling actor (the
    /// connection knows its caller). Each `None` rides the handler default
    /// (`content_type` = all, `limit` = 20 clamped to `[1, 100]`, `offset` = 0;
    /// `before`/`after` are epoch-micros cursors). Returns the result hits (the
    /// reply's forward-compat `extra` overflow is dropped).
    pub async fn query(
        &self,
        query: String,
        content_type: Option<String>,
        before: Option<i64>,
        after: Option<i64>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<FfiSearchResult>, FfiError> {
        let reply = self
            .client()
            .query(SearchQueryRequest {
                query,
                content_type,
                before,
                after,
                limit,
                offset,
                extra: Default::default(),
            })
            .await
            .map_err(stringify)?;
        Ok(reply.results.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_result_mirror_maps_all_named_fields() {
        let proto = SearchResult {
            content_type: "feed_post".into(),
            content_id: "abc123".into(),
            created_at: 1_700_000_000_000,
            rank: -42,
            snippet: "a <b>match</b>".into(),
            extra: Default::default(),
        };
        let ffi: FfiSearchResult = proto.into();
        assert_eq!(
            ffi,
            FfiSearchResult {
                content_type: "feed_post".into(),
                content_id: "abc123".into(),
                created_at: 1_700_000_000_000,
                rank: -42,
                snippet: "a <b>match</b>".into(),
            }
        );
    }
}
