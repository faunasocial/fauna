//! Discovery feed poller: adaptive background polling for discovery-scoped feeds.
//!
//! The `DiscoveryPoller` maintains a priority queue of contributors (nest+author
//! pairs) and polls them at intervals determined by their priority tier (hot,
//! warm, cold). New authors are discovered via referral chains — when a matched
//! post references another author, we speculatively query that author and promote
//! them to a contributor if their content matches the feed's filter rules.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fauna_core::identity::ActorId;
use fauna_core::scoring::{FilterCombination, FilterRule};
use tokio::sync::mpsc;
use tokio::sync::watch;

use crate::db::CacheDb;
use crate::peer_query::query_peer_channel_first;
use crate::routes::{AppState, FeedEvent};

/// Configuration for the discovery poller.
pub struct PollConfig {
    pub hot_interval: Duration,
    pub warm_interval: Duration,
    pub cold_interval: Duration,
    pub priority_recalc_interval: Duration,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            hot_interval: Duration::from_secs(60),
            warm_interval: Duration::from_secs(300),
            cold_interval: Duration::from_secs(1800),
            priority_recalc_interval: Duration::from_secs(600),
        }
    }
}

/// Identifies a single contributor entry in the priority queue.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ContributorKey {
    feed_id: String,
    nest_url: String,
    author_id: Option<Vec<u8>>,
}

impl PartialOrd for ContributorKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ContributorKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.feed_id
            .cmp(&other.feed_id)
            .then_with(|| self.nest_url.cmp(&other.nest_url))
            .then_with(|| self.author_id.cmp(&other.author_id))
    }
}

/// Background polling task for discovery-scoped feeds.
pub struct DiscoveryPoller {
    db: Arc<CacheDb>,
    /// Full app state — the peer query goes channel-first (Spec Y2 slice 4 §6),
    /// which needs the `federation_pool` + `http_client` + `federation_registry`
    /// (HTTP fallback) it carries.
    state: Arc<AppState>,
    config: PollConfig,
    event_rx: mpsc::Receiver<FeedEvent>,
    shutdown: watch::Receiver<bool>,
    queue: BinaryHeap<Reverse<(Instant, ContributorKey)>>,
    active_feeds: HashSet<String>,
    /// Per-feed cache of authors whose content did not match the feed rules.
    /// Bounded to 1000 entries per feed; oldest entries are evicted.
    rejected_authors: HashMap<String, HashMap<Vec<u8>, Instant>>,
    /// Failure count per nest_url (for demotion / eviction).
    failure_counts: HashMap<String, u32>,
}

impl DiscoveryPoller {
    pub fn new(
        db: Arc<CacheDb>,
        state: Arc<AppState>,
        config: PollConfig,
        event_rx: mpsc::Receiver<FeedEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        Self {
            db,
            state,
            config,
            event_rx,
            shutdown,
            queue: BinaryHeap::new(),
            active_feeds: HashSet::new(),
            rejected_authors: HashMap::new(),
            failure_counts: HashMap::new(),
        }
    }

    /// Map a priority string to its polling interval.
    fn interval_for_priority(&self, priority: &str) -> Duration {
        match priority {
            "hot" => self.config.hot_interval,
            "warm" => self.config.warm_interval,
            _ => self.config.cold_interval,
        }
    }

    /// Load all discovery-scoped feeds and their contributors on startup.
    async fn load_initial_state(&mut self) {
        let feeds = match self.db.list_feeds().await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("discovery poller: failed to load feeds: {e}");
                return;
            }
        };
        for feed in feeds {
            if feed.scope == "discovery" {
                self.active_feeds.insert(feed.feed_id.clone());
                self.enqueue_contributors_for_feed(&feed.feed_id).await;
            }
        }
        tracing::info!(
            "discovery poller: loaded {} active discovery feeds",
            self.active_feeds.len()
        );
    }

    /// Load contributors for a single feed and push them into the queue.
    async fn enqueue_contributors_for_feed(&mut self, feed_id: &str) {
        let contributors = match self.db.list_contributors(feed_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("discovery poller: failed to load contributors for {feed_id}: {e}");
                return;
            }
        };
        let now = Instant::now();
        for c in contributors {
            let interval = self.interval_for_priority(&c.poll_priority);
            let key = ContributorKey {
                feed_id: c.feed_id,
                nest_url: c.nest_url,
                author_id: c.author_id,
            };
            self.queue.push(Reverse((now + interval, key)));
        }
    }

    /// Main event loop.
    pub async fn run(&mut self) {
        self.load_initial_state().await;

        let mut recalc_interval = tokio::time::interval(self.config.priority_recalc_interval);
        // Don't fire immediately — we just loaded state.
        recalc_interval.tick().await;

        loop {
            // Compute sleep duration until next poll item (or a long fallback).
            let sleep_until = self
                .queue
                .peek()
                .map(|Reverse((instant, _))| *instant)
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(60));

            let sleep_future =
                tokio::time::sleep_until(tokio::time::Instant::from_std(sleep_until));

            tokio::select! {
                // Shutdown signal — also exit if the sender was dropped, so a
                // dead channel can't busy-spin this `select!` arm.
                res = self.shutdown.changed() => {
                    if res.is_err() {
                        tracing::info!("discovery poller: shutdown channel closed, exiting");
                        return;
                    }
                    if *self.shutdown.borrow() {
                        tracing::info!("discovery poller: shutting down");
                        return;
                    }
                }

                // Feed event from route handlers.
                event = self.event_rx.recv() => {
                    match event {
                        Some(ev) => self.handle_event(ev).await,
                        None => {
                            tracing::info!("discovery poller: event channel closed, shutting down");
                            return;
                        }
                    }
                }

                // Priority recalculation timer.
                _ = recalc_interval.tick() => {
                    self.recalculate_all_priorities().await;
                }

                // Time to poll the next contributor.
                _ = sleep_future => {
                    self.poll_next_contributor().await;
                }
            }
        }
    }

    /// Handle a FeedEvent from route handlers.
    async fn handle_event(&mut self, event: FeedEvent) {
        match event {
            FeedEvent::Created { feed_id } => {
                // Check if this is a discovery feed.
                if let Ok(Some(feed)) = self.db.get_feed(&feed_id).await
                    && feed.scope == "discovery"
                {
                    self.active_feeds.insert(feed_id.clone());
                    // Enqueue contributors with immediate polling.
                    let contributors = self
                        .db
                        .list_contributors(&feed_id)
                        .await
                        .unwrap_or_default();
                    let now = Instant::now();
                    for c in contributors {
                        let key = ContributorKey {
                            feed_id: c.feed_id,
                            nest_url: c.nest_url,
                            author_id: c.author_id,
                        };
                        self.queue.push(Reverse((now, key)));
                    }
                }
            }
            FeedEvent::Deleted { feed_id } => {
                self.active_feeds.remove(&feed_id);
                self.rejected_authors.remove(&feed_id);
                // Stale queue entries will be skipped in poll_next_contributor.
            }
            FeedEvent::Updated { feed_id } => {
                if self.active_feeds.contains(&feed_id) {
                    // Rules changed: clear rejected cache and re-enqueue.
                    // Stats are already reset by the route handler.
                    self.rejected_authors.remove(&feed_id);
                    self.enqueue_contributors_for_feed(&feed_id).await;
                }
            }
            FeedEvent::ContributorAdded {
                feed_id,
                nest_url,
                author_id,
            } => {
                if self.active_feeds.contains(&feed_id) {
                    let key = ContributorKey {
                        feed_id,
                        nest_url,
                        author_id,
                    };
                    self.queue.push(Reverse((Instant::now(), key)));
                }
            }
            FeedEvent::ContributorRemoved { .. } => {
                // Stale entries will be skipped when polled.
            }
        }
    }

    /// Pop the next contributor from the queue and poll it.
    async fn poll_next_contributor(&mut self) {
        let Reverse((_, key)) = match self.queue.pop() {
            Some(entry) => entry,
            None => return,
        };

        // Skip entries for feeds that are no longer active.
        if !self.active_feeds.contains(&key.feed_id) {
            return;
        }

        // Verify the contributor still exists in the database.
        let contributors = match self.db.list_contributors(&key.feed_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "discovery poller: list_contributors error for {}: {e}",
                    key.feed_id
                );
                // Re-enqueue with warm interval as fallback.
                self.queue
                    .push(Reverse((Instant::now() + self.config.warm_interval, key)));
                return;
            }
        };

        let contributor = contributors
            .iter()
            .find(|c| c.nest_url == key.nest_url && c.author_id == key.author_id);

        let priority = match contributor {
            Some(c) => c.poll_priority.clone(),
            None => {
                // Contributor was removed; don't re-enqueue.
                return;
            }
        };

        // Load feed rules.
        let feed = match self.db.get_feed(&key.feed_id).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.active_feeds.remove(&key.feed_id);
                return;
            }
            Err(e) => {
                tracing::warn!("discovery poller: get_feed error for {}: {e}", key.feed_id);
                self.queue
                    .push(Reverse((Instant::now() + self.config.warm_interval, key)));
                return;
            }
        };

        let rules: Vec<FilterRule> = match fauna_core::encoding::canonical_decode(&feed.rules) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "discovery poller: decode rules error for {}: {e}",
                    key.feed_id
                );
                return;
            }
        };

        // A rule this nest cannot evaluate (`transport.md` § Rule 3 in full):
        // the feed keeps its rules intact and yields nothing new from discovery
        // until a build that can evaluate them runs — never a peer query that
        // would admit what the unknown rule might have excluded.
        if rules.iter().any(|r| !r.is_known()) {
            tracing::debug!(
                "discovery poller: feed {} holds a rule this nest cannot evaluate; skipping",
                key.feed_id
            );
            return;
        }

        let combination = match feed.combination.as_str() {
            "any" => FilterCombination::Any,
            _ => FilterCombination::All,
        };

        // Build author list for the query.
        let authors: Vec<ActorId> = match &key.author_id {
            Some(id) if id.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(id);
                vec![ActorId(arr)]
            }
            _ => vec![], // nest-level seed: query all authors on that nest
        };

        // Perform the peer query (over the federation channel; no HTTP fallback).
        let result = query_peer_channel_first(
            &self.state,
            &key.nest_url,
            &rules,
            combination,
            &authors,
            50, // limit per poll
            None,
        )
        .await;

        match result {
            Ok(candidates) => {
                // Reset failure count on success.
                self.failure_counts.remove(&key.nest_url);

                let now_us = fauna_core::data::Timestamp::now().as_i64();

                let mut referral_follows: u32 = 0;
                let mut all_references = Vec::new();

                for candidate in &candidates {
                    if !index_peer_candidate(&self.db, candidate, &key.nest_url).await {
                        continue;
                    }

                    // Record hit with current time in microseconds.
                    let _ = self
                        .db
                        .record_contributor_hit(
                            &key.feed_id,
                            &key.nest_url,
                            key.author_id
                                .as_deref()
                                .and_then(|id| <&[u8] as TryInto<&[u8; 32]>>::try_into(id).ok()),
                            now_us,
                        )
                        .await;

                    // Collect references for referral chain processing.
                    all_references.extend(candidate.references.clone());
                }

                // For nest-level seeds (author_id=None), promote to specific authors.
                if key.author_id.is_none() && !candidates.is_empty() {
                    let mut seen_authors: HashSet<Vec<u8>> = HashSet::new();
                    for candidate in &candidates {
                        if let fauna_core::feed::UnifiedIdentity::Fauna { actor_id } =
                            &candidate.author
                            && seen_authors.insert(actor_id.0.to_vec())
                        {
                            let _ = self
                                .db
                                .upsert_contributor(
                                    &key.feed_id,
                                    &key.nest_url,
                                    Some(&actor_id.0),
                                    "seed",
                                )
                                .await;
                            // Enqueue the new specific-author contributor.
                            self.queue.push(Reverse((
                                Instant::now() + self.interval_for_priority(&priority),
                                ContributorKey {
                                    feed_id: key.feed_id.clone(),
                                    nest_url: key.nest_url.clone(),
                                    author_id: Some(actor_id.0.to_vec()),
                                },
                            )));
                        }
                    }
                    // Remove the NULL entry.
                    let _ = self
                        .db
                        .remove_contributor(&key.feed_id, &key.nest_url, None)
                        .await;
                    // Don't re-enqueue the nest-level key.
                    // Follow references from this poll.
                    self.follow_references(
                        &key.feed_id,
                        &rules,
                        combination,
                        &all_references,
                        &mut referral_follows,
                    )
                    .await;
                    return;
                }

                // Follow referral chains.
                self.follow_references(
                    &key.feed_id,
                    &rules,
                    combination,
                    &all_references,
                    &mut referral_follows,
                )
                .await;

                // Re-enqueue with interval based on priority.
                self.queue.push(Reverse((
                    Instant::now() + self.interval_for_priority(&priority),
                    key,
                )));
            }
            Err(e) => {
                tracing::warn!(
                    "discovery poller: query_peer error for {} on {}: {e}",
                    key.feed_id,
                    key.nest_url
                );

                // Bound failure_counts to 10000 entries.
                if self.failure_counts.len() >= 10_000
                    && !self.failure_counts.contains_key(&key.nest_url)
                {
                    // Evict the entry with the lowest failure count.
                    if let Some(min_key) = self
                        .failure_counts
                        .iter()
                        .min_by_key(|(_, v)| **v)
                        .map(|(k, _)| k.clone())
                    {
                        self.failure_counts.remove(&min_key);
                    }
                }

                let count = self.failure_counts.entry(key.nest_url.clone()).or_insert(0);
                *count += 1;

                if *count >= 50 {
                    // Evict all contributors on this nest.
                    tracing::warn!(
                        "discovery poller: evicting nest {} after {} failures",
                        key.nest_url,
                        count
                    );
                    self.failure_counts.remove(&key.nest_url);
                    // Remove from DB for each active feed.
                    for feed_id in &self.active_feeds {
                        let _ = self.db.conn().await.execute(
                            "DELETE FROM feed_contributors WHERE feed_id = ?1 AND nest_url = ?2",
                            rusqlite::params![feed_id, key.nest_url],
                        );
                    }
                    // Don't re-enqueue.
                    return;
                }

                if *count >= 10 {
                    // Demote to cold via direct SQL.
                    let _ = self.db.conn().await.execute(
                        "UPDATE feed_contributors SET poll_priority = 'cold' WHERE nest_url = ?1",
                        rusqlite::params![key.nest_url],
                    );
                    self.queue
                        .push(Reverse((Instant::now() + self.config.cold_interval, key)));
                } else {
                    // Exponential backoff: warm_interval * 2^failures (capped at cold).
                    let backoff = self
                        .config
                        .warm_interval
                        .saturating_mul(1u32.wrapping_shl(*count));
                    let interval = backoff.min(self.config.cold_interval);
                    self.queue.push(Reverse((Instant::now() + interval, key)));
                }
            }
        }
    }

    /// Follow referral chains: for each reference, check if the referred author
    /// is already a contributor. If not, speculatively query and potentially add
    /// them.
    async fn follow_references(
        &mut self,
        feed_id: &str,
        rules: &[FilterRule],
        combination: FilterCombination,
        references: &[fauna_core::feed::PostReference],
        referral_follows: &mut u32,
    ) {
        for reference in references {
            if *referral_follows >= 5 {
                break;
            }

            // Validate author bytes.
            if reference.author.len() != 32 {
                continue;
            }
            let mut author_arr = [0u8; 32];
            author_arr.copy_from_slice(&reference.author);

            // Check if author is already a contributor.
            let contributors = match self.db.list_contributors(feed_id).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let already_contributor = contributors
                .iter()
                .any(|c| c.author_id.as_deref() == Some(reference.author.as_slice()));
            if already_contributor {
                continue;
            }

            // Check rejected cache (with 1-hour TTL).
            if let Some(rejected) = self.rejected_authors.get(feed_id)
                && let Some(rejected_at) = rejected.get(&reference.author)
                && rejected_at.elapsed() < Duration::from_secs(3600)
            {
                continue;
            }

            // Resolve nest URL: (a) from reference, (b) from contributors table, (c) from post_index source.
            let nest_url = if let Some(url) = &reference.nest_url {
                url.clone()
            } else if let Some(url) = self
                .resolve_nest_from_contributors(feed_id, &reference.author)
                .await
            {
                url
            } else if let Some(url) = self.resolve_nest_from_post_index(&author_arr).await {
                url
            } else {
                continue;
            };

            *referral_follows += 1;

            // Query the referred author's nest (channel-first, HTTP fallback).
            let authors = vec![ActorId(author_arr)];
            let result = query_peer_channel_first(
                &self.state,
                &nest_url,
                rules,
                combination,
                &authors,
                20,
                None,
            )
            .await;

            match result {
                Ok(candidates) if !candidates.is_empty() => {
                    // Matches found — add as contributor and ingest posts.
                    let _ = self
                        .db
                        .upsert_contributor(feed_id, &nest_url, Some(&author_arr), "referral")
                        .await;

                    let now_us = fauna_core::data::Timestamp::now().as_i64();

                    for candidate in &candidates {
                        if !index_peer_candidate(&self.db, candidate, &nest_url).await {
                            continue;
                        }
                        let _ = self
                            .db
                            .record_contributor_hit(feed_id, &nest_url, Some(&author_arr), now_us)
                            .await;
                    }

                    // Enqueue the new contributor.
                    self.queue.push(Reverse((
                        Instant::now() + self.config.warm_interval,
                        ContributorKey {
                            feed_id: feed_id.to_string(),
                            nest_url,
                            author_id: Some(author_arr.to_vec()),
                        },
                    )));
                }
                Ok(_) => {
                    // No matches — add to rejected cache.
                    self.add_rejected(feed_id, reference.author.clone());
                }
                Err(e) => {
                    tracing::debug!(
                        "discovery poller: referral query failed for {} on {}: {e}",
                        hex::encode(&reference.author),
                        nest_url
                    );
                    // Don't cache as rejected — might be transient failure.
                }
            }
        }
    }

    /// Add an author to the rejected cache for a feed, evicting the oldest if
    /// the cache exceeds 1000 entries.
    fn add_rejected(&mut self, feed_id: &str, author: Vec<u8>) {
        let cache = self
            .rejected_authors
            .entry(feed_id.to_string())
            .or_default();
        if cache.len() >= 1000 {
            // Evict the oldest entry.
            if let Some(oldest_key) = cache
                .iter()
                .min_by_key(|(_, instant)| *instant)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest_key);
            }
        }
        cache.insert(author, Instant::now());
    }

    /// Try to find a nest URL for an author from the feed_contributors table.
    /// Searches across all feeds (not just the current one) per spec §8.2.
    async fn resolve_nest_from_contributors(
        &self,
        _feed_id: &str,
        author: &[u8],
    ) -> Option<String> {
        let conn = self.db.conn().await;
        let mut stmt = conn
            .prepare("SELECT nest_url FROM feed_contributors WHERE author_id = ?1 LIMIT 1")
            .ok()?;
        let mut rows = stmt
            .query_map(rusqlite::params![author], |row| row.get::<_, String>(0))
            .ok()?;
        rows.next()?.ok()
    }

    /// Try to find a nest URL for an author from a post of theirs this nest
    /// already indexed via discovery (`content.origin_nest_url`). This used to scan `source LIKE 'http%'` and parse a
    /// base URL out of the fetch-URL `source` overloaded there; now the nest
    /// URL has its own column, already in the exact shape callers want.
    async fn resolve_nest_from_post_index(&self, author: &[u8; 32]) -> Option<String> {
        let conn = self.db.conn().await;
        let mut stmt = conn
            .prepare(
                "SELECT origin_nest_url FROM content \
                 WHERE author = ?1 AND origin_nest_url IS NOT NULL LIMIT 1",
            )
            .ok()?;
        let mut rows = stmt
            .query_map(rusqlite::params![author.as_slice()], |row| {
                row.get::<_, String>(0)
            })
            .ok()?;
        rows.next()?.ok()
    }

    /// Recalculate priorities for all active feeds.
    async fn recalculate_all_priorities(&self) {
        for feed_id in &self.active_feeds {
            if let Err(e) = self.db.recalculate_priorities(feed_id).await {
                tracing::warn!("discovery poller: recalculate_priorities error for {feed_id}: {e}");
            }
        }
    }
}

/// Spawn the discovery poller as a background task.
pub fn spawn_discovery_poller(
    db: Arc<CacheDb>,
    state: Arc<AppState>,
    config: PollConfig,
    event_rx: mpsc::Receiver<FeedEvent>,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): the handle is returned and its only
    // caller adopts it (`lib.rs`'s `state.scope_handle(spawn_discovery_poller(…))`).
    tokio::spawn(async move {
        let mut poller = DiscoveryPoller::new(db, state, config, event_rx, shutdown);
        poller.run().await;
    })
}

/// Index one peer candidate as a feed-index stub, recording the contributor
/// nest this poll fetched it from — the one place a discovery poll writes a
/// peer's claim into the projection, shared by the contributor poll and the
/// referral follow.
///
/// `source` carries the peer's advertised protocol token, never the fetch URL
/// (a locator in the badge/filter column), and the contributor nest rides its
/// own `origin_nest_url` column, which the referral-chain lookups
/// (`resolve_nest_from_post_index`, `get_post_references`) read.
///
/// The candidate's `created_at` is the peer's own claim, bound to no signed
/// body, and it becomes the column the local feed sorts on — so it is held to
/// the native future bound (`docs/goal/ui/feed.md` § The read model).
///
/// Returns `false` for a candidate this nest does not index — its address or
/// author is not Fauna-native, or it is dated past that bound — which the
/// caller skips like any other malformed candidate.
async fn index_peer_candidate(
    db: &CacheDb,
    candidate: &fauna_core::feed::ScoredCandidate,
    origin_nest_url: &str,
) -> bool {
    let fauna_core::feed::ContentAddress::Fauna { post_id } = &candidate.post_id else {
        return false;
    };
    let fauna_core::feed::UnifiedIdentity::Fauna { actor_id } = &candidate.author else {
        return false;
    };
    if crate::storage::reject_future_created_at(candidate.created_at).is_err() {
        tracing::debug!(
            post_id = %hex::encode(post_id.digest()),
            "discovery: dropping a candidate dated past the future bound"
        );
        return false;
    }
    let _ = db
        .insert_post_index_entry_with_origin(
            &post_id.digest(),
            &actor_id.0,
            candidate.created_at.as_i64(),
            candidate.metadata.has_media,
            candidate.metadata.is_reply,
            &candidate.source_token,
            &candidate.metadata.tags,
            Some(origin_nest_url),
        )
        .await;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contributor_key_ordering() {
        let a = ContributorKey {
            feed_id: "a".to_string(),
            nest_url: "http://a".to_string(),
            author_id: None,
        };
        let b = ContributorKey {
            feed_id: "a".to_string(),
            nest_url: "http://b".to_string(),
            author_id: None,
        };
        assert!(a < b);
    }

    #[test]
    fn test_poll_config_defaults() {
        let config = PollConfig::default();
        assert_eq!(config.hot_interval, Duration::from_secs(60));
        assert_eq!(config.warm_interval, Duration::from_secs(300));
        assert_eq!(config.cold_interval, Duration::from_secs(1800));
        assert_eq!(config.priority_recalc_interval, Duration::from_secs(600));
    }

    #[test]
    fn test_rejected_cache_eviction() {
        let mut poller_rejected: HashMap<String, HashMap<Vec<u8>, Instant>> = HashMap::new();
        let feed_id = "test_feed";
        let cache = poller_rejected.entry(feed_id.to_string()).or_default();

        // Fill to 1000.
        for i in 0u32..1000 {
            let mut key = vec![0u8; 32];
            key[..4].copy_from_slice(&i.to_be_bytes());
            cache.insert(key, Instant::now());
        }
        assert_eq!(cache.len(), 1000);

        // Simulate add_rejected behavior.
        if cache.len() >= 1000
            && let Some(oldest_key) = cache
                .iter()
                .min_by_key(|(_, instant)| *instant)
                .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest_key);
        }
        let new_key = vec![0xffu8; 32];
        cache.insert(new_key, Instant::now());
        assert_eq!(cache.len(), 1000);
    }

    /// A peer candidate's `created_at` is the peer's own claim, bound to no
    /// signed body, and it is written into the column the local feed sorts
    /// on. So a candidate dated past the future bound — or past `i64::MAX`,
    /// where a narrowing would wrap it — is not indexed, while an honest one
    /// beside it is, so the refusal cannot be passed by indexing nothing.
    #[tokio::test]
    async fn a_peer_candidate_dated_past_the_future_bound_is_not_indexed() {
        use fauna_core::data::Timestamp;
        use fauna_core::feed::{
            CandidateMetadata, ContentAddress, ScoredCandidate, SourceTag, UnifiedIdentity,
        };
        let db = CacheDb::open_in_memory().unwrap();
        let candidate = |id: u8, created_at: Timestamp| ScoredCandidate {
            post_id: ContentAddress::Fauna {
                post_id: fauna_cbor::Cid::from_digest_dag_cbor([id; 32]),
            },
            author: UnifiedIdentity::Fauna {
                actor_id: ActorId([0x5Au8; 32]),
            },
            source: SourceTag::Fauna,
            source_token: "fauna".into(),
            created_at,
            score: None,
            scorer_version: None,
            fetch_url: String::new(),
            metadata: CandidateMetadata {
                tags: vec![],
                has_media: false,
                is_reply: false,
                body_hint: None,
            },
            references: vec![],
        };
        let now = Timestamp::now().0;
        let cases = [
            (
                0xA1u8,
                Timestamp(now + 3_600_000_000),
                false,
                "an hour ahead",
            ),
            (0xA2u8, Timestamp(u64::MAX), false, "past i64::MAX"),
            (0xA3u8, Timestamp(now), true, "now"),
        ];
        for (id, created_at, indexed, when) in cases {
            index_peer_candidate(&db, &candidate(id, created_at), "https://peer.example").await;
            assert_eq!(
                db.post_exists(&[id; 32]).await.unwrap(),
                indexed,
                "a peer candidate dated {when} (created_at {}) must {}be indexed",
                created_at.0,
                if indexed { "" } else { "not " }
            );
        }
    }
}
