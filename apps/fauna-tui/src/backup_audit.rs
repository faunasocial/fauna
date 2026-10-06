//! The tui app's local home for the backup audit loop's state — the native
//! side of `fauna_client_backup::audit`'s [`AuditStateStore`] seam, plus the one
//! observation feed that keeps its freshness comparison honest.
//!
//! Spec: `docs/goal/ui/backups.md` § Audit-alert surface ("audit state … persists
//! client-locally"); the loop itself and its ratified constants are owned by
//! `docs/goal/behavior/backup-restore.md` § Background Tasks. tui is the second
//! shell after linux, whose `backup_audit.rs` this mirrors — same file format,
//! same account scoping, same e2e clock seam.
//!
//! ## Why a plain JSON file, and why here
//!
//! The state is **this device's own evidence** about a destination, not a user
//! preference, so it never goes near the account plane (`fauna.state.backup`) — two devices auditing the same
//! destination hold independently valid records, and a synced copy would let one
//! device's stale verdict silence another's fresh one. It lives under a
//! per-actor `backup/` subdir of tui's config dir ([`state_path`]), the same
//! `fauna_sync_engine::db::actor_state_dir` scoping `crate::account_scope`
//! applies to `mls_state.db`, so switching accounts cannot show one owner the
//! other's audit history.
//!
//! Every field is **recreatable** — losing the file costs exactly one re-audit —
//! which is why the reads below degrade to the default instead of surfacing an
//! error the user could not act on, and why this file needs no migration story.
//!
//! ## The observation feed
//!
//! [`observe_thread_activity`] is the load-bearing half. Freshness compares what
//! the *destination* holds against what this client itself knows exists, and the
//! client must not learn the latter from the party being audited: a source nest
//! that answers "nothing new" would otherwise make freshness unfailable forever.
//! So the client writes down what it has actually **displayed** — the newest
//! conversation activity it has ever rendered — and that observation cannot be
//! retracted by the source afterwards. Monotonic, per the shared
//! `observe_local_record`.
//!
//! **Why this one caches where linux does not.** linux feeds the observation
//! from a GTK `render()` that fires on real repaints; tui's feed rides
//! `conversations::elements()`, which is rebuilt for *every* automation read as
//! well as every frame, so linux's load-file-per-render shape would put a
//! syscall pair on the hot path. [`OBSERVED`] makes the steady state one mutex
//! and one integer compare, and touches the file only when the high-water
//! genuinely advances. It is keyed by actor because a tui account switch is
//! in-process ([`crate::app::App::switch_account`] drops state and re-launches
//! without re-execing), so a bare process-wide cache would let one account's
//! observation suppress another's — the freshness comparison would then be made
//! against evidence from an identity that never saw it.
//!
//! The store itself and the e2e clock-offset trio are the shared
//! `fauna_client_backup::native_store` (re-exported below) — only [`state_path`]
//! (this install's per-actor directory convention) and [`OBSERVED`] (this
//! shell's own render-path cache) are tui-specific.

use std::path::PathBuf;
use std::sync::Mutex;

use fauna_client_backup::audit;
use fauna_client_backup::native_store::FileAuditStateStore;

pub use fauna_client_backup::native_store::now_secs;
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub use fauna_client_backup::native_store::set_clock_offset_secs;

/// The audit state file for `actor_id_hex`, under this install's config dir.
///
/// Mirrors linux's `sync::backup_state_dir()` (`<config>/backup/<actor-scope>/`)
/// so a box running both apps keeps two independent sets of evidence rather
/// than one app's verdict standing in for the other's.
fn state_path(actor_id_hex: &str) -> PathBuf {
    let flat = crate::session::config_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("fauna-tui"))
        .join("backup");
    fauna_sync_engine::db::actor_state_dir_or_unresolved(&flat, actor_id_hex)
        .join("audit-state.json")
}

/// `actor_id_hex`'s [`FileAuditStateStore`], over [`state_path`].
pub fn store(actor_id_hex: &str) -> FileAuditStateStore {
    FileAuditStateStore::at(state_path(actor_id_hex))
}

/// The `(actor, high-water)` this process last wrote or read, so the render-path
/// feed below can skip the file entirely on the overwhelmingly common no-op.
/// `None` until the first observation; the actor half is what makes an
/// in-process account switch re-seed instead of inheriting (module docs).
static OBSERVED: Mutex<Option<(String, i64)>> = Mutex::new(None);

/// Record that `actor_id_hex`'s client has displayed conversation activity
/// stamped `last_activity_ms` (epoch **milliseconds**, as `ThreadSummary` carries
/// it).
///
/// Called from the conversation list's paint — the one place the client shows the
/// user what it knows about the nest-originated message kinds, and therefore the
/// honest moment to say "I have seen a record this new". The load → observe →
/// save fold, and the millisecond→second boundary it crosses, are owned once by
/// `fauna_client_backup::audit`; this shell supplies only its store, its logging
/// idiom, and the [`OBSERVED`] cache the module docs explain.
///
/// Returns the `(actor, last_activity_ms)` pair this call actually consumed —
/// built from its own parameters, never a binding a caller held separately —
/// so a caller that starts forwarding a *different* freshly-resolved actor here
/// shows up in what comes back. Kept in
/// **milliseconds**, the caller's own unit: the store's committed high-water is
/// in **seconds** and is a monotonic, process-wide value shared across every
/// call for an actor, so pinning it here would make a caller's return
/// order-dependent on other calls. `None` only on the poisoned-lock degrade
/// below, where nothing was read, consulted, or written.
///
/// Failures are logged, never surfaced: a missed observation costs at most a
/// weaker freshness comparison until the next render, and there is nothing a user
/// could do about it.
pub fn observe_thread_activity(actor_id_hex: &str, last_activity_ms: i64) -> Option<(String, i64)> {
    let secs = audit::activity_secs(last_activity_ms);
    let mut observed = match OBSERVED.lock() {
        Ok(o) => o,
        // A poisoned lock means another thread panicked mid-update. The cache is
        // pure optimization, so the honest degrade is to skip this observation
        // rather than propagate a panic out of a paint. Nothing was consulted or
        // written, so there is no consumed pair to echo.
        Err(_) => return None,
    };
    // The fast path: same account, nothing newer than what we already wrote.
    if let Some((actor, high_water)) = observed.as_ref()
        && actor == actor_id_hex
        && *high_water >= secs
    {
        return Some((actor_id_hex.to_string(), last_activity_ms));
    }

    let outcome = audit::observe_thread_activity(&store(actor_id_hex), last_activity_ms);
    for e in &outcome.degradations {
        tracing::debug!("backup audit: {e}");
    }
    // Seed from the SNAPSHOT's high-water, not from `secs`: a store already
    // holding something newer (an earlier launch, or the account we just
    // switched to) must be what the fast path compares against, or every render
    // would re-read the file.
    *observed = Some((actor_id_hex.to_string(), outcome.high_water.unwrap_or(secs)));
    Some((actor_id_hex.to_string(), last_activity_ms))
}

// The e2e clock (CLOCK_OFFSET_SECS/clock_offset_secs/set_clock_offset_secs) and
// the store round-trip/missing/corrupt cases live once, in
// `fauna_client_backup::native_store` — nothing left to test here beyond this
// shell's own path-scoping.

#[cfg(test)]
mod tests {
    use super::*;

    /// Two accounts on one box must not share evidence — the scoping that makes
    /// the in-memory cache's actor key meaningful.
    #[test]
    fn two_actors_resolve_to_different_state_files() {
        let a = state_path(&"aa".repeat(32));
        let b = state_path(&"bb".repeat(32));
        assert_ne!(a, b, "per-actor scoping collapsed onto one file");
        assert_eq!(a.file_name(), b.file_name());
    }
}
