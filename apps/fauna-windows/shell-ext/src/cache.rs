use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Instant;

use fauna_ipc::sync::FileStatus;

const TTL_SECS: u64 = 30;
const EVICTION_AGE_SECS: u64 = 300;

struct CacheEntry {
    status: FileStatus,
    timestamp: Instant,
}

#[derive(Default)]
pub struct ShellCache {
    entries: RwLock<HashMap<PathBuf, CacheEntry>>,
}

impl ShellCache {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Get cached status for a path. Returns None on miss or stale entry.
    pub fn get(&self, path: &Path) -> Option<FileStatus> {
        let entries = self.entries.read().unwrap();
        let entry = entries.get(path)?;
        if entry.timestamp.elapsed().as_secs() < TTL_SECS {
            Some(entry.status)
        } else {
            None
        }
    }

    /// Insert or update a cache entry with a fresh timestamp.
    pub fn set(&self, path: PathBuf, status: FileStatus) {
        let mut entries = self.entries.write().unwrap();
        entries.insert(
            path,
            CacheEntry {
                status,
                timestamp: Instant::now(),
            },
        );
    }

    /// Returns true if the entry exists but is older than TTL_SECS.
    pub fn is_stale(&self, path: &Path) -> bool {
        let entries = self.entries.read().unwrap();
        entries
            .get(path)
            .is_some_and(|e| e.timestamp.elapsed().as_secs() >= TTL_SECS)
    }

    /// Remove entries older than EVICTION_AGE_SECS.
    pub fn evict_old(&self) -> usize {
        let mut entries = self.entries.write().unwrap();
        let before = entries.len();
        entries.retain(|_, e| e.timestamp.elapsed().as_secs() < EVICTION_AGE_SECS);
        before - entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miss_returns_none() {
        let cache = ShellCache::new();
        assert_eq!(cache.get(Path::new("C:\\foo.txt")), None);
    }

    #[test]
    fn set_then_get() {
        let cache = ShellCache::new();
        cache.set(PathBuf::from("C:\\foo.txt"), FileStatus::Synced);
        assert_eq!(
            cache.get(Path::new("C:\\foo.txt")),
            Some(FileStatus::Synced)
        );
    }

    #[test]
    fn update_overwrites() {
        let cache = ShellCache::new();
        cache.set(PathBuf::from("C:\\foo.txt"), FileStatus::Syncing);
        cache.set(PathBuf::from("C:\\foo.txt"), FileStatus::Synced);
        assert_eq!(
            cache.get(Path::new("C:\\foo.txt")),
            Some(FileStatus::Synced)
        );
    }

    #[test]
    fn fresh_entry_is_not_stale() {
        let cache = ShellCache::new();
        cache.set(PathBuf::from("C:\\foo.txt"), FileStatus::Synced);
        assert!(!cache.is_stale(Path::new("C:\\foo.txt")));
    }

    #[test]
    fn missing_entry_is_not_stale() {
        let cache = ShellCache::new();
        assert!(!cache.is_stale(Path::new("C:\\nonexistent")));
    }

    #[test]
    fn evict_removes_nothing_when_fresh() {
        let cache = ShellCache::new();
        cache.set(PathBuf::from("C:\\foo.txt"), FileStatus::Synced);
        assert_eq!(cache.evict_old(), 0);
    }

    #[test]
    fn full_overlay_pipeline() {
        use crate::overlay::{OverlayKind, should_show_overlay};

        let cache = ShellCache::new();

        // Initially no overlay
        assert!(!should_show_overlay(
            OverlayKind::Synced,
            Path::new("C:\\doc.txt"),
            &cache,
            None,
        ));

        // Simulate query function (as if pipe client returned Syncing)
        let query = |_: &Path| -> Option<FileStatus> { Some(FileStatus::Syncing) };
        assert!(should_show_overlay(
            OverlayKind::Syncing,
            Path::new("C:\\doc.txt"),
            &cache,
            Some(&query),
        ));

        // Synced handler should not match (file is Syncing)
        assert!(!should_show_overlay(
            OverlayKind::Synced,
            Path::new("C:\\doc.txt"),
            &cache,
            None,
        ));

        // Simulate event update: file is now synced
        cache.set(PathBuf::from("C:\\doc.txt"), FileStatus::Synced);

        // Now Synced matches, Syncing doesn't
        assert!(should_show_overlay(
            OverlayKind::Synced,
            Path::new("C:\\doc.txt"),
            &cache,
            None,
        ));
        assert!(!should_show_overlay(
            OverlayKind::Syncing,
            Path::new("C:\\doc.txt"),
            &cache,
            None,
        ));
    }
}
