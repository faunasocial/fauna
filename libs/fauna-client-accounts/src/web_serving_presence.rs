//! Web's **serving-presence lock** — the web twin of the native account serving
//! lock (*presence, not exclusion*): every tab whose in-memory identity is an
//! account holds `fauna.accounts.serving/<actor-id-hex>` **shared**, from the
//! moment its identity settles to the moment it changes, so an erase can see a
//! tab that holds an account's secret whether or not it runs that account's
//! engine (`docs/goal/architecture/apps/account-scoping.md` § Concurrent
//! instances → *Web's serving-presence lock*; the name's place in the scheme:
//! `account-runtime-roles.md` § Multi-instance concurrency → *Election
//! mechanics*).
//!
//! - **The take queues; the probe never does.** [`take_serving_presence`] is a
//!   queued shared request (`fauna_core::web_lock::WebLockHold::take_shared`):
//!   the only exclusive holder this name ever has is a probe, held for one
//!   round trip, and an `ifAvailable` take that met one would leave the tab
//!   undeclared for life. Without a lock manager the take is skipped and the tab
//!   serves undeclared.
//! - **The probe** ([`serving_presence_held_elsewhere`]) is a momentary
//!   exclusive `ifAvailable` try, released before it answers, and degrades
//!   OPEN. For an account **this** tab holds, the tab puts its own shared hold
//!   down for the probe and takes it again afterwards whatever the answer
//!   (native's `without_own_lock`) — nothing contends for a shared hold, so the
//!   gap costs nothing but the window the ruling leaves open on every seat.
//! - **The holds live here, per tab**, keyed by name, like the
//!   conversations-engine role's (`fauna_mls::engine_role`): a wasm object is
//!   freed only explicitly, so a hold owned by one would outlive the identity
//!   it declares. The browser releases them when the tab goes away.
//! - **Wanted, not just held.** A take can still be queued when the identity
//!   moves on; [`release_serving_presence`] withdraws the wish, and a grant that
//!   lands after it is dropped at once rather than declaring an account the tab
//!   no longer serves.
//!
//! The SPA's whole part is one subscription on its identity store calling the
//! take and the release; the erase's question is `fauna-wasm`'s
//! `actors_served_elsewhere`, which asks the role and then this lock.

/// The serving-presence lock's Web Locks name for `actor_id_hex`. Normalized
/// (trimmed, lowercase hex) like the role's name, so every spelling of one
/// actor keys one lock.
pub fn web_serving_lock_name(actor_id_hex: &str) -> String {
    format!(
        "fauna.accounts.serving/{}",
        actor_id_hex.trim().to_ascii_lowercase()
    )
}

#[cfg(all(feature = "web-localstorage", target_arch = "wasm32"))]
pub use web::*;

#[cfg(all(feature = "web-localstorage", target_arch = "wasm32"))]
mod web {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    use fauna_core::web_lock::{WebLockAsk, WebLockHold};

    use super::web_serving_lock_name;

    thread_local! {
        /// This tab's granted serving holds, by lock name.
        static HELD: RefCell<HashMap<String, WebLockHold>> = RefCell::new(HashMap::new());
        /// The names this tab means to hold — a take still queued included.
        static WANTED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    }

    /// Declare that this tab serves `actor_id_hex`: take its serving lock
    /// shared, queued (module docs). Idempotent per account; resolves once the
    /// hold is granted, or at once when it cannot be asked for (skipped — the
    /// tab serves undeclared).
    pub async fn take_serving_presence(actor_id_hex: &str) {
        let name = web_serving_lock_name(actor_id_hex);
        if !WANTED.with(|w| w.borrow_mut().insert(name.clone())) {
            return;
        }
        acquire(name).await;
    }

    /// Withdraw this tab's declaration for `actor_id_hex` — or, with `None`,
    /// for every account — waiting until the lock manager has let each go. A
    /// take still queued is withdrawn too: its grant is dropped on arrival.
    pub async fn release_serving_presence(actor_id_hex: Option<&str>) {
        let holds: Vec<WebLockHold> = match actor_id_hex {
            Some(actor) => {
                let name = web_serving_lock_name(actor);
                WANTED.with(|w| w.borrow_mut().remove(&name));
                HELD.with(|h| h.borrow_mut().remove(&name))
                    .into_iter()
                    .collect()
            }
            None => {
                WANTED.with(|w| w.borrow_mut().clear());
                HELD.with(|h| h.borrow_mut().drain().map(|(_, hold)| hold).collect())
            }
        };
        for hold in holds {
            hold.release().await;
        }
    }

    /// Whether this tab holds `actor_id_hex`'s serving lock right now.
    pub fn holds_serving_presence_here(actor_id_hex: &str) -> bool {
        let name = web_serving_lock_name(actor_id_hex);
        HELD.with(|h| h.borrow().contains_key(&name))
    }

    /// Whether ANOTHER tab of this browser profile declares `actor_id_hex`
    /// served right now — the erase's second question (module docs: a
    /// momentary exclusive `ifAvailable` try, released before answering;
    /// degrades OPEN; this tab's own hold put down for the probe and retaken
    /// whatever the answer).
    pub async fn serving_presence_held_elsewhere(actor_id_hex: &str) -> bool {
        let name = web_serving_lock_name(actor_id_hex);
        let own = HELD.with(|h| h.borrow_mut().remove(&name));
        let had_own = own.is_some();
        if let Some(own) = own {
            own.release().await;
        }
        let served = match WebLockHold::try_take(&name).await {
            WebLockAsk::Held(probe) => {
                probe.release().await;
                false
            }
            WebLockAsk::Refused => true,
            WebLockAsk::Degraded(_) => false,
        };
        if had_own && WANTED.with(|w| w.borrow().contains(&name)) {
            acquire(name).await;
        }
        served
    }

    /// Take `name` shared and keep the hold while the tab still wants it.
    async fn acquire(name: String) {
        match WebLockHold::take_shared(&name).await {
            Ok(hold) => {
                if WANTED.with(|w| w.borrow().contains(&name)) {
                    // A concurrent acquire of the same name keeps the first
                    // hold; this one drops (a shared hold beside it).
                    HELD.with(|h| {
                        h.borrow_mut().entry(name).or_insert(hold);
                    });
                }
                // Otherwise the identity moved on while the take queued: the
                // hold drops here, released.
            }
            Err(e) => tracing::warn!(
                "[serving-presence] {name} could not be taken ({e}) — this tab serves undeclared"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_serving_lock_name_is_one_normalized_derivation() {
        assert_eq!(
            web_serving_lock_name("  AbC123 "),
            "fauna.accounts.serving/abc123"
        );
        assert_eq!(
            web_serving_lock_name("ABC123"),
            web_serving_lock_name("abc123")
        );
    }
}
