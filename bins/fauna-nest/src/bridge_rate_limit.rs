//! Sliding-window rate limiter for per-credential blob-fetch RPCs.
//! In-memory only — restart resets the window. Per spec § Audit-trail
//! caveat: this is an independent abuse bound that doesn't depend on
//! bridge audit-log honesty.
//!
//! Window: 60 seconds, 30 events. Tunable via Limiter::with_config.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

#[derive(Debug, Clone, Copy)]
pub struct LimiterConfig {
    pub window: Duration,
    pub max_events: u32,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            max_events: 30,
        }
    }
}

/// Per-actor throttle for the post training correction
/// (`fauna.moderation.train` — a post fetch + decode and a report-aggregate
/// recompute per call), keyed by the authenticated caller. 60 events / 60 s is far above any
/// human marking spam, but bounds a tight loop so one authenticated user
/// can't pin the read path. A constant (an abuse bound the binary sets, not an
/// admin preference — `mail-spam.md` config-surface invariant).
pub const SPAM_TRAIN_LIMITER_CONFIG: LimiterConfig = LimiterConfig {
    window: Duration::from_secs(60),
    max_events: 60,
};

/// Per-(actor, channel) throttle on a **non-claimant** rostered member's MLS
/// Commit into a **claimed** folder channel (`federation.md` § Cross-nest
/// shared folders + channel append, residual (a) — the 2026-08-24
/// roster-membership admission widening's named DoS follow-up: a hostile
/// member can churn epochs with takeover commits, and every co-member
/// processes each one). The claimant is exempt (checked before this limiter
/// is ever consulted — an owner rotating their own set is never throttled).
///
/// Sized above a naive "once per device transition" estimate: the
/// device-owned-epoch takeover fires on a member's first application send in
/// ANY epoch (`devices.md` § Cross-device MLS group-state sync), so in an
/// actively-alternating multi-member set the legitimate rate is
/// traffic-shaped, not device-transition-shaped — several members can each
/// owe one takeover in quick succession after a single real epoch change. 60
/// events / hour gives room for that burst while still bounding a determined
/// churner to two orders of magnitude below what unthrottled spam would cost
/// (an abuse bound the binary sets, not an admin preference — `mail-spam.md`
/// config-surface invariant). Starting point, refutable once real multi-
/// member-folder telemetry exists.
pub const CHANNEL_COMMIT_LIMITER_CONFIG: LimiterConfig = LimiterConfig {
    window: Duration::from_secs(3600),
    max_events: 60,
};

/// Per-principal throttle on the HTTP record door (`third-party-kinds.md`
/// § Kind namespacing → *Two doors onto the same plane*: "rate-limited per
/// principal by a hard constant"), bucket key `(account, account,
/// "records:<principal id hex>")`. Every admitted request counts — a put, a
/// delete, a walk page alike. 600 / 60 s is ten a second sustained: far above
/// a remote server syncing its own records (a scope holds at most
/// `MAX_STATE_ENTRIES_PER_SCOPE` live rows, walked in pages), and a bound on
/// one app pinning the account's plane. A constant (an abuse bound the binary
/// sets, not an admin preference — the one-configuration-surface rule).
pub const RECORDS_DOOR_LIMITER_CONFIG: LimiterConfig = LimiterConfig {
    window: Duration::from_secs(60),
    max_events: 600,
};

#[derive(Debug, Default)]
struct Bucket {
    events: Vec<Instant>,
}

/// DashMap key for the rate-limiter bucket. Two fixed-size pubkey
/// arrays (no heap allocation) plus the credential_id string. The
/// final `String` does still allocate on insert; eliminating it
/// would need a small-string crate (compact_str / smol_str) which
/// is not yet a workspace dep — separate slice if it shows on the
/// hot-path profile.
type BucketKey = ([u8; 32], [u8; 32], String);

#[derive(Debug)]
pub struct Limiter {
    config: LimiterConfig,
    buckets: DashMap<BucketKey, Bucket>,
}

impl Limiter {
    pub fn new() -> Self {
        Self::with_config(LimiterConfig::default())
    }

    pub fn with_config(config: LimiterConfig) -> Self {
        Self {
            config,
            buckets: DashMap::new(),
        }
    }

    /// Returns true iff the call is permitted; records the event on
    /// success.
    pub fn check(
        &self,
        bridge_actor_id: &[u8; 32],
        target_actor_id: &[u8; 32],
        credential_id: &str,
    ) -> bool {
        let key: BucketKey = (
            *bridge_actor_id,
            *target_actor_id,
            credential_id.to_string(),
        );
        let now = Instant::now();
        let mut entry = self.buckets.entry(key).or_default();
        let cutoff = now.checked_sub(self.config.window).unwrap_or(now);
        entry.events.retain(|t| *t > cutoff);
        if entry.events.len() as u32 >= self.config.max_events {
            return false;
        }
        entry.events.push(now);
        true
    }

    /// Returns true iff the bucket is full right now, recording nothing — the
    /// before-the-work peek of a caller that records only on a later outcome
    /// (`failed_credential_throttle`, which counts refusals, not attempts).
    pub fn is_full(
        &self,
        bridge_actor_id: &[u8; 32],
        target_actor_id: &[u8; 32],
        credential_id: &str,
    ) -> bool {
        let key: BucketKey = (
            *bridge_actor_id,
            *target_actor_id,
            credential_id.to_string(),
        );
        let Some(entry) = self.buckets.get(&key) else {
            return false;
        };
        let now = Instant::now();
        let cutoff = now.checked_sub(self.config.window).unwrap_or(now);
        entry.events.iter().filter(|t| **t > cutoff).count() as u32 >= self.config.max_events
    }

    /// Drops bucket entries whose event lists are entirely outside the
    /// rate-limit window. Returns the number of buckets evicted. A
    /// long-lived deployment would otherwise grow `buckets`
    /// unboundedly as compromised approved bridges spray distinct
    /// credential_id strings; this sweep bounds that growth by evicting
    /// stale buckets.
    pub fn sweep(&self) -> usize {
        let now = Instant::now();
        let cutoff = now.checked_sub(self.config.window).unwrap_or(now);
        let mut to_drop: Vec<BucketKey> = Vec::new();
        for entry in self.buckets.iter() {
            if entry.value().events.iter().all(|t| *t <= cutoff) {
                to_drop.push(entry.key().clone());
            }
        }
        let mut evicted = 0;
        for key in to_drop {
            // remove_if avoids dropping a bucket that another
            // request just touched between the iter scan and the
            // remove call.
            if self
                .buckets
                .remove_if(&key, |_, b| b.events.iter().all(|t| *t <= cutoff))
                .is_some()
            {
                evicted += 1;
            }
        }
        evicted
    }

    /// Number of buckets currently tracked. Test/observability hook.
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

pub type SharedLimiter = Arc<Limiter>;

/// Spawn a tokio task that periodically calls `Limiter::sweep` on the
/// supplied limiter, via the shared [`crate::sweeper::spawn_periodic_sweeper`]
/// primitive, skipping the initial immediate tick so we don't sweep an empty
/// limiter at startup before any traffic. The default interval (5 × window)
/// is fine for a light deployment; once metrics land the cadence can be tuned
/// from observed bucket growth. Returns a
/// `JoinHandle` so the caller can abort on shutdown if needed.
pub fn spawn_sweeper(limiter: SharedLimiter, interval: Duration) -> tokio::task::JoinHandle<()> {
    crate::sweeper::spawn_periodic_sweeper(interval, true, move || {
        let limiter = limiter.clone();
        async move {
            let evicted = limiter.sweep();
            if evicted > 0 {
                tracing::debug!(
                    target: "bridge_rate_limit",
                    evicted,
                    bucket_count = limiter.bucket_count(),
                    "swept stale rate-limit buckets"
                );
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_under_limit() {
        let l = Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: 3,
        });
        let bridge = [1u8; 32];
        let actor = [2u8; 32];
        for _ in 0..3 {
            assert!(l.check(&bridge, &actor, "cred"));
        }
        assert!(!l.check(&bridge, &actor, "cred"));
    }

    #[test]
    fn separate_credentials_separate_buckets() {
        let l = Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: 1,
        });
        let bridge = [1u8; 32];
        let actor = [2u8; 32];
        assert!(l.check(&bridge, &actor, "cred-a"));
        assert!(!l.check(&bridge, &actor, "cred-a"));
        assert!(l.check(&bridge, &actor, "cred-b"));
    }

    #[test]
    fn separate_bridges_separate_buckets() {
        let l = Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: 1,
        });
        let bridge_a = [1u8; 32];
        let bridge_b = [2u8; 32];
        let actor = [3u8; 32];
        assert!(l.check(&bridge_a, &actor, "cred"));
        assert!(l.check(&bridge_b, &actor, "cred"));
        assert!(!l.check(&bridge_a, &actor, "cred"));
    }

    #[test]
    fn events_outside_window_are_pruned() {
        let l = Limiter::with_config(LimiterConfig {
            window: Duration::from_millis(50),
            max_events: 1,
        });
        let bridge = [1u8; 32];
        let actor = [2u8; 32];
        assert!(l.check(&bridge, &actor, "cred"));
        assert!(!l.check(&bridge, &actor, "cred"));
        std::thread::sleep(Duration::from_millis(80));
        assert!(l.check(&bridge, &actor, "cred"));
    }

    #[test]
    fn sweep_evicts_buckets_with_only_stale_events() {
        let l = Limiter::with_config(LimiterConfig {
            window: Duration::from_millis(40),
            max_events: 5,
        });
        let bridge_a = [1u8; 32];
        let bridge_b = [2u8; 32];
        let actor = [3u8; 32];
        // Bridge A goes idle (events will age out); bridge B keeps active.
        assert!(l.check(&bridge_a, &actor, "cred"));
        assert!(l.check(&bridge_b, &actor, "cred"));
        assert_eq!(l.bucket_count(), 2);

        std::thread::sleep(Duration::from_millis(60));
        // Bridge B refreshes; bridge A's events are now stale.
        assert!(l.check(&bridge_b, &actor, "cred"));

        let evicted = l.sweep();
        assert_eq!(evicted, 1, "exactly one stale bucket should be evicted");
        assert_eq!(l.bucket_count(), 1);
    }

    #[test]
    fn sweep_is_a_noop_when_nothing_stale() {
        let l = Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: 5,
        });
        let bridge = [1u8; 32];
        let actor = [2u8; 32];
        assert!(l.check(&bridge, &actor, "cred"));
        let evicted = l.sweep();
        assert_eq!(evicted, 0);
        assert_eq!(l.bucket_count(), 1);
    }

    #[tokio::test]
    async fn spawn_sweeper_evicts_after_two_ticks() {
        let l = Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_millis(40),
            max_events: 5,
        }));
        l.check(&[1u8; 32], &[2u8; 32], "cred");
        assert_eq!(l.bucket_count(), 1);

        let handle = spawn_sweeper(l.clone(), Duration::from_millis(30));
        // The eviction needs: the sweeper's skipped startup tick (30ms) + the
        // bucket aging out of its 40ms window + a second tick (30ms) — about
        // 100ms of real time on an idle box.
        //
        // The assertion is a DEADLINE POLL, not a settle-sleep (testing.md
        // § point 14). It previously slept a fixed 180ms and then asserted; that
        // budget holds only on an idle machine, while the primary dev VM
        // routinely runs 20+ concurrent builds at double-digit load, so it was in
        // the defunct wall-clock class — the same shape that had already gone red
        // on `origin/main` twice in this crate (the audit and TLS-RPT retention
        // sweepers). The budget below is sized far above any non-pathological
        // scheduling delay; a green run exits on the first poll that observes the
        // eviction.
        const EVICT_BUDGET: Duration = Duration::from_secs(30);
        let deadline = std::time::Instant::now() + EVICT_BUDGET;
        loop {
            let count = l.bucket_count();
            if count == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "sweeper did not evict the bucket within {EVICT_BUDGET:?} \
                 ({count} bucket(s) still present)"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        handle.abort();
    }
}
