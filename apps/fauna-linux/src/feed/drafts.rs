//! Linux leg of draft-persistence v2 for the **posts rail**
//! (`docs/goal/behavior/reserved-folders.md` § Drafts Sync;
//! `docs/goal/ui/feed.md` § Persistence).
//!
//! Pure trigger glue over the shared core — the exact structural twin of
//! [`crate::conversations::drafts`] one rail over (a second mechanism here
//! would be a per-app divergence in the one place, priority #1, where the two
//! rails are the same problem): [`fauna_client_drafts::DraftsSync`] owns the
//! seal, the WS-RPC `fauna.drafts.{get,put}` calls, the launch gate, and the
//! last-saved baseline; the posts-rail at-rest shape lives in
//! [`fauna_feed::FeedManager`]'s own `drafts_snapshot_bytes` / `restore_drafts`
//! pair. Only the RAIL and the manager type differ from the conversations leg.
//!
//! * **Load on launch** — spawn `DraftsSync::load()` on the tokio runtime, then
//!   `manager.restore_drafts(bytes)` so the composer reflects the post the
//!   user left half-written here or on another device.
//! * **Debounced autosave** — a [`crate::feed::observer`] tick fires on every
//!   manager change; the tick is marshalled onto the GTK main loop (same
//!   widget-thread rule as the conversations leg) where a generation-counter
//!   debounce waits for ~1.5 s of quiescence, then spawns
//!   `DraftsSync::save_if_changed(manager.drafts_snapshot_bytes())` on the
//!   tokio runtime.
//!
//! **The feed manager notifies far more often than the conversations one** —
//! every reload, resolved quote, link preview and score adjustment ticks the
//! same observer — and that is fine rather than something to filter here: a
//! non-compose tick costs one cheap byte-compare against the baseline and
//! stops, because the snapshot only carries user-authored compose fields.
//! Inventing a per-app "was it a compose change?" predicate would be exactly
//! the cleverness the shared dedup exists to avoid.
//!
//! Wired from the AuthSuccess handler beside `crate::conversations::drafts::start`
//! (`app.rs`), and, like the conversations leg, outside any engine/MLS gate:
//! drafts seal under the owner's `BackupKey` and need no MLS, so an unrelated
//! init failure must not also cost the user their unsent post.
//!
//! **Lifecycle differs from the conversations leg — read before "simplifying"
//! back to it.** `crate::conversations::host::manager()` is an eternal
//! `OnceLock` singleton: the same object for the process lifetime.
//! `crate::feed::host::manager()` is **swappable** — `init()` replaces the
//! singleton pointer with a brand-new `FeedManager` on every re-auth/account
//! switch (`feed/host.rs`'s own doc comment). A strong `Arc` held across the
//! debounce window would therefore pin the OLD manager (and the autosave
//! task) alive forever past that point — a leak on every re-auth, not merely
//! a missed cleanup. [`crate::drafts_autosave::attach_autosave`] holds only a
//! `Weak` for exactly this reason (uniformly across both rails — see its own
//! module doc): once nothing else holds the old manager, it drops, its
//! observer list drops with it, the channel closes, and both the receive
//! loop and any still-pending debounce timer retire on their next check
//! rather than resurrecting a torn-down session's manager.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fauna_client::NestClient;
use fauna_client_drafts::DraftsSync;
use fauna_core::identity::ActorKeypair;

use crate::drafts_autosave::DraftsHost;
use crate::feed::host::LinuxFeedManager;

impl DraftsHost for LinuxFeedManager {
    // Delegates to the inherent method of the same name — Rust's method
    // resolution always prefers an inherent impl over a trait impl for `.`
    // call syntax, so this is the real snapshot, not infinite recursion.
    fn drafts_snapshot_bytes(&self) -> Vec<u8> {
        self.drafts_snapshot_bytes()
    }
}

/// The feed composer's `__drafts` path (one combined blob, per
/// `reserved-folders.md` § Drafts Sync). The enumeration is the wire's, not
/// this app's — a leg that minted its own rail name would round-trip only
/// with itself and silently lose every draft the user's other devices wrote.
const RAIL: &str = fauna_protocol::drafts::RAIL_POSTS;

type FeedDraftsSync = DraftsSync<Arc<NestClient>>;

/// The current session's feed `DraftsSync`, mirroring
/// `crate::conversations::drafts`'s slot — see [`flush_now_blocking`].
/// Overwritten (never leaked) on every `start()`, one per re-auth, the same
/// as `feed::host`'s own swappable manager slot.
fn slot() -> &'static Mutex<Option<Arc<FeedDraftsSync>>> {
    static SYNC: OnceLock<Mutex<Option<Arc<FeedDraftsSync>>>> = OnceLock::new();
    SYNC.get_or_init(|| Mutex::new(None))
}

/// Force an immediate, bounded-blocking save of the feed manager's current
/// snapshot — the leave-door flush (`reserved-folders.md` § The leave-flush
/// promise, row 481). No-op before the first [`start`], or if the feed
/// manager has since been torn down (re-auth mid-close is not a real window,
/// but the check costs nothing).
pub fn flush_now_blocking(bounded: bool) {
    let Some(sync) = slot().lock().unwrap().clone() else {
        return;
    };
    let Some(manager) = crate::feed::host::manager() else {
        return;
    };
    let snapshot = manager.drafts_snapshot_bytes();
    crate::blocking_flush::run_bounded(
        async move {
            if let Err(e) = sync.save_if_changed(&snapshot).await {
                tracing::warn!("feed drafts: leave-flush failed: {e}");
            }
        },
        Duration::from_millis(2_000),
        Duration::from_millis(2_500),
        bounded,
    );
}

/// Wire feed-draft persistence for the just-authenticated actor: build the
/// shared [`DraftsSync`], restore the draft on launch, and attach the
/// debounced autosave. Called from the `AuthSuccess` handler alongside
/// `crate::conversations::drafts::start`.
///
/// A malformed secret is logged and skipped (drafts simply don't persist)
/// rather than aborting login — the same non-fatal arm as the conversations
/// leg.
///
/// `succession_predecessors` is the retired identities' `BackupKey`s
/// (`FaunaClient::predecessor_backup_keys()`, resolved ONCE by the caller and
/// shared with the `__mls` plane and the other two drafts rails — never a
/// second registry walk), offered to [`DraftsSync::with_predecessors`] so a
/// successor's launch load opens this rail instead of hard-erroring on it.
pub fn start(
    manager: Arc<LinuxFeedManager>,
    nest: Arc<NestClient>,
    secret_hex: &str,
    runtime: &tokio::runtime::Handle,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) {
    let keypair = match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("feed drafts: malformed identity secret, persistence disabled: {e}");
            return;
        }
    };
    let sync = Arc::new(build_sync(nest, &keypair, succession_predecessors));
    *slot().lock().unwrap() = Some(Arc::clone(&sync));
    restore_on_launch(Arc::clone(&manager), Arc::clone(&sync), runtime);
    // A fresh observer channel of the same kind the UI render loop uses; the
    // receiver lives on the GTK main loop so the shared debounce is legal.
    let rx = crate::feed::observer::attach(&manager);
    crate::drafts_autosave::attach_autosave(manager, rx, sync, runtime.clone(), "feed drafts");
}

/// Assemble this rail's [`DraftsSync`] — factored out of [`start`] so the
/// call-site pin test below can call it without building a whole session
/// (mirrors `conversations::drafts::build_sync`).
///
/// ⚠ The retired keys are a READ fallback, never a seal root
/// ([`DraftsSync::with_predecessors`]). A successor's launch load beats the
/// post-auth re-seal pass more often than not, and without them it
/// hard-errors, the load gate never lifts, and the user's half-written post
/// stays invisible for the whole session.
fn build_sync(
    nest: Arc<NestClient>,
    keypair: &ActorKeypair,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) -> FeedDraftsSync {
    DraftsSync::new(nest, keypair, RAIL).with_predecessors(succession_predecessors.to_vec())
}

/// Spawn the launch load: fetch + unseal this actor's feed draft and hand it
/// to the manager. `None` is first run (keep the empty composer); a
/// transport/seal error is logged and left non-fatal — an unreachable nest
/// must not blank the composer.
fn restore_on_launch(
    manager: Arc<LinuxFeedManager>,
    sync: Arc<FeedDraftsSync>,
    runtime: &tokio::runtime::Handle,
) {
    runtime.spawn(async move {
        match sync.load().await {
            Ok(Some(bytes)) => {
                tracing::info!("feed drafts: restored {} bytes", bytes.len());
                manager.restore_drafts(bytes);
            }
            Ok(None) => tracing::debug!("feed drafts: none persisted yet (first run)"),
            Err(e) => tracing::warn!("feed drafts: load failed: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rail path is the wire's enumeration, not a linux-local string. Same
    /// pin the tui leg carries, for the same reason: a minted name round-trips
    /// only with itself and loses the user's other devices.
    #[test]
    fn rail_is_the_shared_posts_path() {
        assert_eq!(RAIL, "posts");
        assert!(
            fauna_protocol::drafts::is_ratified_rail(RAIL),
            "a rail the nest refuses would fail every save at runtime, not here",
        );
    }

    fn offline_manager() -> Arc<LinuxFeedManager> {
        Arc::new(fauna_feed::FeedManager::new(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            [7u8; 32],
        ))
    }

    /// **The load-bearing property this leg exists to get right**, per the
    /// module doc's Lifecycle note: unlike the conversations leg, a strong
    /// `Arc` held across the debounce loop would pin a torn-down session's
    /// manager (and its nest connection) alive forever across every re-auth.
    /// The debounce timer must instead see the manager as gone once the last
    /// strong ref (the `feed::host` slot) drops it.
    #[test]
    fn weak_manager_reports_gone_once_the_strong_ref_is_dropped() {
        let manager = offline_manager();
        let weak = Arc::downgrade(&manager);
        assert!(weak.upgrade().is_some());
        drop(manager);
        assert!(
            weak.upgrade().is_none(),
            "a fired debounce timer must skip the save rather than resurrect a \
             torn-down session's manager",
        );
    }
}

#[cfg(test)]
mod succession_fallback_tests {
    use super::*;

    /// **The call-site pin for the `__drafts` read fallback on the feed rail.**
    /// `fauna-client-drafts`' own tests prove the fallback *works*; none of
    /// them can see THIS app stop passing the walk.
    ///
    /// Mutation: drop the `.with_predecessors(..)` in [`build_sync`] and this reds.
    #[test]
    fn the_feed_rail_offers_the_accounts_retired_roots() {
        let retired = vec![fauna_core::crypto::BackupKey::from_bytes([0x22u8; 32])];
        let sync = build_sync(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            &retired,
        );
        assert_eq!(sync.predecessor_count(), 1);
    }

    #[test]
    fn an_identity_that_never_succeeded_offers_nothing() {
        let sync = build_sync(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            &[],
        );
        assert_eq!(sync.predecessor_count(), 0);
    }
}
