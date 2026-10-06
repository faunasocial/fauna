//! The linux app's local home for the backup audit loop's state — the native
//! side of `fauna_client_backup::audit`'s [`AuditStateStore`] seam, plus the one
//! observation feed that keeps its freshness comparison honest.
//!
//! Spec: `docs/goal/ui/backups.md` § Audit-alert surface ("audit state … persists
//! client-locally"); the loop itself and its ratified constants are owned by
//! `docs/goal/behavior/backup-restore.md` § Background Tasks.
//!
//! ## Why a plain JSON file, and why here
//!
//! The state is **this device's own evidence** about a destination, not a user
//! preference, so it never goes near the account plane (`fauna.state.backup`) — two devices auditing the same
//! destination hold independently valid records, and a synced copy would let one
//! device's stale verdict silence another's fresh one. It lives beside the
//! segment-backup state under [`crate::sync::backup_state_dir`], keyed by the
//! **session's own actor id** (passed in by both call sites below, never a
//! fresh `active_actor_id_hex()` read — that can name a different account
//! than the one this process serves on a bound/secondary launch), so two
//! accounts sharing this device never share one account's audit evidence.
//!
//! Every field is **recreatable** — losing the file costs exactly one re-audit —
//! which is why the reads below degrade to the default instead of surfacing an
//! error the user could not act on, and why this file needs no migration story.
//!
//! The store itself is the shared `fauna_client_backup::native_store`, and the
//! e2e clock-offset trio is the platform-neutral
//! `fauna_client_backup::audit_clock` (it moved out of `native_store` when web's
//! `localStorage` store started sharing it — `native_store` keeps a compat
//! re-export, but the canonical home is the one imported below). Only
//! [`state_path`] (this account's own directory convention) is linux-specific;
//! tui's `backup_audit.rs` is the same shape over its own per-actor path.
//!
//! ## Bound-vs-active
//!
//! Sibling of tui's own fix for the same shape. Until this was
//! fixed, [`state_path`] read `crate::sync::backup_state_dir`
//! keyed by a fresh `active_actor_id_hex()` rather than the caller's own
//! session actor. Because linux's two call sites — the conversation list's
//! render-path [`observe_thread_activity`] and the Backups page's audit-pass
//! [`store`] — shared that one wrong key, the harm was never "an account's
//! activity goes unobserved": both halves agreed with each other, just with
//! the *active* account rather than the session's own. The real harm was two
//! accounts sharing the active account's `audit-state.json` on a bound
//! (secondary) launch: false freshness alarms in both directions, each
//! account's audit pass wiping the other's last-passed records via
//! `fauna_client_backup::audit::merge_outcomes`, and one owner's evidence
//! sitting in the other owner's scope dir. Both call sites now pass their own
//! session actor (`FaunaClient::actor_id()` / `AppState.actor_id`) straight
//! through to [`state_path`] — no shared accessor, no `active()` read.
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

use std::path::PathBuf;

use fauna_client_backup::audit;
use fauna_client_backup::native_store::FileAuditStateStore;

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub use fauna_client_backup::audit_clock::set_clock_offset_secs;

/// `actor_id_hex`'s audit state file, beside its segment-backup state.
fn state_path(actor_id_hex: &str) -> PathBuf {
    crate::sync::backup_state_dir(actor_id_hex).join("audit-state.json")
}

/// `actor_id_hex`'s [`FileAuditStateStore`], over [`state_path`].
pub fn store(actor_id_hex: &str) -> FileAuditStateStore {
    FileAuditStateStore::at(state_path(actor_id_hex))
}

/// Record that `actor_id_hex`'s client has displayed conversation activity
/// stamped `last_activity_ms` (epoch **milliseconds**, as `ThreadSummary`
/// carries it).
///
/// Called from the conversation list's render — the one place the client shows
/// the user what it knows about the nest-originated message kinds, and therefore
/// the honest moment to say "I have seen a record this new". The load → observe
/// → save fold, and the millisecond→second boundary it crosses, are owned once
/// by `fauna_client_backup::audit::observe_thread_activity`; this shell supplies
/// only its store and its logging idiom.
///
/// `actor_id_hex` is the *rendering* session's own actor — the caller's, never
/// a fresh registry read — so the observation always lands beside the account
/// whose activity it actually is.
///
/// Failures are logged, never surfaced: a missed observation costs at most a
/// weaker freshness comparison until the next render, and there is nothing a
/// user could do about it.
pub fn observe_thread_activity(actor_id_hex: &str, last_activity_ms: i64) {
    for e in audit::observe_thread_activity(&store(actor_id_hex), last_activity_ms).degradations {
        tracing::debug!("backup audit: {e}");
    }
}

// ── the re-run poke ──────────────────────────────────────────────────────────
//
// Compiled out of release artifacts (testing.md convention 15), like every other
// automation hook in this client. GTK-specific (the Backups page's own render
// thread) — no tui equivalent.

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
thread_local! {
    /// The Backups page's own re-run hook, installed while the page is wired
    /// (GTK thread). Poking it drives the **production** audit path — same
    /// `run_audit_pass`, same render — rather than a test-only shortcut that
    /// would prove nothing about what a user sees.
    static RERUN: std::cell::RefCell<Option<std::rc::Rc<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Install the Backups page's audit re-run hook (GTK thread).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn set_rerun_hook(hook: std::rc::Rc<dyn Fn()>) {
    RERUN.with(|h| *h.borrow_mut() = Some(hook));
}

/// Drive one audit pass on the Backups page if it is built. Returns whether a
/// hook was there to poke — the caller reports that, so a command against a page
/// that was never opened fails loudly rather than silently doing nothing
/// (testing.md convention 11).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn poke_rerun() -> bool {
    RERUN.with(|h| {
        let hook = h.borrow().clone();
        match hook {
            Some(hook) => {
                hook();
                true
            }
            None => false,
        }
    })
}

// The store round-trip/missing/corrupt cases live once, in
// `fauna_client_backup::native_store::tests` (the RERUN hook needs a live
// GTK thread) — this file's own test is the per-actor scoping [`state_path`]
// now does, which both real call sites (the render-path observation and the
// Backups page's audit pass) funnel through.

#[cfg(test)]
mod tests {
    use super::*;

    /// Two accounts on this device must not share evidence — the scoping
    /// that keeps both the render-path observation ([`observe_thread_activity`])
    /// and the Backups page's audit-pass read ([`store`]) out of each
    /// other's account. Twin of tui's pin of the same name.
    #[test]
    fn two_actors_resolve_to_different_state_files() {
        let a = state_path("aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11");
        let b = state_path("bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22");
        assert_ne!(a, b, "per-actor scoping collapsed onto one file");
        assert_eq!(a.file_name(), b.file_name());
    }
}
