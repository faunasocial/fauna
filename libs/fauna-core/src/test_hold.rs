//! Holds an e2e test arms to read a page WHILE a read is in flight, without a
//! clock — the shared primitives behind the client-side test holds:
//! [`OneShotHold`] (`fauna_feed::FeedManager::hold_next_reload_for_test`) and
//! [`GateHold`] (the folders page's mail-custody reads,
//! `fauna_client_folders::webdav_provision::hold_custody_reads_for_test`).
//!
//! Why a client-side hold rather than the nest's reply hold: an app's e2e agent
//! that awaits its page ops cannot tell a held reply from a slow nest and would
//! park on it, and a read that never reaches the nest (an account-store read)
//! cannot be held there at all. A hold the client owns is one the agent can see
//! ([`OneShotHold::is_armed`], [`GateHold::is_closed`]) before it decides
//! whether to await.
//!
//! Compiled unconditionally (no dependency, no state until armed); each
//! consumer gates its own arming surface behind its test cfg, so a production
//! build never arms one and its park returns at once.

use std::sync::Mutex;
use std::task::{Poll, Waker};

/// Arm, park the next reader that reaches the park point, release.
///
/// A waker-registering park, so it wakes under any executor (tokio natively,
/// the browser's on wasm). `const`-constructible, so a process-wide hold can be
/// a `static`.
#[derive(Default)]
pub struct OneShotHold {
    inner: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The next reader to reach the park point parks there.
    armed: bool,
    /// The parked reader may proceed.
    released: bool,
    waker: Option<Waker>,
}

impl OneShotHold {
    /// An unarmed hold.
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(State {
                armed: false,
                released: false,
                waker: None,
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Park the next reader that reaches [`Self::park_if_armed`].
    pub fn arm(&self) {
        let mut s = self.state();
        s.armed = true;
        s.released = false;
    }

    /// Release the parked reader, or disarm a hold no reader has reached yet.
    pub fn release(&self) {
        let mut s = self.state();
        s.armed = false;
        s.released = true;
        if let Some(waker) = s.waker.take() {
            waker.wake();
        }
    }

    /// Whether a hold is armed and no reader has reached it yet.
    pub fn is_armed(&self) -> bool {
        self.state().armed
    }

    /// Park until [`Self::release`] if armed, consuming the arm; return at once
    /// if not.
    pub async fn park_if_armed(&self) {
        {
            let mut s = self.state();
            if !std::mem::take(&mut s.armed) {
                return;
            }
            // A release that raced ahead of this reader belongs to an earlier
            // arm; this reader waits for its own.
            s.released = false;
        }
        std::future::poll_fn(|cx| {
            let mut s = self.state();
            if std::mem::take(&mut s.released) {
                Poll::Ready(())
            } else {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
}

/// A hold that, while closed, parks EVERY reader that reaches it, and opens for
/// all of them at once — for a read the app may issue again while the first is
/// parked (a store-change re-read), where a one-shot hold would let the second
/// read answer ahead of the release.
#[derive(Default)]
pub struct GateHold {
    inner: Mutex<GateState>,
}

#[derive(Default)]
struct GateState {
    closed: bool,
    /// Bumped on every open; a parked reader proceeds once it moves.
    opened: u64,
    wakers: Vec<Waker>,
}

impl GateHold {
    /// An open gate.
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(GateState {
                closed: false,
                opened: 0,
                wakers: Vec::new(),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Park every reader that reaches [`Self::park_while_closed`] from now on.
    pub fn close(&self) {
        self.state().closed = true;
    }

    /// Open the gate: every parked reader proceeds, and later ones pass.
    pub fn open(&self) {
        let wakers = {
            let mut s = self.state();
            s.closed = false;
            s.opened += 1;
            std::mem::take(&mut s.wakers)
        };
        wakers.into_iter().for_each(Waker::wake);
    }

    /// Whether the gate is closed.
    pub fn is_closed(&self) -> bool {
        self.state().closed
    }

    /// Park until [`Self::open`] if closed; return at once if open.
    pub async fn park_while_closed(&self) {
        let generation = {
            let s = self.state();
            if !s.closed {
                return;
            }
            s.opened
        };
        std::future::poll_fn(|cx| {
            let mut s = self.state();
            if s.opened != generation {
                Poll::Ready(())
            } else {
                s.wakers.push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::task::{Context, Wake};

    struct Flag(std::sync::atomic::AtomicBool);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn poll_once<F: std::future::Future>(
        f: std::pin::Pin<&mut F>,
        waker: &Waker,
    ) -> Poll<F::Output> {
        f.poll(&mut Context::from_waker(waker))
    }

    #[test]
    fn an_unarmed_hold_never_parks() {
        let hold = OneShotHold::new();
        let flag = Arc::new(Flag(Default::default()));
        let waker = Waker::from(flag);
        let mut fut = std::pin::pin!(hold.park_if_armed());
        assert!(poll_once(fut.as_mut(), &waker).is_ready());
    }

    #[test]
    fn an_armed_hold_parks_one_reader_until_released() {
        let hold = OneShotHold::new();
        hold.arm();
        assert!(hold.is_armed());
        let flag = Arc::new(Flag(Default::default()));
        let waker = Waker::from(Arc::clone(&flag));
        let mut fut = std::pin::pin!(hold.park_if_armed());
        assert!(poll_once(fut.as_mut(), &waker).is_pending());
        // The arm is consumed by the reader that parked.
        assert!(!hold.is_armed());
        hold.release();
        assert!(flag.0.load(std::sync::atomic::Ordering::SeqCst));
        assert!(poll_once(fut.as_mut(), &waker).is_ready());
        // One-shot: the next reader passes.
        let mut next = std::pin::pin!(hold.park_if_armed());
        assert!(poll_once(next.as_mut(), &waker).is_ready());
    }

    #[test]
    fn a_release_before_any_reader_disarms() {
        let hold = OneShotHold::new();
        hold.arm();
        hold.release();
        assert!(!hold.is_armed());
        let waker = Waker::from(Arc::new(Flag(Default::default())));
        let mut fut = std::pin::pin!(hold.park_if_armed());
        assert!(poll_once(fut.as_mut(), &waker).is_ready());
    }
    #[test]
    fn a_closed_gate_parks_every_reader_until_it_opens() {
        let gate = GateHold::new();
        let waker = Waker::from(Arc::new(Flag(Default::default())));
        let mut open_pass = std::pin::pin!(gate.park_while_closed());
        assert!(poll_once(open_pass.as_mut(), &waker).is_ready());
        gate.close();
        let mut first = std::pin::pin!(gate.park_while_closed());
        let mut second = std::pin::pin!(gate.park_while_closed());
        assert!(poll_once(first.as_mut(), &waker).is_pending());
        assert!(poll_once(second.as_mut(), &waker).is_pending());
        assert!(gate.is_closed());
        gate.open();
        assert!(poll_once(first.as_mut(), &waker).is_ready());
        assert!(poll_once(second.as_mut(), &waker).is_ready());
        assert!(!gate.is_closed());
    }
}
