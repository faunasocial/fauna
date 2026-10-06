//! [`AccessGate`] — the engine's **terminal `access-revoked` park state** for a
//! folder whose write grant the authoritative nest has refused
//! (*revocation is fail-closed
//! AND loud*).
//!
//! # Why a shared handle rather than a field on the engine
//!
//! A demotion surfaces at **two** places, and only one of them is inside the
//! engine:
//!
//! - the control plane — `fauna.sync.changes.record` (same-nest) or its
//!   `nest_url`-routed cross-nest relay — which [`crate::SyncEngine`] calls
//!   directly; and
//! - the **byte plane** — [`crate::write_token_bearer::WriteTokenBearer`] minting
//!   via `fauna.folders.write_token.get`, which is constructed *before* the
//!   engine (it is the engine's `SyncClient`'s bearer) and holds no engine
//!   reference.
//!
//! So the gate is an `Arc` both sides hold: whichever seam meets the refusal
//! first flips it, and the engine reads it as its own terminal state. That is
//! also why the mint is worth wiring at all rather than waiting for the record —
//! a demoted writer hits the mint on its *first upload byte*, well before it has
//! anything to record.
//!
//! # What "parked" means
//!
//! Terminal, not a backoff: once revoked, the engine performs no further remote
//! work for the set — no retry-forever loop, which is precisely the *silent*
//! un-sync `file-sync.md`'s iron rule forbids. It does **not** touch local
//! files or pending local edits: the user's bytes stay exactly where they are
//! and stay visible; the folder simply — and visibly, via the surfaced state —
//! stops being tracked. Recovery is a fresh bind gesture, which re-runs the
//! eager `write_token.get` verify (D3) and so fails just as loudly if the grant
//! is still gone.
//!
//! The flag is one-way on purpose. A gate that could un-revoke itself would let
//! a transient blip that *looked* like a refusal resurrect a parked engine
//! behind the user's back; re-binding is the explicit, verified path back.

use std::sync::Arc;

use tokio::sync::watch;

/// A one-way terminal flag: `false` (live) → `true` (parked, access revoked).
///
/// Cloned via [`Arc`] between the engine and its byte-plane bearer. Also
/// awaitable ([`Self::wait_revoked`]) so an engine host can react to the
/// transition — the agent uses it to persist the parked state and drop the set
/// from its running engine plan.
#[derive(Debug)]
pub struct AccessGate {
    tx: watch::Sender<bool>,
}

impl Default for AccessGate {
    fn default() -> Self {
        Self {
            tx: watch::channel(false).0,
        }
    }
}

impl AccessGate {
    /// A fresh, live gate.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Whether this set is parked — the authoritative nest has refused the
    /// write grant and no retry will change that.
    pub fn is_revoked(&self) -> bool {
        *self.tx.borrow()
    }

    /// Park the set. Idempotent; returns `true` only for the call that made the
    /// transition, so the caller can log/report exactly once rather than on
    /// every subsequent refusal in the same doomed cycle.
    pub fn revoke(&self) -> bool {
        // `send_if_modified` reports whether the closure changed the value, and
        // notifies watchers only then — the once-only semantics and the
        // single-wakeup guarantee come from the same call.
        self.tx.send_if_modified(|revoked| {
            if *revoked {
                false
            } else {
                *revoked = true;
                true
            }
        })
    }

    /// Resolve once this gate is (or already was) revoked.
    ///
    /// Returns immediately on an already-parked gate, so a host that subscribes
    /// late cannot miss the transition. Never resolves for a gate that stays
    /// live — callers `select!` it against their cancellation token.
    pub async fn wait_revoked(&self) {
        let mut rx = self.tx.subscribe();
        // Check the current value first: `changed()` only reports values sent
        // *after* subscribing, so a gate revoked before this call would
        // otherwise hang forever.
        if *rx.borrow_and_update() {
            return;
        }
        // The sender lives as long as this `AccessGate`, and `&self` is borrowed
        // for the whole future, so `changed()` cannot fail here.
        while rx.changed().await.is_ok() {
            if *rx.borrow_and_update() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_live_and_revokes_once() {
        let gate = AccessGate::new();
        assert!(!gate.is_revoked(), "a fresh gate is live");
        assert!(gate.revoke(), "the first revoke makes the transition");
        assert!(gate.is_revoked());
        assert!(
            !gate.revoke(),
            "a second revoke is a no-op — the host reports the park exactly once"
        );
        assert!(gate.is_revoked(), "and it stays parked (one-way)");
    }

    /// The late-subscriber case: an engine host that starts watching *after* the
    /// refusal already landed must still observe the park, or the agent would
    /// never persist it and the folder would resume syncing on the next
    /// reconcile — a silent un-park.
    #[tokio::test]
    async fn wait_revoked_resolves_for_an_already_parked_gate() {
        let gate = AccessGate::new();
        gate.revoke();
        tokio::time::timeout(std::time::Duration::from_secs(5), gate.wait_revoked())
            .await
            .expect("an already-revoked gate resolves immediately");
    }

    #[tokio::test]
    async fn wait_revoked_wakes_on_the_transition() {
        let gate = AccessGate::new();
        let watcher = Arc::clone(&gate);
        let handle = tokio::spawn(async move { watcher.wait_revoked().await });
        // Not yet revoked: the watcher must still be pending.
        tokio::task::yield_now().await;
        assert!(!handle.is_finished(), "no wakeup before the refusal");
        gate.revoke();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("the watcher wakes on revoke")
            .expect("watcher task did not panic");
    }
}
