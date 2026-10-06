//! tui leg of draft-persistence v2 for the **posts rail**
//! (`docs/goal/behavior/reserved-folders.md` § Drafts Sync;
//! `docs/goal/ui/feed.md` § Encryption at rest).
//!
//! Pure trigger glue over the shared core, and the first app leg on this rail:
//! the seal, the `fauna.drafts.{get,put}` calls, the launch gate and the
//! last-saved baseline all live in [`fauna_client_drafts::DraftsSync`], and the
//! posts-rail at-rest shape lives in [`fauna_feed::PostDrafts`] behind the
//! manager's own `drafts_snapshot_bytes` / `restore_drafts` pair. The exact
//! shape of [`crate::conversations::drafts`] one rail over — deliberately so:
//! a second mechanism here would be a per-app divergence in the one place
//! (priority #1) where the two rails are the same problem.
//!
//! * **Load on launch** — `DraftsSync::load()`, then `manager.restore_drafts()`
//!   so the composer reflects the post the user left half-written here or on
//!   another device.
//! * **Debounced autosave** — a [`FeedSnapshotObserver`] ticks on every manager
//!   change; after [`autosave_debounce`] of quiescence the coalesced burst is saved via
//!   `DraftsSync::save_if_changed(manager.drafts_snapshot_bytes())`.
//!
//! **The feed manager notifies far more often than the conversations one** —
//! every reload, resolved quote, link preview and score adjustment ticks the
//! same observer — and that is *fine* rather than something to filter here: a
//! non-compose tick costs one cheap byte-compare against the baseline and
//! stops, because the snapshot only carries user-authored compose fields
//! (`PostDrafts`). Filtering by "was it a compose change?" would need the
//! manager to classify its own notifications, which is exactly the per-app
//! cleverness the shared dedup exists to avoid.
//!
//! The debounce loop itself, the launch-restore glue and the `Weak`-liveness
//! check are shared with [`crate::conversations::drafts`] via
//! [`crate::drafts_autosave`] — this file keeps only what genuinely differs
//! per rail: the observer's foreign trait, the rail name, and this manager's
//! own [`DraftsHost`] impl.
//!
//! **Lifecycle.** The autosave task holds a [`Weak`] manager and its receiver's
//! sender lives in the observer the manager owns, so a re-login — which builds
//! a fresh `FeedState` and drops the old manager — closes the channel and ends
//! the task. Same `Weak`-liveness idiom as the conversations leg; neither leaks
//! a task onto a torn-down session's nest.
//!
//! Wired from the one post-auth hook (`session::establish`), beside
//! `crate::feed::init` — and, like the conversations leg, outside any
//! engine/MLS gate: drafts seal under the owner's `BackupKey`, so nothing else
//! failing may cost the user their unsent post.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_drafts::DraftsSync;
use fauna_core::identity::ActorKeypair;
use fauna_feed::FeedSnapshotObserver;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::CliFeedManager;
use crate::drafts_autosave::{self, DraftsHost};

impl DraftsHost for CliFeedManager {
    fn drafts_snapshot_bytes(&self) -> Vec<u8> {
        CliFeedManager::drafts_snapshot_bytes(self)
    }
    /// A feed manager serves one identity for its whole life — every app
    /// rebuilds it at an identity change rather than clearing it, so a late
    /// restore lands in the discarded instance — and its epoch never moves.
    fn identity_epoch(&self) -> u64 {
        0
    }
    async fn restore_drafts_at(&self, _epoch: u64, bytes: Vec<u8>) {
        CliFeedManager::restore_drafts(self, bytes)
    }
}

/// The feed composer's `__drafts` path (one combined blob, per
/// `reserved-folders.md` § Drafts Sync step 1). The enumeration is the
/// wire's, not this app's — a leg that minted its own rail name would
/// round-trip only with itself and silently lose every draft the user's other
/// devices wrote.
const RAIL: &str = fauna_protocol::drafts::RAIL_POSTS;

pub(crate) type FeedDraftsSync = DraftsSync<Arc<NestClient>>;

/// Forwards manager change notifications to the autosave debounce task.
///
/// `on_changed` fires synchronously on whatever thread mutated, so it does the
/// least possible work: a bare tick on an unbounded channel. Coalescing is safe
/// because the receiver re-reads a fresh snapshot per save — a dropped
/// duplicate loses nothing. Same contract as `TuiFeedObserver`.
struct DraftsAutosaveObserver {
    tx: UnboundedSender<()>,
}

impl FeedSnapshotObserver for DraftsAutosaveObserver {
    fn on_changed(&self) {
        // A closed channel means the autosave task retired (session rebuild).
        let _ = self.tx.send(());
    }
}

/// Wire feed-draft persistence for the just-authenticated actor: build the
/// shared [`DraftsSync`], restore the draft on launch, and attach the debounced
/// autosave.
///
/// A malformed secret is logged and skipped (drafts simply don't persist)
/// rather than failing login — the same non-fatal arm as the conversations leg.
///
/// Returns the built `DraftsSync`, so the caller can store it on
/// `app.feed.drafts_sync` for the leave-door flush (`drafts_autosave::flush_now`,
/// awaited directly in `main.rs`) — `None` iff the malformed-secret arm above
/// disabled persistence for this session.
pub fn start(
    manager: Arc<CliFeedManager>,
    nest: Arc<NestClient>,
    secret_hex: &str,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) -> Option<Arc<FeedDraftsSync>> {
    let keypair = match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("feed drafts: malformed identity secret, persistence disabled: {e}");
            return None;
        }
    };
    let sync = Arc::new(build_sync(nest, &keypair, succession_predecessors));
    drafts_autosave::restore_on_launch(Arc::clone(&manager), Arc::clone(&sync), "feed drafts");
    attach_autosave(&manager, Arc::clone(&sync));
    Some(sync)
}

/// Assemble this rail's [`DraftsSync`] — factored out of [`start`] **so the
/// call-site pin can call it**, exactly as `settings::devices::label_custody`
/// is factored for the label plane's twin.
///
/// ⚠ The retired keys are a READ fallback, never a seal root
/// ([`DraftsSync::with_predecessors`]). A successor's launch load beats the
/// post-auth re-seal pass more often than not, and without them it hard-errors,
/// the load gate never lifts, and the user's half-written post stays invisible for the
/// whole session. Resolved ONCE by the session hook and passed in.
fn build_sync(
    nest: Arc<NestClient>,
    keypair: &ActorKeypair,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) -> FeedDraftsSync {
    DraftsSync::new(nest, keypair, RAIL).with_predecessors(succession_predecessors.to_vec())
}

/// Attach the debounced autosave: observer → unbounded channel → the shared
/// [`drafts_autosave::run_autosave_loop`].
///
/// `save_if_changed` gates the pre-load window and dedups an unchanged
/// snapshot, so a tick raised by a non-compose manager change (a reload, a
/// resolved quote) costs only a cheap snapshot compare — no upload, and
/// crucially no empty PUT before the launch load has run.
fn attach_autosave(manager: &Arc<CliFeedManager>, sync: Arc<FeedDraftsSync>) {
    let (tx, rx) = unbounded_channel();
    manager.add_observer(Arc::new(DraftsAutosaveObserver { tx }));
    let manager = Arc::downgrade(manager);
    tokio::spawn(drafts_autosave::run_autosave_loop(
        rx,
        manager,
        sync,
        "feed drafts",
    ));
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use fauna_feed::FeedManager;

    /// Ceiling on any single await in these tests. Generous against
    /// [`autosave_debounce`] so it can only fire on a genuine hang, and virtual under
    /// `start_paused` — a green run pays nothing for it.
    ///
    /// **Load-bearing, not belt-and-braces** (the lesson the conversations leg
    /// paid for): Rust's test harness has no per-test timeout, and under a
    /// paused clock an await with no timer attached parks *forever* rather than
    /// failing, which once wedged a build for hours while looking like a slow
    /// compile.
    const TEST_AWAIT_CEILING: Duration = Duration::from_secs(60);

    async fn bounded<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
        match tokio::time::timeout(TEST_AWAIT_CEILING, fut).await {
            Ok(v) => v,
            Err(_) => panic!("timed out waiting for {what} — the autosave trigger is not firing"),
        }
    }

    /// A real manager over an offline transport — the established tui shape
    /// (`feed::tests::feed_app_with`, `search::tests::app_ready_to_navigate`).
    /// These tests never make a call: the compose mutators and the draft
    /// snapshot are pure state, so no nest has to answer.
    fn offline_manager() -> Arc<CliFeedManager> {
        Arc::new(FeedManager::new(
            crate::app::tests::test_session().client,
            [7u8; 32],
        ))
    }

    /// The rail path is the wire's enumeration, not a tui-local string. Same
    /// pin the conversations leg carries, for the same reason: a minted name
    /// round-trips only with itself and loses the user's other devices.
    #[test]
    fn rail_is_the_shared_posts_path() {
        assert_eq!(RAIL, "posts");
        assert!(
            fauna_protocol::drafts::is_ratified_rail(RAIL),
            "a rail the nest refuses would fail every save at runtime, not here",
        );
    }

    /// The autosave task must retire when its manager is dropped — otherwise a
    /// re-login leaks a task that keeps saving into the *previous* session's
    /// nest.
    #[test]
    fn snapshot_reports_gone_once_the_manager_is_dropped() {
        let manager = offline_manager();
        let weak = Arc::downgrade(&manager);
        assert!(weak.upgrade().map(|m| m.drafts_snapshot_bytes()).is_some());
        drop(manager);
        assert!(
            weak.upgrade().map(|m| m.drafts_snapshot_bytes()).is_none(),
            "the save path must report gone rather than resurrect a torn-down \
             session's manager",
        );
    }

    /// An edit burst must coalesce into ONE save, and the observer tick must be
    /// what drives it. Drives the real door: mutate through `update_compose`,
    /// the same call the composer's key handler makes, and read the canonical
    /// bytes back through a fresh manager — the relaunch journey.
    #[tokio::test(start_paused = true)]
    async fn a_burst_of_edits_coalesces_into_one_quiesced_snapshot() {
        let manager = offline_manager();
        let (tx, mut rx) = unbounded_channel();
        manager.add_observer(Arc::new(DraftsAutosaveObserver { tx }));

        // Three edits in one burst — each notifies, so more ticks are queued.
        manager.update_compose("a".into(), String::new(), None);
        manager.update_compose("ab".into(), String::new(), None);
        manager.update_compose("abc".into(), String::new(), None);

        assert!(
            bounded("the first edit's tick", rx.recv()).await.is_some(),
            "an edit must tick the autosave observer",
        );
        assert!(
            bounded("the burst to quiesce", drafts_autosave::quiesce(&mut rx)).await,
            "the burst must quiesce, not close",
        );

        // One save follows the burst, and it carries the LAST edit. A *value*
        // assertion, not a non-empty one: a stale draft, an empty composer and
        // a placeholder all satisfy "non-empty".
        let bytes = Arc::downgrade(&manager)
            .upgrade()
            .map(|m| m.drafts_snapshot_bytes())
            .expect("manager alive");
        let restored = offline_manager();
        restored.restore_drafts(bytes);
        assert_eq!(restored.snapshot().compose.text, "abc");
    }
}

#[cfg(test)]
mod succession_fallback_tests {
    use super::*;

    /// **The call-site pin for the `__drafts` read fallback on the posts rail.**
    ///
    /// `fauna-client-drafts`' own tests prove the fallback *works*; none of them
    /// can see THIS app stop passing the walk. That is the exact vacuity
    /// caught on the byte plane — a threading shipped with zero production
    /// writers while every unit test stayed green — so the app leg carries its
    /// own assertion, as `settings::devices` does for the label plane.
    ///
    /// Mutation: drop the `.with_predecessors(..)` in [`build_sync`] and this reds.
    #[test]
    fn the_posts_rail_offers_the_accounts_retired_roots() {
        let retired = vec![fauna_core::crypto::BackupKey::from_bytes([0x11u8; 32])];
        let sync = build_sync(
            NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate()),
            &ActorKeypair::generate(),
            &retired,
        );

        assert_eq!(
            sync.predecessor_count(),
            1,
            "after a succession this rail is still sealed under a predecessor's \
             root; without the offered key the launch load hard-errors and the \
             user's half-written post stays invisible for the whole session"
        );
    }

    /// The overwhelmingly common path — an identity that never succeeded — is
    /// unchanged, so a red above is about the threading and not about
    /// construction in general.
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
