//! The conversations oracle's door, hosted by the FFI conversations session
//! (`conversations-oracle.md` § The custodian and the door) — the macOS and
//! windows apps' seat of the one assembly tui and linux already call,
//! `fauna_client_account_runtime::conversations_door::spawn_conversations_door`.
//!
//! The assembly needs a tokio runtime to keep the door on, and the session
//! factory that holds every input is a synchronous UniFFI export with none —
//! `fauna-ffi` owns no fallback runtime on purpose (`crate::account_runtime`).
//! So the factory STAGES the door here, and the door starts on the runtime the
//! account store's install recorded: at the store edge when the session came
//! first, at the factory's end when the store did. Nothing the door serves
//! works before the store is open anyway (its meta rows read "not open yet").
//!
//! One door per session: a rebuilt session replaces the staged door (the old
//! one is dropped, so the endpoint is given up and the new keeper takes it on
//! its next tick), and `release_account_scoped_stores` drops it at sign-out.
//! The seat is process-wide like the store runtime it starts on: an FFI app
//! holds one signed-in account's engine at a time.

use std::any::Any;
use std::sync::{Arc, Mutex, Weak};

/// What a started door is held as: dropping it gives the door up.
pub(crate) type HeldDoor = Box<dyn Any + Send>;

/// Starts a staged door on a runtime; `None` when this device has no endpoint
/// for the account (the assembly logs why).
pub(crate) type StartDoor = Box<dyn FnOnce(&tokio::runtime::Handle) -> Option<HeldDoor> + Send>;

enum Door {
    Staged(StartDoor),
    /// Started — `None` when the start found no endpoint, which is not retried
    /// until the next session.
    Started {
        _held: Option<HeldDoor>,
    },
}

struct Seated<K> {
    session: Weak<K>,
    door: Door,
}

/// The door of the session `K` this seat serves, staged or started.
pub(crate) struct DoorSeat<K>(Mutex<Option<Seated<K>>>);

impl<K> DoorSeat<K> {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn seat(&self) -> std::sync::MutexGuard<'_, Option<Seated<K>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The factory edge: `session` holds the account's engine now. Replaces
    /// whatever door the seat held, dropped outside the lock.
    #[cfg_attr(
        not(all(any(target_os = "macos", windows), not(test))),
        allow(dead_code)
    )]
    pub(crate) fn stage(&self, session: &Arc<K>, start: StartDoor) {
        let previous = self.seat().replace(Seated {
            session: Arc::downgrade(session),
            door: Door::Staged(start),
        });
        drop(previous);
    }

    /// The runtime edge: start a staged door on `rt`. A no-op for a door
    /// already started, or a seat holding none — so both edges may call it.
    pub(crate) fn start(&self, rt: &tokio::runtime::Handle) {
        let mut seat = self.seat();
        let Some(seated) = seat.as_mut() else {
            return;
        };
        if matches!(seated.door, Door::Started { .. }) {
            return;
        }
        let Door::Staged(start) =
            std::mem::replace(&mut seated.door, Door::Started { _held: None })
        else {
            unreachable!("checked just above");
        };
        seated.door = Door::Started { _held: start(rt) };
    }

    /// The sign-out edge: drop the door if `session` is the one it serves. A
    /// stale session's release leaves its successor's door standing.
    pub(crate) fn release(&self, session: &Arc<K>) {
        let mut seat = self.seat();
        let ours = seat
            .as_ref()
            .is_some_and(|seated| std::ptr::eq(seated.session.as_ptr(), Arc::as_ptr(session)));
        let taken = if ours { seat.take() } else { None };
        drop(seat);
        drop(taken);
    }
}

/// The process's seat.
pub(crate) static SEAT: DoorSeat<fauna_conversations::ConversationsSession> = DoorSeat::new();

/// Stage the door of the session the factory just built, from the inputs it
/// already holds. Under the in-crate tests nothing is staged: they would serve
/// a real endpoint in this user's agent dir (tui gates its spawn the same way).
#[cfg(all(any(target_os = "macos", windows), not(test)))]
pub(crate) fn stage(
    session: &Arc<fauna_conversations::ConversationsSession>,
    inputs: fauna_client_account_runtime::conversations_door::ConversationsDoorInputs,
) {
    SEAT.stage(
        session,
        Box::new(move |rt| {
            fauna_client_account_runtime::conversations_door::spawn_conversations_door(inputs, rt)
                .map(|door| Box::new(door) as HeldDoor)
        }),
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A door whose start and drop are counted.
    struct Counted(Arc<AtomicUsize>);

    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting_start(starts: &Arc<AtomicUsize>, drops: &Arc<AtomicUsize>) -> StartDoor {
        let starts = Arc::clone(starts);
        let drops = Arc::clone(drops);
        Box::new(move |_rt| {
            starts.fetch_add(1, Ordering::SeqCst);
            Some(Box::new(Counted(drops)) as HeldDoor)
        })
    }

    fn counters() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
        (Arc::default(), Arc::default())
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
    }

    /// A door staged by the factory starts only once a runtime arrives, and
    /// exactly once however many edges call — the store edge and the factory's
    /// end both do.
    #[test]
    fn a_staged_door_starts_once_on_the_first_runtime() {
        let seat = DoorSeat::<()>::new();
        let session = Arc::new(());
        let (starts, drops) = counters();
        seat.stage(&session, counting_start(&starts, &drops));
        assert_eq!(starts.load(Ordering::SeqCst), 0, "no runtime, no door");

        let rt = runtime();
        seat.start(rt.handle());
        seat.start(rt.handle());
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "the door is held");
    }

    /// A rebuilt session replaces the door: the old one is given up, so the
    /// new session's keeper can take the endpoint.
    #[test]
    fn a_rebuilt_session_gives_the_previous_door_up() {
        let seat = DoorSeat::<()>::new();
        let rt = runtime();
        let first = Arc::new(());
        let (starts, drops) = counters();
        seat.stage(&first, counting_start(&starts, &drops));
        seat.start(rt.handle());

        let second = Arc::new(());
        let (second_starts, second_drops) = counters();
        seat.stage(&second, counting_start(&second_starts, &second_drops));
        assert_eq!(drops.load(Ordering::SeqCst), 1, "the first door is dropped");
        seat.start(rt.handle());
        assert_eq!(second_starts.load(Ordering::SeqCst), 1);
        assert_eq!(second_drops.load(Ordering::SeqCst), 0);
    }

    /// Sign-out drops the door of the session it releases — and only that
    /// session's: a stale release must not close its successor's door.
    #[test]
    fn sign_out_drops_only_the_released_sessions_door() {
        let seat = DoorSeat::<()>::new();
        let rt = runtime();
        let stale = Arc::new(());
        let current = Arc::new(());
        let (starts, drops) = counters();
        seat.stage(&current, counting_start(&starts, &drops));
        seat.start(rt.handle());

        seat.release(&stale);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "a stale release keeps it");
        seat.release(&current);
        assert_eq!(drops.load(Ordering::SeqCst), 1, "sign-out gives it up");

        seat.start(rt.handle());
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "a released seat starts nothing"
        );
    }

    /// A door released before any runtime arrived never starts.
    #[test]
    fn a_door_released_before_its_runtime_never_starts() {
        let seat = DoorSeat::<()>::new();
        let session = Arc::new(());
        let (starts, drops) = counters();
        seat.stage(&session, counting_start(&starts, &drops));
        seat.release(&session);
        seat.start(runtime().handle());
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }
}
