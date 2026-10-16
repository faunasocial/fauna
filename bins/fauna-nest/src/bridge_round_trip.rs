//! The nest→bridge round trip — one waiter registry for every call the nest
//! starts and a bridge answers.
//!
//! `transport.md` § Request lifecycle rejects a `Reply` from any client
//! connection, so the nest cannot send a bridge a `Request` frame. Every
//! nest→bridge call therefore takes one shape: the nest pushes a request
//! carrying a random `request_id`, and the bridge answers over a BRIDGE-class
//! kind naming that id. This module is the correlation half: the waiter is
//! registered under the id **before** the push goes out, the deliver kind
//! hands the answer to it, and the caller's deadline, its own drop, or the
//! oldest-first shed removes it.
//!
//! Users: the permission-set call (`/oauth/par` resolving an `include:`
//! through the PDS bridge's chain — [`crate::oauth_as_permission_sets`]) and
//! the oracle's `atproto.service_auth` class (the PDS bridge minting with the
//! repo signing key only it holds — [`crate::atproto_oracle`];
//! `key-material-hierarchy.md` § The oracle → *The bridge-held custodian*).
//!
//! **Every failure is the caller's one closed-world refusal.** A shed waiter
//! sees its sender dropped (`RecvError`), a late answer matches nothing
//! (`deliver` → `false`), and neither is ever turned into a guess.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

/// Bytes of entropy in a `request_id`. The id names an in-memory waiter for at
/// most one deadline; 128 bits is what stops another party's answer landing
/// on it by guess, and no more is needed.
pub const REQUEST_ID_LEN: usize = 16;

/// How many round trips of one kind may be in flight at once before the
/// oldest is shed.
///
/// Bounded like every other store the nest keeps for a caller-driven flow
/// (`ParStore`, `ConsentWakes`): an unbounded map keyed by requests a remote
/// party drives is a memory lever. Sized far above what an honest population
/// keeps in flight within one deadline, so only a flood sheds — its own
/// oldest — and a shed waiter is **refused**, never guessed.
pub const ROUND_TRIP_CAPACITY: usize = 1024;

struct Waiter<D> {
    sender: oneshot::Sender<D>,
    /// Monotonic open order, for oldest-first shedding.
    opened: u64,
}

struct Inner<D> {
    waiters: HashMap<[u8; REQUEST_ID_LEN], Waiter<D>>,
    next_open: u64,
}

/// The in-flight round trips of one call: `request_id` → the caller waiting
/// on the bridge's delivery `D`.
pub struct RoundTrips<D> {
    /// Names the call in the shed log line.
    name: &'static str,
    inner: Mutex<Inner<D>>,
}

impl<D> RoundTrips<D> {
    /// An empty registry for the call `name` (a log label).
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            inner: Mutex::new(Inner {
                waiters: HashMap::new(),
                next_open: 0,
            }),
        }
    }

    /// Mint a request id and register the waiter that will receive its answer.
    ///
    /// Registered **before** the push is sent, so an answer that arrives
    /// faster than the caller can await it is not lost — the `oneshot` holds
    /// it. At [`ROUND_TRIP_CAPACITY`] the oldest waiter is shed: its sender
    /// drops and its receiver sees `RecvError`, which every caller refuses.
    pub fn open(&self) -> ([u8; REQUEST_ID_LEN], oneshot::Receiver<D>) {
        use rand::RngCore as _;

        let mut inner = self.inner.lock().expect("round trips poisoned");
        if inner.waiters.len() >= ROUND_TRIP_CAPACITY {
            let oldest = inner
                .waiters
                .iter()
                .min_by_key(|(_, w)| w.opened)
                .map(|(id, _)| *id);
            if let Some(id) = oldest {
                inner.waiters.remove(&id);
                tracing::warn!(
                    target: "bridge_round_trip",
                    call = self.name,
                    "round trips at capacity; shed the oldest waiter (it is refused, never guessed)"
                );
            }
        }
        let mut id = [0u8; REQUEST_ID_LEN];
        loop {
            rand::rngs::OsRng.fill_bytes(&mut id);
            if !inner.waiters.contains_key(&id) {
                break;
            }
        }
        let (sender, receiver) = oneshot::channel();
        let opened = inner.next_open;
        inner.next_open += 1;
        inner.waiters.insert(id, Waiter { sender, opened });
        (id, receiver)
    }

    /// Hand the bridge's answer to the waiting caller. `false` when nothing
    /// waits under that id — the deadline already answered, the id was never
    /// minted, or the waiter was shed — which is a log line for the bridge and
    /// never an error: a late answer is ordinary.
    pub fn deliver(&self, request_id: &[u8], delivery: D) -> bool {
        let Ok(id) = <[u8; REQUEST_ID_LEN]>::try_from(request_id) else {
            return false;
        };
        let waiter = self
            .inner
            .lock()
            .expect("round trips poisoned")
            .waiters
            .remove(&id);
        match waiter {
            // A receiver that has already gone away (the caller's future was
            // dropped) makes the send fail; that is the same "nobody waits"
            // answer, not a fault.
            Some(w) => w.sender.send(delivery).is_ok(),
            None => false,
        }
    }

    /// Drop a waiter the caller has stopped waiting on.
    pub fn forget(&self, request_id: &[u8; REQUEST_ID_LEN]) {
        self.inner
            .lock()
            .expect("round trips poisoned")
            .waiters
            .remove(request_id);
    }

    /// How many waiters are registered — for tests witnessing that a refusal
    /// leaves nothing behind.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.inner
            .lock()
            .expect("round trips poisoned")
            .waiters
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_delivery_before_the_await_is_not_lost() {
        let registry = RoundTrips::<Option<Vec<u8>>>::new("test");
        let (id, receiver) = registry.open();
        assert!(registry.deliver(&id, Some(b"doc".to_vec())));
        assert_eq!(receiver.await.unwrap(), Some(b"doc".to_vec()));
        assert_eq!(registry.pending(), 0);
    }

    #[test]
    fn an_unknown_or_malformed_id_is_not_accepted() {
        let registry = RoundTrips::<Option<Vec<u8>>>::new("test");
        let (id, _receiver) = registry.open();
        assert!(!registry.deliver(&[0u8; REQUEST_ID_LEN], Some(vec![])));
        assert!(!registry.deliver(&id[..5], Some(vec![])));
        assert!(registry.deliver(&id, None), "a refusal is a delivery too");
        assert!(
            !registry.deliver(&id, None),
            "a second answer matches nothing"
        );
    }

    #[tokio::test]
    async fn at_capacity_the_oldest_waiter_is_shed_and_refused() {
        let registry = RoundTrips::<Option<String>>::new("test");
        let (_, oldest) = registry.open();
        let mut later = Vec::new();
        for _ in 1..ROUND_TRIP_CAPACITY {
            later.push(registry.open());
        }
        assert_eq!(registry.pending(), ROUND_TRIP_CAPACITY);
        let (_, newest) = registry.open();
        assert_eq!(registry.pending(), ROUND_TRIP_CAPACITY);
        assert!(
            oldest.await.is_err(),
            "the shed waiter must be refused, not left hanging"
        );
        drop(newest);
        drop(later);
    }

    #[test]
    fn a_forgotten_waiter_matches_no_later_answer() {
        let registry = RoundTrips::<Option<String>>::new("test");
        let (id, _receiver) = registry.open();
        registry.forget(&id);
        assert_eq!(registry.pending(), 0);
        assert!(!registry.deliver(&id, Some("late".into())));
    }
}
