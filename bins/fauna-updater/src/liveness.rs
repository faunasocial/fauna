//! The updater's own health (`update-channel.md` § The updater → *Its
//! health*): a heartbeat the loop writes each time a nest's health read
//! answers, and the `--health` check the image's `HEALTHCHECK` hands an
//! updater container to, which reads its age. Healthy therefore means the
//! updater is running and can reach the nest it keeps current — nothing
//! listens in this container, so no probe of a port could say that.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// In the container's writable layer: the updater mounts nothing but the
/// socket, and the file is the container's own, gone with it.
pub const HEARTBEAT_PATH: &str = "/tmp/fauna-updater-heartbeat";

/// A heartbeat older than this reads unhealthy. Ten nest polls: a pass that
/// pulls a new build and re-creates the nest writes no beat while it runs,
/// and the nest's own restart is a minute or two of unanswered reads.
pub const STALE_AFTER: Duration = Duration::from_secs(600);

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Record that a nest answered now. A failed write is logged and otherwise
/// ignored: the check then reads the updater unhealthy, which is the truth
/// about an updater that cannot write its own layer.
pub fn beat(path: &Path) {
    if let Err(e) = std::fs::write(path, unix_now().to_string()) {
        tracing::warn!("heartbeat: {e}");
    }
}

/// Whether a heartbeat's contents, read at `now` (unix seconds), are fresh.
/// Anything that does not parse is stale; a beat from the future (a clock
/// stepped back) counts as fresh.
pub fn fresh(contents: &str, now: u64) -> bool {
    contents
        .trim()
        .parse::<u64>()
        .is_ok_and(|at| now.saturating_sub(at) <= STALE_AFTER.as_secs())
}

/// The `--health` check: `true` when the heartbeat at `path` exists and is
/// fresh. No heartbeat yet — the updater has not reached its nest since it
/// started — is unhealthy; the image's start period covers the first poll.
pub fn healthy(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|c| fresh(&c, unix_now()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heartbeat_is_fresh_until_it_is_older_than_the_limit() {
        let limit = STALE_AFTER.as_secs();
        assert!(fresh("1000", 1000));
        assert!(fresh("1000\n", 1000 + limit));
        assert!(!fresh("1000", 1000 + limit + 1));
        assert!(fresh("2000", 1000), "a beat from the future is fresh");
    }

    #[test]
    fn a_heartbeat_that_does_not_parse_is_stale() {
        for junk in ["", "soon", "-5", "1.5"] {
            assert!(!fresh(junk, 1000), "{junk:?}");
        }
    }

    #[test]
    fn a_beat_reads_healthy_and_no_beat_reads_unhealthy() {
        let dir =
            std::env::temp_dir().join(format!("fauna-updater-liveness-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("heartbeat");
        let _ = std::fs::remove_file(&path);
        assert!(!healthy(&path), "no heartbeat yet");
        beat(&path);
        assert!(healthy(&path));
        std::fs::write(&path, "0").unwrap();
        assert!(!healthy(&path), "a beat from 1970 is stale");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
