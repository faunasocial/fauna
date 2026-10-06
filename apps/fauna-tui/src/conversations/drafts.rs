//! tui leg of draft-persistence v2 (`docs/goal/behavior/reserved-folders.md`
//! § Drafts Sync; `docs/goal/ui/conversations.md` § Persistence).
//!
//! Pure trigger glue over the shared core, the last of the 7 app legs: the
//! seal, the `fauna.drafts.{get,put}` calls, the launch gate and the last-saved
//! baseline all live in [`fauna_client_drafts::DraftsSync`], and the
//! conversations-rail `DraftStore` lives inside the shared
//! [`ConversationsManager`] — including the single-slot `new_thread` compose
//! (`store/drafts.rs`'s `DraftsSnapshot`), so tui has no interim in-memory draft
//! store to retire (same as linux; the per-app leg-step 4 of the rollout is N/A
//! here). This file wires only the two platform-specific triggers:
//!
//! * **Load on launch** — `DraftsSync::load()`, then `manager.restore_drafts_at()`
//!   with the identity epoch read before the load, so the composer reflects
//!   drafts the user left on this or another device.
//! * **Debounced autosave** — a [`SnapshotObserver`] ticks on every compose edit
//!   (`set_compose_body` / `set_compose_subject` / `set_new_thread_body` all
//!   `notify()`); after [`autosave_debounce`] of quiescence the coalesced burst is saved
//!   via `DraftsSync::save_if_changed(manager.drafts_snapshot_bytes())`.
//!
//! **Simpler than linux's leg by one whole mechanism.** linux must marshal the
//! observer tick onto the GTK main loop and hand-roll a generation counter over
//! `glib::timeout_add_local_once`, because its widget thread is not a tokio
//! context. The whole tui app is one tokio runtime, so the debounce is just a
//! `select!` between a sleep and the next tick — the timer *is* the generation,
//! and a later edit supersedes an earlier pending save by construction. Same
//! observable behavior, no counter to keep honest.
//!
//! The debounce loop itself, the launch-restore glue and the `Weak`-liveness
//! check are shared with [`crate::feed::drafts`] via [`crate::drafts_autosave`]
//! — this file keeps only what genuinely differs per rail: the observer's
//! foreign trait, the rail name, and this manager's own [`DraftsHost`] impl.
//!
//! **Lifecycle.** The autosave task holds a [`Weak`] manager and its receiver's
//! sender lives in the observer the manager owns, so a re-login — which builds a
//! fresh `ConversationsState` and drops the old manager — closes the channel and
//! ends the task. Two independent retirement paths, the same `Weak`-liveness
//! idiom the conversations receive loop uses (`conv_backend.rs`); neither leaks a
//! task onto a torn-down session's nest.
//!
//! Wired from the one post-auth hook (`session::establish`) beside
//! `conv_backend::start_conversations_session`, and deliberately **outside** its
//! MLS gate: drafts seal under the owner's `BackupKey`, so a failed MLS-engine
//! init must not also cost the user their unsent compose. Same placement as
//! linux's `app.rs` AuthSuccess handler.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_drafts::DraftsSync;
use fauna_conversations::ConversationsManager;
use fauna_conversations::observer::SnapshotObserver;
use fauna_core::identity::ActorKeypair;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::drafts_autosave::{self, DraftsHost};

impl DraftsHost for ConversationsManager {
    fn drafts_snapshot_bytes(&self) -> Vec<u8> {
        ConversationsManager::drafts_snapshot_bytes(self)
    }
    fn identity_epoch(&self) -> u64 {
        ConversationsManager::identity_epoch(self)
    }
    async fn restore_drafts_at(&self, epoch: u64, bytes: Vec<u8>) {
        ConversationsManager::restore_drafts_at(self, epoch, bytes).await
    }
}

/// The conversations compose rail's `__drafts` path (one combined blob, per
/// `reserved-folders.md` § Drafts Sync step 1). Feed / Events reuse the same
/// plane later under `"posts"` / `"events"` — the enumeration is the wire's, not
/// this app's, so never mint a tui-specific rail name.
const RAIL: &str = fauna_protocol::drafts::RAIL_CONVERSATIONS;

pub(crate) type ConvDraftsSync = DraftsSync<Arc<NestClient>>;

/// Forwards manager change notifications to the autosave debounce task.
///
/// `on_changed` fires synchronously on whatever thread mutated, so it does the
/// least possible work: a bare tick on an unbounded channel. Coalescing is safe
/// because the receiver re-reads a fresh snapshot per save — a dropped duplicate
/// loses nothing. Same contract as [`super::TuiConvObserver`].
struct DraftsAutosaveObserver {
    tx: UnboundedSender<()>,
}

impl SnapshotObserver for DraftsAutosaveObserver {
    fn on_changed(&self) {
        // A closed channel means the autosave task retired (session rebuild).
        let _ = self.tx.send(());
    }
}

/// Wire draft persistence for the just-authenticated actor: build the shared
/// [`DraftsSync`], restore drafts on launch, and attach the debounced autosave.
///
/// A malformed secret is logged and skipped (drafts simply don't persist) rather
/// than failing login — the same non-fatal arm as linux's leg and as
/// `conv_backend`'s engine init.
///
/// Returns the built `DraftsSync`, so the caller can store it on
/// `app.conversations.drafts_sync` for the leave-door flush
/// (`drafts_autosave::flush_now`, awaited directly in `main.rs`) — `None` iff
/// the malformed-secret arm above disabled persistence for this session.
pub fn start(
    manager: Arc<ConversationsManager>,
    nest: Arc<NestClient>,
    secret_hex: &str,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) -> Option<Arc<ConvDraftsSync>> {
    let keypair = match ActorKeypair::from_secret_hex(secret_hex) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("drafts: malformed identity secret, persistence disabled: {e}");
            return None;
        }
    };
    let sync = Arc::new(build_sync(nest, &keypair, succession_predecessors));
    drafts_autosave::restore_on_launch(Arc::clone(&manager), Arc::clone(&sync), "drafts");
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
/// the load gate never lifts, and the user's half-written reply stays invisible for the
/// whole session. Resolved ONCE by the session hook and passed in.
fn build_sync(
    nest: Arc<NestClient>,
    keypair: &ActorKeypair,
    succession_predecessors: &[fauna_client_drafts::BackupKey],
) -> ConvDraftsSync {
    DraftsSync::new(nest, keypair, RAIL).with_predecessors(succession_predecessors.to_vec())
}

/// Attach the debounced autosave: observer → unbounded channel → the shared
/// [`drafts_autosave::run_autosave_loop`].
///
/// `save_if_changed` gates the pre-load window and dedups an unchanged snapshot,
/// so a tick raised by a non-draft manager change (an arriving message, a thread
/// rename) costs only a cheap snapshot compare — no upload, and crucially no
/// empty PUT before the launch load has run.
fn attach_autosave(manager: &Arc<ConversationsManager>, sync: Arc<ConvDraftsSync>) {
    let (tx, rx) = unbounded_channel();
    manager.add_observer(Arc::new(DraftsAutosaveObserver { tx }));
    let manager = Arc::downgrade(manager);
    tokio::spawn(drafts_autosave::run_autosave_loop(
        rx, manager, sync, "drafts",
    ));
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Ceiling on any single await in these tests. Generous against [`autosave_debounce`]
    /// so it can only fire on a genuine hang, and virtual under
    /// `start_paused` — a green run pays nothing for it.
    ///
    /// **Not belt-and-braces; this is load-bearing.** Rust's test harness has no
    /// per-test timeout, and under a paused clock an await with no timer
    /// attached (a `recv()` that will never be woken) leaves the runtime with
    /// nothing to advance to, so it parks *forever* rather than failing. An
    /// earlier draft of the burst test below did exactly that and wedged a build
    /// for three and a half hours while looking like a slow compile. Every await
    /// here goes through [`bounded`] so a regression fails loudly in
    /// milliseconds instead.
    const TEST_AWAIT_CEILING: Duration = Duration::from_secs(60);

    /// Await `fut` under [`TEST_AWAIT_CEILING`], failing with `what` on expiry.
    async fn bounded<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
        match tokio::time::timeout(TEST_AWAIT_CEILING, fut).await {
            Ok(v) => v,
            Err(_) => panic!("timed out waiting for {what} — the autosave trigger is not firing"),
        }
    }

    /// The rail path is the wire's enumeration, not a tui-local string: a leg
    /// that minted its own name would round-trip only with itself and silently
    /// lose every draft written by the user's other devices
    /// (`reserved-folders.md` § Drafts Sync step 1).
    #[test]
    fn rail_is_the_shared_conversations_path() {
        assert_eq!(RAIL, "conversations");
    }

    /// **A launch load held open across an identity change fills nothing** —
    /// the shell half of `account-scoping.md` § The scoping taxonomy's rule that
    /// the writers of account-scoped state retire with the drop. tui is safe
    /// today because it builds a manager per actor; this pins the shared restore
    /// body so a manager kept across a switch stays safe too. Mutation: read the
    /// epoch inside the future after the load returns, and this reds.
    #[tokio::test]
    async fn a_load_the_outgoing_account_started_fills_nothing_after_the_switch() {
        let outgoing = ConversationsManager::new();
        outgoing.start_new_conversation();
        outgoing.set_new_thread_body("the outgoing account's draft".into());
        let blob = outgoing.drafts_snapshot_bytes();

        let manager = ConversationsManager::new();
        let (reply, load) = tokio::sync::oneshot::channel::<Vec<u8>>();
        let restore = drafts_autosave::restore_when_loaded(
            Arc::clone(&manager),
            async move { Ok::<_, String>(Some(load.await.expect("the reply"))) },
            "drafts",
        );
        let task = tokio::spawn(restore);

        manager.clear_for_identity_change();
        reply.send(blob.clone()).expect("the restore is waiting");
        bounded("the late restore", task).await.expect("no panic");

        manager.start_new_conversation();
        assert_eq!(
            manager
                .snapshot()
                .new_thread_compose
                .expect("the composer")
                .body_draft,
            "",
            "the outgoing account's reply filled the manager the incoming one uses"
        );

        // A load started after the switch still fills.
        let restore = drafts_autosave::restore_when_loaded(
            Arc::clone(&manager),
            async move { Ok::<_, String>(Some(blob)) },
            "drafts",
        );
        manager.cancel_new_conversation();
        bounded("the current restore", restore).await;
        manager.start_new_conversation();
        assert_eq!(
            manager
                .snapshot()
                .new_thread_compose
                .expect("the composer")
                .body_draft,
            "the outgoing account's draft"
        );
    }

    /// The autosave task must retire when its manager is dropped — otherwise a
    /// re-login leaks a task that keeps saving into the *previous* session's
    /// nest. Proves the `Weak` half directly: with no strong manager left, the
    /// snapshot helper reports gone instead of resurrecting it.
    #[test]
    fn snapshot_reports_gone_once_the_manager_is_dropped() {
        let manager = ConversationsManager::new();
        let weak = Arc::downgrade(&manager);
        assert!(weak.upgrade().map(|m| m.drafts_snapshot_bytes()).is_some());
        drop(manager);
        assert!(weak.upgrade().map(|m| m.drafts_snapshot_bytes()).is_none());
    }

    /// An edit burst must coalesce into ONE save, and the observer tick must be
    /// what drives it. Drives the real doors: mutate the manager through the
    /// same compose mutators the UI calls, and read the canonical bytes back.
    #[tokio::test(start_paused = true)]
    async fn a_burst_of_edits_coalesces_into_one_quiesced_snapshot() {
        let manager = ConversationsManager::new();
        let (tx, mut rx) = unbounded_channel();
        manager.add_observer(Arc::new(DraftsAutosaveObserver { tx }));

        // Open the composer FIRST — the real door the UI drives
        // (`new-conversation-button` → `start_new_conversation`). Not optional
        // scaffolding: `set_new_thread_body` is a deliberate no-op while no
        // new-thread draft is stashed (`manager.rs:649`), so a test that skipped
        // this would assert on a draft that was never created.
        manager.start_new_conversation();
        // Three edits in one burst — each notifies, so more ticks are queued.
        manager.set_new_thread_body("a".into());
        manager.set_new_thread_body("ab".into());
        manager.set_new_thread_body("abc".into());

        assert!(
            bounded("the composer-open tick", rx.recv()).await.is_some(),
            "opening the composer must tick",
        );
        assert!(
            bounded("the burst to quiesce", drafts_autosave::quiesce(&mut rx)).await,
            "the burst must quiesce, not close",
        );

        // One save follows the burst, and it carries the LAST edit. Replay the
        // real relaunch journey against a FRESH manager — restore, then re-open
        // the composer — because that is the order the app runs in and the only
        // order that surfaces the draft: `restore_drafts` refills the store, but
        // a snapshot exposes `new_thread_compose` only while the composer is the
        // active view (`manager.rs:168`), and `start_new_conversation` is
        // documented to PRESERVE a stashed draft rather than seed an empty one.
        let bytes = Arc::downgrade(&manager)
            .upgrade()
            .map(|m| m.drafts_snapshot_bytes())
            .expect("manager alive");
        let restored = ConversationsManager::new();
        restored.restore_drafts(bytes).await;
        restored.start_new_conversation();
        // A *value* assertion, not a non-empty one: a stale draft, an empty
        // composer and a placeholder all satisfy "non-empty", and this queue has
        // been bitten by that class three times.
        assert_eq!(
            restored
                .snapshot()
                .new_thread_compose
                .expect("the restored new-thread draft must survive re-opening the composer")
                .body_draft,
            "abc",
        );
    }
}

#[cfg(test)]
mod succession_fallback_tests {
    use super::*;

    /// **The call-site pin for the `__drafts` read fallback on the conversations rail.**
    ///
    /// `fauna-client-drafts`' own tests prove the fallback *works*; none of them
    /// can see THIS app stop passing the walk. That is the exact vacuity
    /// caught on the byte plane — a threading shipped with zero production
    /// writers while every unit test stayed green — so the app leg carries its
    /// own assertion, as `settings::devices` does for the label plane.
    ///
    /// Mutation: drop the `.with_predecessors(..)` in [`build_sync`] and this reds.
    #[test]
    fn the_conversations_rail_offers_the_accounts_retired_roots() {
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
             user's half-written reply stays invisible for the whole session"
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
