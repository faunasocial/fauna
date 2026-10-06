//! The **local arm's seam** — how the manager reaches backend 2 (the sealed
//! per-user tantivy replica) without this crate ever learning what tantivy is.
//!
//! # Why a seam and not a dependency
//!
//! `docs/goal/ui/search.md` § State & data shape requires the local arm to be
//! **off on wasm** ("no browser Tantivy … the web SPA's manager runs nest-only,
//! *structurally*"). A trait seam is what makes that structural rather than
//! conditional: `fauna-client-search` depends only on `fauna-protocol` +
//! `fauna-core`, so the web SPA's WASM graph cannot contain an index engine
//! even by accident — there is no `cfg` a future edit could get wrong. It is the
//! same seam pattern `fauna-conversations` uses to keep the transport out
//! (`SchedulingSink`, `InboundMailSource`, `IndexBuilderLauncher`).
//!
//! A native app registers an implementation; the SPA registers none, and a
//! manager with no local index simply has one arm (§ *no local reader* is a
//! normal state, rendered as no local rows — never an error).
//!
//! # Why the hits arrive already projected
//!
//! The seam yields display-ready [`LocalSearchHit`]s, not raw index hits. A
//! local row's snippet must be rendered from **locally-held content**, because
//! the sealed index stores postings only and never the source text
//! (`content-index.md` § Don't do these), and its navigation target must be
//! resolved from the content id. Both of those need the app's content stores —
//! knowledge that belongs on the native side of the seam, not in the shared
//! merge. So the implementation resolves them and the manager merges rows that
//! are already rows.

use crate::kind::SearchKindClass;
use crate::snapshot::SearchNav;

/// One already-projected hit from the local sealed index.
///
/// The local counterpart of a wire `fauna_protocol::search::SearchResult`; the
/// manager turns both into the same [`SearchResultRow`](crate::SearchResultRow).
#[derive(Clone, Debug, PartialEq)]
pub struct LocalSearchHit {
    /// The producer-owned real id the sealed index stored (for mail, the RFC
    /// `Message-ID`). Half of the merge's dedup key.
    pub content_id: String,
    /// The raw type string, in the same vocabulary the nest uses — so a local
    /// row and its nest twin classify identically and can dedup
    /// (`crate::kind::kind_class`).
    pub content_type: String,
    /// Rendered from locally-held content by the implementation.
    pub snippet: String,
    /// Epoch **millis** (the index stores nanos; the implementation divides).
    pub timestamp: i64,
    /// Tantivy's BM25 score — the same statistic family as the nest's
    /// `rank / 1e6`, which is what makes one merged ordering meaningful
    /// (`search.md` § The local/nest merge).
    pub score: f32,
    /// Resolved by the implementation, which has the content stores to do it.
    /// `None` renders inert, exactly like a non-navigable nest row.
    pub navigation: Option<SearchNav>,
}

/// The local search backend, as the manager sees it.
///
/// Async so an implementation can move a blocking index query off the reactor
/// (tantivy is blocking) or refresh its replica first, without the manager
/// knowing either happened.
#[async_trait::async_trait]
pub trait LocalSearchIndex: Send + Sync {
    /// Query the classes named, newest-and-most-relevant first, capped at
    /// `limit`.
    ///
    /// `kinds` is never empty — the manager skips the arm entirely when the
    /// active filter selects no local class (`crate::kind::local_kind_classes`),
    /// so an implementation never has to answer a question with a guaranteed
    /// empty answer.
    ///
    /// `Err` is for a **real** failure (the replica could not be opened, the
    /// query could not be parsed). "This device has no index yet" is not one:
    /// that is `Ok(vec![])`, because a fresh device having published nothing is
    /// a normal state and must render as *no local results*, never as a page
    /// error (`content-index.md` § Where queries run).
    async fn query(
        &self,
        query: &str,
        kinds: &[SearchKindClass],
        limit: usize,
    ) -> Result<Vec<LocalSearchHit>, String>;

    /// `true` when this arm was minted **without** a reader a later
    /// precondition would add — a master-only arm minted before mail is
    /// enabled has no mail reader and claims no `Contact` kind, because both
    /// ride the MSEK. The manager keeps such an arm (its classes are real) but
    /// re-asks its [`LocalIndexResolver`] on every query while it holds one, so
    /// the precondition arriving later completes the arm instead of the page
    /// answering without those kinds for the life of the process
    /// (`content-index.md` § Ingest triggers, v1 → *An arm attaches when its
    /// precondition arrives*). A complete arm answers `false` and is never
    /// re-resolved.
    fn awaits_precondition(&self) -> bool {
        false
    }
}

/// The local index's ear at the posts **trickle chokepoint** — called after the
/// nest confirms a `fauna.posts.create`, so the just-composed post is staged at
/// once instead of waiting for the next reconcile walk
/// (`content-index.md` § Ingest triggers, v1 → the posts ruling: *the client's
/// own `posts_create` is the trickle chokepoint — index at create, ungated by
/// the lease exactly as every trickle is*).
///
/// A seam here, in the engine-free vocabulary crate, because the two surfaces
/// that own creates and the object that owns the builder live in crates with no
/// clean edge between them: `PostsClient` itself is a stateless per-call mint
/// (~a dozen sites), so the hook rides the types that own the create *flows* —
/// `fauna-feed`'s composer and `fauna-ffi`'s posts façade — while the
/// implementation lives beside the index launcher. Both already speak this
/// crate. The wasm SPA never wires it: web is a querier by ratified design.
///
/// Fire-and-forget and non-blocking; an implementation with no builder attached
/// yet (phones, a not-yet-resumed arm) drops the call silently — the walk is
/// the correctness carrier, so a dropped trickle costs freshness only.
pub trait OwnPostIndexObserver: Send + Sync {
    /// `post_id_hex` is the nest-echoed hex-lowercase post digest;
    /// `body_text` is `fauna_core::data::Post::body_text()` of the **bytes that
    /// were sent** — the same extraction the nest's own enumeration rows carry,
    /// so the trickle and the walk can never disagree about a post's text.
    fn own_post_created(&self, post_id_hex: &str, body_text: &str);
}

/// Mints the local arm once its precondition exists — the **query-side twin** of
/// the build side's `IndexBuilderLauncher::ensure_arm`.
///
/// # Why registration cannot be a one-shot
///
/// Opening the sealed replica needs the actor's MSEK, which does not exist until
/// mail is enabled. App glue registers at login, so a user who enables mail
/// *afterwards* had nothing to register: the arm resolved to `None` once and the
/// Search page stayed nest-only for the life of the process, even after the
/// builder started publishing that session's mail. That is the same one-shot
/// shape the build side had, arriving at the other end of the pipeline — and it
/// is why closing only the build half left
/// `tests/e2e-unified/tests/test_search_local_index.py` red: the mail *was*
/// indexed, and the page had no arm to ask (`content-index.md` § Ingest
/// triggers, v1 → *An arm attaches when its precondition arrives*).
///
/// So glue registers a **resolver** instead of an arm, and the manager asks it
/// on any query made while it still has no arm — caching the first `Some`.
/// `None` stays a normal answer, re-asked on the next query, which costs a
/// resolver call only on a page that has no local arm anyway.
#[async_trait::async_trait]
pub trait LocalIndexResolver: Send + Sync {
    /// The local arm, or `None` while its precondition is still absent (mail
    /// not enabled, replica unreadable). Never an error: *no local arm* renders
    /// as no local rows, exactly as a device that has published nothing.
    async fn resolve(&self) -> Option<std::sync::Arc<dyn LocalSearchIndex>>;
}

/// One local arm over several key-class readers — the query-side counterpart of
/// the build side's fan-out observer.
///
/// # Why this exists
///
/// The per-kind key split gives the index **two** classes with two manifests and
/// two keys (`content-index.md` § Encryption posture), so a device that can
/// search everything it holds needs two readers. The manager, though, holds
/// exactly one arm slot — deliberately, because *which* readers exist is a
/// native-side fact the engine-free manager must not learn. Composing them
/// behind one `LocalSearchIndex` is what reconciles the two: glue registers one
/// arm, and the classes stay separate types holding separate keys behind it.
///
/// This mirrors `FanOutObserver` on the build side, where one seam slot feeds N
/// per-class builders. Build fans out; query fans in.
///
/// # Ordering and failure
///
/// Hits are concatenated in member order and left unsorted: the manager already
/// sorts the merged local+nest rows by score and dedups by `(class, id)`
/// (`search.md` § The local/nest merge), so imposing an order here would be
/// thrown away. A member that errors **fails the whole arm**, exactly as a
/// single reader would — a partial answer silently missing one class's rows is
/// indistinguishable to the user from that content not existing, which is the
/// failure mode this whole slice exists to remove. A member with nothing
/// published is not an error: it answers no rows, and the arm still serves the
/// others.
pub struct CompositeLocalSearch {
    members: Vec<std::sync::Arc<dyn LocalSearchIndex>>,
    awaits_precondition: bool,
}

impl CompositeLocalSearch {
    pub fn new(members: Vec<std::sync::Arc<dyn LocalSearchIndex>>) -> Self {
        Self {
            members,
            awaits_precondition: false,
        }
    }

    /// Mark the arm as minted without a reader a later precondition would add
    /// (the glue knows which: the mail reader and the `Contact` claim wait on
    /// the MSEK) — see [`LocalSearchIndex::awaits_precondition`].
    pub fn awaiting_precondition(mut self) -> Self {
        self.awaits_precondition = true;
        self
    }
}

#[async_trait::async_trait]
impl LocalSearchIndex for CompositeLocalSearch {
    async fn query(
        &self,
        query: &str,
        kinds: &[SearchKindClass],
        limit: usize,
    ) -> Result<Vec<LocalSearchHit>, String> {
        let mut all = Vec::new();
        for member in &self.members {
            // `limit` is passed to each member rather than divided: the classes
            // are disjoint, so a member's cap must not depend on how many other
            // classes happen to be registered, and the manager caps the merged
            // list anyway. Dividing would make a mail-only query return fewer
            // rows on a device that also has a master reader.
            all.extend(member.query(query, kinds, limit).await?);
        }
        Ok(all)
    }

    fn awaits_precondition(&self) -> bool {
        self.awaits_precondition
    }
}

#[cfg(test)]
mod composite_tests {
    use super::*;
    use std::sync::Arc;

    struct Member {
        rows: Vec<&'static str>,
        fail: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl LocalSearchIndex for Member {
        async fn query(
            &self,
            _query: &str,
            _kinds: &[SearchKindClass],
            _limit: usize,
        ) -> Result<Vec<LocalSearchHit>, String> {
            if let Some(e) = self.fail {
                return Err(e.to_string());
            }
            Ok(self
                .rows
                .iter()
                .map(|id| LocalSearchHit {
                    content_id: (*id).to_string(),
                    content_type: "mail".into(),
                    snippet: String::new(),
                    timestamp: 0,
                    score: 1.0,
                    navigation: None,
                })
                .collect())
        }
    }

    fn member(rows: &[&'static str]) -> Arc<dyn LocalSearchIndex> {
        Arc::new(Member {
            rows: rows.to_vec(),
            fail: None,
        })
    }

    /// The whole point of the composite: one arm slot, every class's rows. Before
    /// it, the slot held the mail reader alone and master-class content was
    /// indexed but unreachable.
    #[tokio::test]
    async fn the_arm_returns_the_union_of_every_class_it_composes() {
        let c = CompositeLocalSearch::new(vec![member(&["mail-1"]), member(&["conv-1", "conv-2"])]);

        let rows = c.query("q", &[SearchKindClass::Mail], 10).await.unwrap();

        let ids: Vec<&str> = rows.iter().map(|r| r.content_id.as_str()).collect();
        assert_eq!(ids, vec!["mail-1", "conv-1", "conv-2"]);
    }

    /// A member that fails fails the arm. A partial answer that silently omits
    /// one class reads to the user exactly like that content not existing —
    /// which is the failure this slice was built to remove, so it must not be
    /// reintroduced as a "graceful" degradation.
    #[tokio::test]
    async fn one_members_failure_fails_the_arm_rather_than_hiding_a_class() {
        let c = CompositeLocalSearch::new(vec![
            member(&["mail-1"]),
            Arc::new(Member {
                rows: vec![],
                fail: Some("replica unreadable"),
            }),
        ]);

        let err = c
            .query("q", &[SearchKindClass::Mail], 10)
            .await
            .expect_err("a member failure must surface");
        assert!(err.contains("replica unreadable"));
    }

    /// A class with nothing published is not a failure — it contributes no rows
    /// and the arm still serves the others. This is the fresh-device state.
    #[tokio::test]
    async fn an_empty_member_is_a_normal_state_not_an_error() {
        let c = CompositeLocalSearch::new(vec![member(&[]), member(&["conv-1"])]);

        let rows = c.query("q", &[SearchKindClass::Mail], 10).await.unwrap();

        assert_eq!(rows.len(), 1);
    }

    /// An arm is complete unless the glue says a later precondition would add
    /// a reader to it — the manager re-resolves exactly the arms that say so.
    #[test]
    fn an_arm_awaits_its_precondition_only_when_marked() {
        assert!(!CompositeLocalSearch::new(vec![member(&[])]).awaits_precondition());
        assert!(
            CompositeLocalSearch::new(vec![member(&[])])
                .awaiting_precondition()
                .awaits_precondition()
        );
    }
}
