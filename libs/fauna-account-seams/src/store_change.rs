//! The **store-change notice** — the one watch every app's open store-backed
//! surface re-reads on (`docs/goal/architecture/account-runtime.md`
//! § Multi-instance concurrency → *A runtime's own pump is a source of
//! the notice too*, part 4). One implementation for every runtime-hosting app
//! (priority #2), in the crate every one of them compiles, web included.
//!
//! It joins the sources a runtime has:
//!
//! - **the change generation** — this runtime's own pump changed an entry a
//!   read can answer (`AccountStoreHandle::changed_after`); wakes at the end
//!   of the run that made the change;
//! - **the `data_version` floor** — another connection on the same store
//!   committed (a co-located process holding the pump, or one writing beside
//!   it); polled every [`STORE_CHANGE_POLL_INTERVAL`];
//! - **a poke**, where a host has one ([`StoreChangeWatch::with_poke`]): web's
//!   cross-tab channel, the floor's stand-in where IndexedDB has no counter.
//!
//! and answers them as one payload-free "the store may have changed". It is a
//! **level, not an event**: sources coalesce, a consumer re-reads what it
//! shows and paints only what differs, and a re-read never discards an edit
//! in progress — those halves are the consumer's. The watch ends when the
//! runtime does.
//!
//! Its consumers: the two conversation seams' watchers ([`crate::read_positions`],
//! [`crate::contact_overlays`]) and each app's open-page handler (tui's
//! `session::account_store_watch` first; web's through [`StoreChangeLevel`]).

use std::time::Duration;

use fauna_account_plane::account_driver::AccountStoreHandle;
use tokio::sync::{mpsc, oneshot, watch};

/// How often the watch reads the store's cross-connection change counter —
/// the notification floor (`account-data-plane.md` § Multi-instance
/// concurrency, T9 *poll-with-poke*: one cheap read; correctness never
/// depends on anything faster). This runtime's own pump wakes the watch at
/// once; the poll is only for what another connection commits.
pub const STORE_CHANGE_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// The joined store-change sources of one runtime (module docs). Seeded at
/// construction: assembly is not a change, so nothing that happened before
/// [`Self::new`] fires.
pub struct StoreChangeWatch {
    store: AccountStoreHandle,
    generation: u64,
    floor: Option<u64>,
    poke: Option<mpsc::UnboundedReceiver<()>>,
}

impl StoreChangeWatch {
    /// Seed the watch over `store`: its change generation and its floor
    /// reading now.
    pub async fn new(store: AccountStoreHandle) -> Self {
        let generation = store.change_generation();
        let mut floor = None;
        // A failed first read seeds nothing; the next good reading seeds.
        let _ = floor_moved(&mut floor, store.data_version().await.ok().flatten());
        Self {
            store,
            generation,
            floor,
            poke: None,
        }
    }

    /// Add a poke source — a host's own "another writer committed" signal
    /// (web's cross-tab channel). Each message is a "may have changed"; a
    /// burst coalesces into one, and a closed channel disarms the source.
    #[must_use]
    pub fn with_poke(mut self, poke: mpsc::UnboundedReceiver<()>) -> Self {
        self.poke = Some(poke);
        self
    }

    /// Wait until the store may have changed: `true` then, `false` once the
    /// runtime is gone (the floor's read errs — a sign-out's deterministic
    /// shutdown, or teardown), after which the consumer ends.
    pub async fn changed(&mut self) -> bool {
        let Self {
            store,
            generation,
            floor,
            poke,
        } = self;
        loop {
            // One pinned sleep per wait, re-armed after each poll that found
            // nothing — the `MissedTickBehavior::Delay` cadence, cross-target.
            tokio::select! {
                moved = store.changed_after(*generation) => {
                    *generation = moved;
                    return true;
                }
                poked = async {
                    match poke.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => match poked {
                    Some(()) => {
                        if let Some(rx) = poke.as_mut() {
                            while rx.try_recv().is_ok() {}
                        }
                        return true;
                    }
                    None => *poke = None,
                },
                () = fauna_sleep::sleep(STORE_CHANGE_POLL_INTERVAL) => match store.data_version().await {
                    Err(_) => return false,
                    Ok(reading) => {
                        if floor_moved(floor, reading) {
                            return true;
                        }
                    }
                },
            }
        }
    }
}

/// The watch as a **counted level** — its face for a host whose consumer
/// cannot hold the watch across its own waits (web: a JS caller re-arming a
/// Promise per notice). One relay drives the one [`StoreChangeWatch`] and
/// counts its notices; a consumer asks "past the count I last saw?" as often
/// as it likes, so a notice landing between two of its waits is never lost
/// and a burst reads as one.
///
/// The level ends — every waiter answers `None` — when the runtime is gone
/// (the watch ended) or when this value is dropped, whichever is first: the
/// host keeps it beside its runtime handle and drops both at its stop.
pub struct StoreChangeLevel {
    count: watch::Receiver<u32>,
    /// Dropped with the level: ends the relay at once rather than at the
    /// floor's next poll.
    _stop: oneshot::Sender<()>,
}

impl StoreChangeLevel {
    /// The level over `store` and its relay, which the host spawns (it ends
    /// by itself — see the type docs). The count starts at `0`, and the watch
    /// is seeded before this returns: a change after it is never missed,
    /// whenever the relay is first polled.
    pub async fn new(store: AccountStoreHandle) -> (Self, impl Future<Output = ()>) {
        let (tx, count) = watch::channel(0u32);
        let (stop, mut stopped) = oneshot::channel::<()>();
        let mut watch = StoreChangeWatch::new(store).await;
        let relay = async move {
            let notices = async {
                while watch.changed().await {
                    tx.send_modify(|n| *n = n.wrapping_add(1));
                }
            };
            tokio::select! {
                () = notices => {}
                _ = &mut stopped => {}
            }
        };
        (Self { count, _stop: stop }, relay)
    }

    /// Wait until the count differs from `seen`: the count then, or `None`
    /// once the level has ended. The future owns its reading, so it outlives
    /// the borrow — and a level dropped while it waits ends it.
    pub fn changed_after(&self, seen: u32) -> impl Future<Output = Option<u32>> + use<> {
        let mut count = self.count.clone();
        async move { count.wait_for(|n| *n != seen).await.ok().map(|n| *n) }
    }
}

/// The floor's pure change-detect: does `reading` mean "another connection
/// committed since the seed"?
///
/// The first `Some` reading **seeds silently** — assembly is not a change,
/// and firing on it would refresh every page once per login for nothing. A
/// `None` reading (a medium with no counter, or a transient refusal) neither
/// fires nor clears the seed — "cannot tell right now" must never read as
/// "changed".
fn floor_moved(last: &mut Option<u64>, reading: Option<u64>) -> bool {
    match (last.as_ref(), reading) {
        (_, None) => false,
        (None, Some(v)) => {
            *last = Some(v);
            false
        }
        (Some(prev), Some(v)) if *prev == v => false,
        (_, Some(v)) => {
            *last = Some(v);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::floor_moved;

    #[test]
    fn the_floor_fires_only_on_a_real_move() {
        let mut last = None;
        assert!(!floor_moved(&mut last, None), "no reading is not a change");
        assert!(
            !floor_moved(&mut last, Some(7)),
            "the first reading seeds silently"
        );
        assert!(
            !floor_moved(&mut last, Some(7)),
            "an unmoved counter is quiet"
        );
        assert!(floor_moved(&mut last, Some(8)), "a move fires");
        assert!(!floor_moved(&mut last, Some(8)), "and only once");
        assert!(
            !floor_moved(&mut last, None),
            "a None mid-stream neither fires nor clears the seed"
        );
        assert!(
            !floor_moved(&mut last, Some(8)),
            "the seed survived the None"
        );
        assert!(
            floor_moved(&mut last, Some(9)),
            "and the next real move still fires"
        );
    }
}
