//! URL → resolved-preview TTL cache for the D4 link-preview resolver.
//!
//! `render-model.md` § D4: the nest "**caches by URL** (TTL — a fetched preview
//! is re-used across viewers; do NOT re-fetch per request)". Keyed by the exact
//! requested URL string; a hit returns the already-resolved preview (including
//! the already-stored `image_hash`) with no outbound fetch and no re-store.
//!
//! Concurrency mirrors `bridge_rate_limit::Limiter` (a `DashMap` keyed store with
//! `Instant`-based expiry + a periodic sweeper); same shape as the `ws.rs`
//! idempotency cache, generalized off `[u8; 16]` to a URL string.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// A resolved preview as cached and returned to clients (post-image-store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedPreview {
    pub title: String,
    pub description: String,
    /// Content-addressed blob hash (64-hex blake3) of the stored og:image, if any.
    pub image_hash: Option<String>,
}

/// Default hard ceiling on cached entries (LP-2). Bounds cache memory
/// **independent of the authenticated-actor count** — without it the only bound
/// is (actors × 60/min resolves × 60 min TTL) before the hourly sweep, which
/// scales with the actor population. 10 000 distinct cached previews (each
/// ~title + description + a 64-hex hash, well under a KiB) is a few MiB — ample
/// for a light deployment, where the sweeper keeps the steady state far lower.
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;

/// TTL cache from request URL → [`CachedPreview`].
pub struct LinkPreviewCache {
    entries: DashMap<String, (CachedPreview, Instant)>,
    ttl: Duration,
    /// Hard ceiling on live entries; a fresh insert past it evicts the
    /// oldest-by-insertion entry first (see [`Self::insert`]).
    max_entries: usize,
}

impl LinkPreviewCache {
    pub fn new(ttl: Duration) -> Self {
        Self::with_max_entries(ttl, DEFAULT_MAX_ENTRIES)
    }

    /// Like [`Self::new`] but with an explicit entry ceiling (tests use a tiny
    /// cap to exercise eviction without inserting 10 000 entries).
    pub fn with_max_entries(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: DashMap::new(),
            ttl,
            max_entries: max_entries.max(1),
        }
    }

    /// Fresh cached preview for `url`, or `None` if absent or expired. Expired
    /// entries are evicted lazily on read (the sweeper handles the cold ones).
    pub fn get(&self, url: &str) -> Option<CachedPreview> {
        let expired = {
            let entry = self.entries.get(url)?;
            if entry.1.elapsed() <= self.ttl {
                return Some(entry.0.clone());
            }
            true
        };
        if expired {
            self.entries.remove(url);
        }
        None
    }

    pub fn insert(&self, url: String, preview: CachedPreview) {
        // Bound cache memory independent of actor count (LP-2): a *new* key past
        // the ceiling first evicts the oldest-by-insertion entry. `get` does not
        // refresh timestamps, so insertion order == proximity-to-expiry order —
        // FIFO eviction sheds the entry nearest the TTL anyway. Re-inserting an
        // existing key replaces in place (no growth → no eviction). The check is
        // best-effort under concurrency (a racing insert may briefly exceed the
        // cap or evict one extra) — fine for a cache.
        if self.entries.len() >= self.max_entries && !self.entries.contains_key(&url) {
            self.evict_oldest();
        }
        self.entries.insert(url, (preview, Instant::now()));
    }

    /// Remove the entry with the earliest insertion `Instant`. Only called from
    /// [`Self::insert`] when at the cap, so the O(n) scan is amortized over a
    /// full cache. The oldest key is collected as an owned `String` *before* the
    /// `remove` so no DashMap shard lock is held across the mutation.
    fn evict_oldest(&self) {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|e| e.value().1)
            .map(|e| e.key().clone());
        if let Some(key) = oldest {
            self.entries.remove(&key);
        }
    }

    /// Drop every expired entry; returns the number removed. Call periodically.
    pub fn sweep(&self) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, (_, inserted)| inserted.elapsed() <= self.ttl);
        before - self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Spawn a tokio task that periodically [`LinkPreviewCache::sweep`]s the cache so
/// a long-lived nest doesn't accumulate never-re-requested entries (lazy
/// eviction only fires on a re-read of an expired url). Mirrors
/// [`crate::bridge_rate_limit::spawn_sweeper`] via the shared
/// [`crate::sweeper::spawn_periodic_sweeper`] primitive, skipping the
/// immediate first tick (don't sweep an empty cache at startup); the default
/// cadence (a few × the TTL) is fine for a light deployment. Returns the
/// `JoinHandle`.
pub fn spawn_sweeper(
    cache: Arc<LinkPreviewCache>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    crate::sweeper::spawn_periodic_sweeper(interval, true, move || {
        let cache = cache.clone();
        async move {
            let evicted = cache.sweep();
            if evicted > 0 {
                tracing::debug!(
                    target: "link_preview",
                    evicted,
                    remaining = cache.len(),
                    "swept expired link-preview cache entries"
                );
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(title: &str) -> CachedPreview {
        CachedPreview {
            title: title.to_string(),
            description: "d".to_string(),
            image_hash: Some("a".repeat(64)),
        }
    }

    #[test]
    fn hit_within_ttl() {
        let c = LinkPreviewCache::new(Duration::from_secs(3600));
        c.insert("https://example.com/p".to_string(), preview("T"));
        assert_eq!(c.get("https://example.com/p"), Some(preview("T")));
        assert_eq!(c.get("https://example.com/other"), None);
    }

    #[test]
    fn miss_and_evict_after_ttl() {
        // Zero TTL ⇒ any elapsed time expires the entry. The brief sleep
        // guarantees `elapsed() > 0` deterministically (no race on a 0-ns read).
        let c = LinkPreviewCache::new(Duration::ZERO);
        c.insert("https://example.com/p".to_string(), preview("T"));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(c.get("https://example.com/p"), None);
        // Lazy eviction on the miss removed it.
        assert!(c.is_empty());
    }

    #[test]
    fn sweep_drops_expired_only() {
        let c = LinkPreviewCache::new(Duration::ZERO);
        c.insert("https://a/".to_string(), preview("A"));
        c.insert("https://b/".to_string(), preview("B"));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(c.sweep(), 2);
        assert!(c.is_empty());
    }

    #[test]
    fn evicts_oldest_past_cap() {
        // Cap of 2, long TTL so nothing expires — eviction is the only way the
        // cache can shrink. The 2 ms sleeps make insertion-order `Instant`s
        // strictly increasing so "oldest" is deterministic.
        let c = LinkPreviewCache::with_max_entries(Duration::from_secs(3600), 2);
        c.insert("u1".to_string(), preview("1"));
        std::thread::sleep(Duration::from_millis(2));
        c.insert("u2".to_string(), preview("2"));
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(c.len(), 2);
        // A third *new* key is at the cap → evict the oldest (u1).
        c.insert("u3".to_string(), preview("3"));
        assert_eq!(c.len(), 2, "stays bounded at the cap");
        assert_eq!(c.get("u1"), None, "oldest-by-insertion evicted");
        assert_eq!(c.get("u2"), Some(preview("2")));
        assert_eq!(c.get("u3"), Some(preview("3")));
    }

    #[test]
    fn reinserting_existing_key_at_cap_does_not_evict() {
        // Re-inserting an already-present key replaces in place — no growth, so
        // no other entry is evicted even though the cache is full.
        let c = LinkPreviewCache::with_max_entries(Duration::from_secs(3600), 2);
        c.insert("u1".to_string(), preview("1"));
        c.insert("u2".to_string(), preview("2"));
        c.insert("u1".to_string(), preview("1-updated"));
        assert_eq!(c.len(), 2);
        assert_eq!(c.get("u1"), Some(preview("1-updated")));
        assert_eq!(c.get("u2"), Some(preview("2")), "sibling untouched");
    }
}
