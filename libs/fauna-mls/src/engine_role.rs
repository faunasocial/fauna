//! The **conversations-engine role's web leg** — the Web Lock that stands where
//! the native role lock (`crate::storage::role_lock_path`, taken inside
//! `SqliteStorage::open`) stands: one MLS-writing engine per (origin, account),
//! taken in the shared engine-construction path before any engine exists
//! (`account-runtime.md` § Multi-instance concurrency → *The
//! conversations-engine role*, and → *Election mechanics*, the 2026-10-01
//! ruling on web's lock names).
//!
//! **Why web needs it although each tab has its own engine.** Tabs are separate
//! JS realms, so nothing contends for an engine *object*; what contends is
//! stored state every tab of one origin shares — one device id per account
//! (persisted to `localStorage`), hence one MLS device leaf, and one
//! account-scoped `provider` replica every tab CAS-puts on the nest. Two engines
//! advancing one leaf's ratchet is the fork the device-owned-epoch invariant
//! forbids, and the replica's three-way merge resolves a true conflict
//! last-writer-wins, which is lossy for ratchet state.
//!
//! **One name, derived here** — [`web_role_lock_name`], in the scheme every web
//! lock follows (`fauna.<owner>.<role>/<key>`), keyed by the actor because web
//! has one device leaf per (origin, account) and no per-app `mls_state.db` to
//! key on. It is deliberately NOT the account store's engine-singleton role
//! (`fauna.account-store.engine/<store name>`): two sections, two names, one tab
//! holding both.
//!
//! **Two postures, opposite on purpose — do not unify them:**
//!
//! - **The role fails CLOSED** ([`take_conversations_engine_role`]). Native:
//!   a lock-file I/O failure refuses the engine, because this state is class 5
//!   (user-irrecoverable) and degrading open would trade an availability corner
//!   for a silent ratchet fork. On web the same trade decides the
//!   missing-lock-manager case (no secure context, some embeddings): a tab that
//!   cannot prove it is alone runs no engine, and says so. It is a **try**,
//!   never a queue — a queued acquire would leave the second tab awaiting a
//!   lock the first releases only when it closes, which is a hang, not a wait.
//! - **The erase's probe degrades OPEN**
//!   ([`conversations_engine_role_held_elsewhere`], the question a sign-out or a
//!   remove-account asks before it erases: `apps/account-scoping.md`
//!   § Concurrent instances → *Web owes the same refusal through the lock it
//!   already has*). There the worst case of a wrong "free" is the pre-ruling
//!   sign-out, while a wrong "held" is a browser its owner cannot sign out of;
//!   and without a lock manager no tab can hold the role at all, so nobody is
//!   serving. `ifAvailable`, never a wait; a granted probe is dropped at once.
//!
//! **The tab's holds live here, not on an engine.** A wasm object is freed only
//! by an explicit `free()` (or a non-deterministic finalizer), and the SPA hands
//! its manager to readers that outlive a teardown, so a hold owned by the
//! manager would outlive the account switch and lock the next tab out. So the
//! role is a per-tab registry keyed by its name: taken once, re-used by a later
//! build in the same tab (an abandoned build's replacement), released by the
//! tab's one teardown ([`release_conversations_engine_role`] /
//! [`release_all_conversations_engine_roles`]) together with the runtime it
//! hosts — that pairing is what lets the erase's one probe suffice — and by the
//! browser for free when the tab closes. **The own hold is consulted, never
//! probed**: an `ifAvailable` request beside this tab's own hold would meet its
//! reflection and refuse every sign-out in the elected tab (the case native
//! closes by putting its own lock down for the probe); holding the role *is*
//! the proof no other tab does.

/// The conversations-engine role's Web Locks name for the account
/// `actor_id_hex` — the web keying of the role `crate::storage::role_lock_path`
/// keys natively (module docs). Normalized (trimmed, lowercase hex) so every
/// spelling of one actor keys one lock.
pub fn web_role_lock_name(actor_id_hex: &str) -> String {
    format!(
        "fauna.mls.conversations-engine/{}",
        actor_id_hex.trim().to_ascii_lowercase()
    )
}

#[cfg(all(target_arch = "wasm32", feature = "js"))]
pub use web::*;

#[cfg(all(target_arch = "wasm32", feature = "js"))]
mod web {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use fauna_core::web_lock::{WebLockAsk, WebLockHold};

    use super::web_role_lock_name;
    use crate::error::MlsError;

    thread_local! {
        /// This tab's held conversations-engine roles, by lock name (module
        /// docs: the holds live here, not on an engine).
        static HELD: RefCell<HashMap<String, WebLockHold>> = RefCell::new(HashMap::new());
    }

    /// Why [`take_conversations_engine_role`] refused — both fail CLOSED.
    #[derive(Debug)]
    pub enum WebRoleRefusal {
        /// Another tab of this browser profile holds the role — the native
        /// leg's `StateServedElsewhere`.
        ServedElsewhere,
        /// The role could not be asked for (no lock manager, or the browser
        /// refused the request): solitude cannot be proven, so no engine.
        Degraded(std::io::Error),
    }

    impl std::fmt::Display for WebRoleRefusal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::ServedElsewhere => write!(f, "{}", MlsError::ServedElsewhere),
                Self::Degraded(e) => write!(
                    f,
                    "the conversations-engine role could not be asked for ({e}) — refusing \
                     to run an unguarded MLS engine, this state is user-irrecoverable"
                ),
            }
        }
    }

    impl std::error::Error for WebRoleRefusal {}

    /// Take `actor_id_hex`'s conversations-engine role for this tab, or refuse
    /// (fail CLOSED, never a wait). A role this tab already holds answers `Ok`
    /// without a second request — a Web Lock does not re-enter, and a later
    /// build in the same tab (an abandoned build's replacement) serves under the
    /// hold the tab already has.
    pub async fn take_conversations_engine_role(actor_id_hex: &str) -> Result<(), WebRoleRefusal> {
        let name = web_role_lock_name(actor_id_hex);
        if HELD.with(|held| held.borrow().contains_key(&name)) {
            return Ok(());
        }
        match WebLockHold::try_take(&name).await {
            WebLockAsk::Held(hold) => {
                // A concurrent take in this tab may have landed while this one
                // awaited; keep the first hold, and let this one drop.
                HELD.with(|held| {
                    held.borrow_mut().entry(name).or_insert(hold);
                });
                Ok(())
            }
            // Refused by this tab's own hold, taken by a concurrent build while
            // this one awaited, is not another tab serving the account.
            WebLockAsk::Refused if holds_conversations_engine_role_here(actor_id_hex) => Ok(()),
            WebLockAsk::Refused => Err(WebRoleRefusal::ServedElsewhere),
            WebLockAsk::Degraded(e) => Err(WebRoleRefusal::Degraded(e)),
        }
    }

    /// Whether this tab holds `actor_id_hex`'s conversations-engine role.
    pub fn holds_conversations_engine_role_here(actor_id_hex: &str) -> bool {
        let name = web_role_lock_name(actor_id_hex);
        HELD.with(|held| held.borrow().contains_key(&name))
    }

    /// Hand `actor_id_hex`'s role back, waiting until the lock manager has let
    /// it go. A no-op when this tab does not hold it.
    pub async fn release_conversations_engine_role(actor_id_hex: &str) {
        let name = web_role_lock_name(actor_id_hex);
        let hold = HELD.with(|held| held.borrow_mut().remove(&name));
        if let Some(hold) = hold {
            hold.release().await;
        }
    }

    /// Hand back every conversations-engine role this tab holds — the tab's one
    /// teardown, which cannot know whether a build still in flight has taken
    /// one yet.
    pub async fn release_all_conversations_engine_roles() {
        let holds: Vec<WebLockHold> =
            HELD.with(|held| held.borrow_mut().drain().map(|(_, hold)| hold).collect());
        for hold in holds {
            hold.release().await;
        }
    }

    /// Whether ANOTHER tab of this browser profile holds `actor_id_hex`'s role
    /// right now — the erase's question (module docs: own hold consulted, never
    /// probed; `ifAvailable`, never a wait; degrades OPEN).
    pub async fn conversations_engine_role_held_elsewhere(actor_id_hex: &str) -> bool {
        if holds_conversations_engine_role_here(actor_id_hex) {
            return false;
        }
        match WebLockHold::try_take(&web_role_lock_name(actor_id_hex)).await {
            // Let the probe go before answering, so the role is free again by
            // the time the caller (or a sibling's build) asks for it.
            WebLockAsk::Held(probe) => {
                probe.release().await;
                false
            }
            WebLockAsk::Refused => true,
            WebLockAsk::Degraded(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name's spelling — the one derivation, normalized like the per-tab
    /// pin, in the `fauna.<owner>.<role>/<key>` scheme.
    #[test]
    fn web_role_lock_name_is_one_normalized_derivation() {
        assert_eq!(
            web_role_lock_name("  AbC123 "),
            "fauna.mls.conversations-engine/abc123"
        );
        assert_eq!(web_role_lock_name("ABC123"), web_role_lock_name("abc123"));
    }
}
