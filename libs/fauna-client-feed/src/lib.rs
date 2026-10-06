//! Typed-call wrapper for the user-facing `fauna.feed.*` WS-RPC kinds — the
//! feed-CRUD + feed-query + discovery-contributor plane clients hit from the
//! feed-management + feed-render affordances. Part of the
//! WS-RPC-everywhere migration (tracked internally).
//!
//! Pattern: same shape as `fauna-client-bridges` / `fauna-client-posts` — a
//! thin `pub struct FeedClient<R: RpcRequester> { nest: R }`, one async method
//! per kind, no state machine, generic over the WS-RPC transport so the
//! kind-composition logic is written once and shared across native + wasm
//! (priority #2). This crate is the transport surface only.
//!
//! Feed `rules` ride typed — `Vec<fauna_core::scoring::FilterRule>`, built from
//! the create-feed form by the [`encode_filter_rules`] codec below
//! (`docs/goal/ui/feed.md` § Where logic lives). Post scores ride as `i64`
//! micro-units (×1e6); see `fauna_protocol::feed`.

use fauna_core::scoring::FilterRule;
use fauna_protocol::RpcRequester;
use fauna_protocol::feed::{
    FeedContributorGrantReply, FeedContributorGrantRequest, FeedContributorRevokeReply,
    FeedContributorRevokeRequest, FeedContributorsListReply, FeedContributorsListRequest,
    FeedCreateReply, FeedCreateRequest, FeedDeleteReply, FeedDeleteRequest, FeedFactorsGetReply,
    FeedFactorsGetRequest, FeedFactorsSetReply, FeedFactorsSetRequest, FeedGetReply,
    FeedGetRequest, FeedListReply, FeedListRequest, FeedLocalPostsReply, FeedLocalPostsRequest,
    FeedPostsReply, FeedPostsRequest, FeedTrendingPostsReply, FeedTrendingPostsRequest,
    FeedUpdateReply, FeedUpdateRequest,
};

pub use fauna_protocol::feed;

/// Feed-rule codec — the create-feed form's `(type, value, required)` triple ⇄
/// the typed `FilterRule` the wire carries. Shared by every native app
/// (Linux calls it directly; the UniFFI apps reach it through the thin
/// `#[uniffi::export]` wrappers in `fauna-ffi`). `decode_filter_rules` is the
/// inverse of `encode_filter_rules` — `fauna.feed.get`'s `rules` → editable
/// triples.
mod encoder;
pub use encoder::{
    DEFAULT_RULE_THRESHOLD, DecodedFilterRule, RuleInputKind, RuleTypeOption, can_add_rule,
    decode_filter_rule, decode_filter_rules, encode_filter_rule, encode_filter_rules,
    rule_required_label, rule_summary_label, rule_type_options,
};

/// The create-feed form's built-in ranking factors — the head of every app's
/// `feed-factor-select` list (`engagement`, `trending`), ahead of the caller's
/// labeler and trained-topic factors.
mod factor_options;
pub use factor_options::{FactorOption, builtin_factor_option, builtin_factor_options};

/// Typed `fauna.feed.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm SPA
/// passes its `WsRpcClient`. Errors propagate as the transport's `R::Error`
/// (native `NestClientError`, wasm rpc-wasm error).
pub struct FeedClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> FeedClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.feed.list` — list all feeds (summary view, no rules). Pure
    /// read; replay-safe at 5 s.
    pub async fn feed_list(&self) -> Result<FeedListReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.list",
                FeedListRequest {
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.create` — create a feed. `rules` is the typed filter-rule
    /// list (see the module doc). `scope` is
    /// `"local"` (default) or `"discovery"`; `contributor_seeds` apply only to
    /// discovery feeds. `composition` is the feed's typed factor-weight set
    /// (frame § Composition; `None` = no composition). Echoes the generated
    /// `feed_id`.
    pub async fn feed_create(
        &self,
        name: impl Into<String>,
        rules: Vec<FilterRule>,
        combination: impl Into<String>,
        scope: Option<String>,
        contributor_seeds: Option<Vec<String>>,
        composition: Option<Vec<feed::FeedCompositionEntry>>,
    ) -> Result<FeedCreateReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.create",
                FeedCreateRequest {
                    name: name.into(),
                    rules,
                    combination: combination.into(),
                    scope,
                    contributor_seeds,
                    composition,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.get` — fetch a single feed including its typed `rules`.
    /// Pure read; replay-safe at 5 s.
    pub async fn feed_get(&self, feed_id: impl Into<String>) -> Result<FeedGetReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.get",
                FeedGetRequest {
                    feed_id: feed_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.update` — overwrite a feed's `name` / `rules` /
    /// `combination` (scope + seeds are immutable on update). Idempotent
    /// owner-keyed overwrite. `composition` is tri-state (the wire contract):
    /// `None` = leave the stored composition unchanged, `Some(vec![])` =
    /// explicitly clear it, `Some(entries)` = overwrite it.
    pub async fn feed_update(
        &self,
        feed_id: impl Into<String>,
        name: impl Into<String>,
        rules: Vec<FilterRule>,
        combination: impl Into<String>,
        composition: Option<Vec<feed::FeedCompositionEntry>>,
    ) -> Result<FeedUpdateReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.update",
                FeedUpdateRequest {
                    feed_id: feed_id.into(),
                    name: name.into(),
                    rules,
                    combination: combination.into(),
                    composition,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.factors.get` — read the caller's global factor set (the
    /// entries folded into every one of their feeds' composed `order=score`
    /// orderings — frame § Composition). Pure read; replay-safe at 5 s.
    pub async fn feed_factors_get(&self) -> Result<FeedFactorsGetReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.factors.get",
                FeedFactorsGetRequest {
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.factors.set` — replace the caller's global factor set
    /// (idempotent whole-set overwrite; empty clears).
    pub async fn feed_factors_set(
        &self,
        factors: Vec<feed::FeedCompositionEntry>,
    ) -> Result<FeedFactorsSetReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.factors.set",
                FeedFactorsSetRequest {
                    factors,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.delete` — delete an owned feed.
    pub async fn feed_delete(
        &self,
        feed_id: impl Into<String>,
    ) -> Result<FeedDeleteReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.delete",
                FeedDeleteRequest {
                    feed_id: feed_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.posts` — query a feed's posts. When `order ==
    /// Some("score")`, results are score-ordered and paginated via the keyset
    /// pair `(score_cursor, score_cursor_created_at)` — the ordering key of the
    /// previous page's last row **and that same row's `created_at`**, both
    /// echoed straight back from the reply. Pass both or neither: the tiebreak
    /// is what stops rows tied on the key from being skipped, and it is what
    /// lets a feed whose composition is entirely *sealed* factors paginate at
    /// all (every nest-side key is 0 there, so a key-only cursor never
    /// advances). Otherwise chronological via `cursor`. Optional
    /// comma-separated `search` terms append a `BodyContains` filter.
    // One arg over clippy's threshold: this is a 1:1 transport mirror of
    // `FeedPostsRequest`'s fields, and the crate's contract is exactly that
    // (one thin method per kind, no state) — collapsing the params into a
    // struct here would put a second, divergable shape between the caller and
    // the wire type.
    #[allow(clippy::too_many_arguments)]
    pub async fn feed_posts(
        &self,
        feed_id: impl Into<String>,
        cursor: Option<i64>,
        limit: Option<i64>,
        order: Option<String>,
        score_cursor: Option<i64>,
        score_cursor_created_at: Option<i64>,
        search: Option<String>,
    ) -> Result<FeedPostsReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.posts",
                FeedPostsRequest {
                    feed_id: feed_id.into(),
                    cursor,
                    limit,
                    order,
                    score_cursor,
                    score_cursor_created_at,
                    search,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.local.posts` — query this nest's local feed (always
    /// chronological).
    pub async fn feed_local_posts(
        &self,
        cursor: Option<i64>,
        limit: Option<i64>,
        search: Option<String>,
    ) -> Result<FeedLocalPostsReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.local.posts",
                FeedLocalPostsRequest {
                    cursor,
                    limit,
                    search,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.trending.posts` — query the built-in **Trending** virtual
    /// feed (`trending.md` § The Trending feed): the *scored* sibling of
    /// `fauna.feed.local.posts` over **public posts only**, ordered by the
    /// implicit composition `[(trending, 1000)]` plus the caller's global factor
    /// set. Always score-ordered — no `order`/chronological `cursor` — so it
    /// paginates on the same keyset pair `(score_cursor, score_cursor_created_at)`
    /// as `feed_posts`'s `order=score` branch. Pass both cursor halves or neither
    /// (the tiebreak is what stops rows tied on the key from being skipped).
    pub async fn feed_trending_posts(
        &self,
        limit: Option<i64>,
        score_cursor: Option<i64>,
        score_cursor_created_at: Option<i64>,
        search: Option<String>,
    ) -> Result<FeedTrendingPostsReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.trending.posts",
                FeedTrendingPostsRequest {
                    limit,
                    score_cursor,
                    score_cursor_created_at,
                    search,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.contributors.list` — list a discovery feed's contributors
    /// (owner-only).
    pub async fn feed_contributors_list(
        &self,
        feed_id: impl Into<String>,
    ) -> Result<FeedContributorsListReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.contributors.list",
                FeedContributorsListRequest {
                    feed_id: feed_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.contributors.grant` — add a contributor to a discovery
    /// feed. `added=false` with a `reason` means it already existed.
    pub async fn feed_contributors_grant(
        &self,
        feed_id: impl Into<String>,
        nest_url: impl Into<String>,
        author_id: Option<String>,
    ) -> Result<FeedContributorGrantReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.contributors.grant",
                FeedContributorGrantRequest {
                    feed_id: feed_id.into(),
                    nest_url: nest_url.into(),
                    author_id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.feed.contributors.revoke` — remove a contributor from a
    /// discovery feed. A missing contributor surfaces as a
    /// `fauna.feed.not_found` `RpcError`.
    pub async fn feed_contributors_revoke(
        &self,
        feed_id: impl Into<String>,
        nest_url: impl Into<String>,
        author_id: Option<String>,
    ) -> Result<FeedContributorRevokeReply, R::Error> {
        self.nest
            .request(
                "fauna.feed.contributors.revoke",
                FeedContributorRevokeRequest {
                    feed_id: feed_id.into(),
                    nest_url: nest_url.into(),
                    author_id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = FeedClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The construction-only `MockRequester` above can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: each `FeedClient` method must send
    // its exact `fauna.feed.*` kind and a payload that round-trips back to the
    // typed request. No nest-side conformance test routes through this adapter
    // (they use literal kind strings), so this is the only Rust-layer guard
    // against an adapter-method kind rename. The pattern mirrors
    // `fauna-client-events` / `-conversations`'s `RecordingRequester`
    // (transport-free, so it runs on every target including wasm); real
    // end-to-end round-trip conformance lives in `conformance_feed.rs`.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        use feed::*;
        match kind {
            "fauna.feed.list" => fauna_protocol::encode_canonical(&FeedListReply {
                feeds: vec![],
                extra: Default::default(),
            }),
            "fauna.feed.create" => fauna_protocol::encode_canonical(&FeedCreateReply {
                feed_id: "0011223344556677".into(),
                extra: Default::default(),
            }),
            "fauna.feed.get" => fauna_protocol::encode_canonical(&FeedGetReply {
                feed_id: "0011223344556677".into(),
                owner: "ab".repeat(32),
                name: "Home".into(),
                rules: vec![],
                combination: "all".into(),
                created_at: 0,
                scope: "local".into(),
                contributor_seeds: vec![],
                ..Default::default()
            }),
            "fauna.feed.update" => fauna_protocol::encode_canonical(&FeedUpdateReply {
                extra: Default::default(),
            }),
            "fauna.feed.delete" => fauna_protocol::encode_canonical(&FeedDeleteReply {
                extra: Default::default(),
            }),
            "fauna.feed.posts" => fauna_protocol::encode_canonical(&FeedPostsReply {
                posts: vec![],
                cursor: None,
                score_cursor: None,
                score_cursor_created_at: None,
                extra: Default::default(),
            }),
            "fauna.feed.local.posts" => fauna_protocol::encode_canonical(&FeedLocalPostsReply {
                posts: vec![],
                cursor: None,
                extra: Default::default(),
            }),
            "fauna.feed.trending.posts" => {
                fauna_protocol::encode_canonical(&FeedTrendingPostsReply {
                    posts: vec![],
                    score_cursor: None,
                    score_cursor_created_at: None,
                    extra: Default::default(),
                })
            }
            "fauna.feed.contributors.list" => {
                fauna_protocol::encode_canonical(&FeedContributorsListReply {
                    contributors: vec![],
                    extra: Default::default(),
                })
            }
            "fauna.feed.contributors.grant" => {
                fauna_protocol::encode_canonical(&FeedContributorGrantReply {
                    added: true,
                    reason: None,
                    extra: Default::default(),
                })
            }
            "fauna.feed.contributors.revoke" => {
                fauna_protocol::encode_canonical(&FeedContributorRevokeReply {
                    extra: Default::default(),
                })
            }
            "fauna.feed.factors.get" => fauna_protocol::encode_canonical(&FeedFactorsGetReply {
                factors: vec![],
                extra: Default::default(),
            }),
            "fauna.feed.factors.set" => fauna_protocol::encode_canonical(&FeedFactorsSetReply {
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    /// 64-char hex `feed_id` (`feed_id` / `author_id` are hex strings on this
    /// surface).
    fn hex32() -> String {
        "ab".repeat(32)
    }

    fn client() -> (
        std::sync::Arc<RecordingRequester>,
        FeedClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = FeedClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn feed_list_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.list");
        let _req: feed::FeedListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn feed_create_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_create(
            "My Feed",
            vec![FilterRule::HasHashtag {
                tags: vec!["rust".into()],
            }],
            "all",
            Some("discovery".into()),
            Some(vec!["https://peer.example".into()]),
            None,
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.create");
        let req: feed::FeedCreateRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.name, "My Feed");
        assert_eq!(
            req.rules,
            vec![FilterRule::HasHashtag {
                tags: vec!["rust".into()]
            }]
        );
        assert_eq!(req.combination, "all");
        assert_eq!(req.scope.as_deref(), Some("discovery"));
        assert_eq!(
            req.contributor_seeds.as_deref(),
            Some(&["https://peer.example".to_string()][..])
        );
        assert_eq!(req.composition, None);
    }

    #[test]
    fn feed_create_carries_composition_when_supplied() {
        let (rec, c) = client();
        block_on(c.feed_create(
            "Cats",
            vec![],
            "all",
            None,
            None,
            Some(vec![feed::FeedCompositionEntry {
                factor: "labeler:aabb".into(),
                weight_permille: 2000,
                extra: Default::default(),
            }]),
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.create");
        let req: feed::FeedCreateRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.composition.as_deref(),
            Some(
                &[feed::FeedCompositionEntry {
                    factor: "labeler:aabb".into(),
                    weight_permille: 2000,
                    extra: Default::default(),
                }][..]
            )
        );
    }

    #[test]
    fn feed_get_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_get(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.get");
        let req: feed::FeedGetRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.feed_id, hex32());
    }

    #[test]
    fn feed_update_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_update(hex32(), "Renamed", vec![], "any", None)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.update");
        let req: feed::FeedUpdateRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.feed_id, hex32());
        assert_eq!(req.name, "Renamed");
        assert_eq!(req.combination, "any");
        assert_eq!(req.composition, None);
    }

    #[test]
    fn feed_update_carries_explicit_clear() {
        let (rec, c) = client();
        block_on(c.feed_update(hex32(), "Renamed", vec![], "any", Some(vec![])))
            .expect("infallible mock");
        let (_kind, payload) = rec.recorded();
        let req: feed::FeedUpdateRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.composition, Some(vec![]));
    }

    #[test]
    fn feed_factors_get_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_factors_get()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.factors.get");
        let _req: feed::FeedFactorsGetRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn feed_factors_set_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_factors_set(vec![feed::FeedCompositionEntry {
            factor: "engagement".into(),
            weight_permille: 1500,
            extra: Default::default(),
        }]))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.factors.set");
        let req: feed::FeedFactorsSetRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.factors.len(), 1);
        assert_eq!(req.factors[0].factor, "engagement");
        assert_eq!(req.factors[0].weight_permille, 1500);
    }

    #[test]
    fn feed_delete_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_delete(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.delete");
        let req: feed::FeedDeleteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.feed_id, hex32());
    }

    #[test]
    fn feed_posts_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_posts(
            hex32(),
            None,
            Some(50),
            Some("score".into()),
            Some(3_000_000),
            Some(1_700_000_000_000_000),
            Some("rust,fauna".into()),
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.posts");
        let req: feed::FeedPostsRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.feed_id, hex32());
        assert_eq!(req.limit, Some(50));
        assert_eq!(req.order.as_deref(), Some("score"));
        assert_eq!(req.score_cursor, Some(3_000_000));
        // The keyset cursor's tiebreak half rides beside the key (both or
        // neither — see `feed_posts`).
        assert_eq!(req.score_cursor_created_at, Some(1_700_000_000_000_000));
        assert_eq!(req.search.as_deref(), Some("rust,fauna"));
    }

    #[test]
    fn feed_local_posts_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_local_posts(Some(1_700_000_000_000_000), Some(25), None))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.local.posts");
        let req: feed::FeedLocalPostsRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.cursor, Some(1_700_000_000_000_000));
        assert_eq!(req.limit, Some(25));
        assert_eq!(req.search, None);
    }

    #[test]
    fn feed_trending_posts_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_trending_posts(
            Some(50),
            Some(3_000_000),
            Some(1_700_000_000_000_000),
            Some("rust,fauna".into()),
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.trending.posts");
        let req: feed::FeedTrendingPostsRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        // No feed_id / order — the trending read is always score-ordered over
        // public posts; it carries only the keyset cursor pair + search.
        assert_eq!(req.limit, Some(50));
        assert_eq!(req.score_cursor, Some(3_000_000));
        assert_eq!(req.score_cursor_created_at, Some(1_700_000_000_000_000));
        assert_eq!(req.search.as_deref(), Some("rust,fauna"));
    }

    #[test]
    fn feed_contributors_list_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_contributors_list(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.contributors.list");
        let req: feed::FeedContributorsListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.feed_id, hex32());
    }

    #[test]
    fn feed_contributors_grant_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_contributors_grant(hex32(), "https://peer.example", Some("cd".repeat(32))))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.contributors.grant");
        let req: feed::FeedContributorGrantRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.feed_id, hex32());
        assert_eq!(req.nest_url, "https://peer.example");
        assert_eq!(req.author_id.as_deref(), Some("cd".repeat(32).as_str()));
    }

    #[test]
    fn feed_contributors_revoke_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.feed_contributors_revoke(hex32(), "https://peer.example", None))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.feed.contributors.revoke");
        let req: feed::FeedContributorRevokeRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.feed_id, hex32());
        assert_eq!(req.nest_url, "https://peer.example");
        assert_eq!(req.author_id, None);
    }
}
